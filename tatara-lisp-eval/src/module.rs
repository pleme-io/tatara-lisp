//! Module system — file-as-module + qualified names + alias imports.
//!
//! Design rationale (researched, see commit history): file = module.
//! No explicit `(namespace foo)` declaration; the file's path IS the
//! module's identifier. Exports are explicit via `(provide ...)`;
//! imports through `(require "path" :as alias)` or `(require "path"
//! :refer (a b c))`. Qualified names like `foo/bar` resolve via the
//! loaded module table at eval time.
//!
//! Loader injection: the eval crate is filesystem-free. Embedders pass
//! a `Loader` trait object that resolves a module path string into
//! source. `tatara-script` provides a `FilesystemLoader`; tests use an
//! in-memory `MapLoader`.
//!
//! Cycle detection: each `require` push the path onto a load stack;
//! re-entering the same path raises `EvalError::User`. This is the
//! simplest sound approach — no need for two-phase resolution.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::value::Value;

/// One module's contribution to the global symbol table:
/// every binding it defines, plus the subset that's been
/// `(provide)`-d as exported.
#[derive(Debug, Clone, Default)]
pub struct Module {
    pub path: Arc<str>,
    pub exports: HashSet<Arc<str>>,
    pub bindings: HashMap<Arc<str>, Value>,
}

impl Module {
    pub fn new(path: impl Into<Arc<str>>) -> Self {
        Self {
            path: path.into(),
            exports: HashSet::new(),
            bindings: HashMap::new(),
        }
    }

    /// Look up an exported binding. `None` if the name isn't defined
    /// or isn't in the export set.
    pub fn get_export(&self, name: &str) -> Option<Value> {
        if self.exports.contains(name) {
            self.bindings.get(name).cloned()
        } else {
            None
        }
    }

    /// Add to the export set. Idempotent.
    pub fn add_export(&mut self, name: impl Into<Arc<str>>) {
        self.exports.insert(name.into());
    }

    /// Bind a value (either from a `define` while loading or from
    /// embedder pre-population).
    pub fn define(&mut self, name: impl Into<Arc<str>>, value: Value) {
        self.bindings.insert(name.into(), value);
    }
}

/// Source-loading hook. Resolves a `module path` (the string the user
/// wrote in `(require "path")`) into its source text. Embedders own
/// the path semantics — relative-to-cwd, relative-to-caller, search
/// path with `$TATARA_PATH`, in-memory map for tests, etc.
pub trait Loader: Send + Sync {
    fn load(&self, path: &str) -> Result<String, ModuleError>;
}

/// In-memory loader — useful for tests and bundled-stdlib loading.
/// Path strings map directly to source strings; missing path → error.
#[derive(Default, Debug, Clone)]
pub struct MapLoader {
    pub modules: HashMap<String, String>,
}

impl MapLoader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: impl Into<String>, source: impl Into<String>) -> &mut Self {
        self.modules.insert(path.into(), source.into());
        self
    }
}

impl Loader for MapLoader {
    fn load(&self, path: &str) -> Result<String, ModuleError> {
        self.modules
            .get(path)
            .cloned()
            .ok_or_else(|| ModuleError::NotFound(path.to_string()))
    }
}

/// Default no-op loader for embedders that haven't wired one up yet.
/// Returns `NotFound` for every path; modules calling `(require ...)`
/// will surface that error to the user.
#[derive(Debug, Default, Clone)]
pub struct NoLoader;

impl Loader for NoLoader {
    fn load(&self, path: &str) -> Result<String, ModuleError> {
        Err(ModuleError::NotFound(path.to_string()))
    }
}

/// Errors specific to the module pipeline. Embedders convert these
/// to user-facing `EvalError::User { value: Value::Error(...) }`.
#[derive(Debug, Error, Clone)]
pub enum ModuleError {
    #[error("module not found: {0}")]
    NotFound(String),
    #[error("circular require: {path} (load stack: {stack})")]
    Circular {
        path: String,
        stack: String,
    },
    #[error("name not exported: {1} from module {0}")]
    NotExported(String, String),
}

/// Process-global module registry. Holds every module that's been
/// loaded so far, keyed by path. Two `(require "lib/auth")` calls
/// from different sites share one Module instance — the file is
/// loaded + evaluated exactly once.
#[derive(Debug, Default, Clone)]
pub struct ModuleRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Debug, Default)]
pub(crate) struct RegistryInner {
    pub(crate) modules: HashMap<Arc<str>, Module>,
    /// Currently-loading paths (for cycle detection).
    pub(crate) loading: Vec<String>,
    /// Exports declared via `(provide ...)` inside a still-loading
    /// module. Drained on `finish_load` and merged into the Module.
    /// Keyed by module path; value is the set of names provided.
    pub(crate) exports_staging: HashMap<String, HashSet<Arc<str>>>,
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Has this path already been fully loaded?
    pub fn has(&self, path: &str) -> bool {
        let g = self.inner.lock().unwrap();
        g.modules.contains_key(path)
    }

    /// Snapshot a loaded module. Returns `None` if not yet loaded.
    pub fn get(&self, path: &str) -> Option<Module> {
        let g = self.inner.lock().unwrap();
        g.modules.get(path).cloned()
    }

    /// Begin loading `path`. Pushes onto the load stack and returns
    /// `Err(Circular)` if the path is already on the stack.
    pub fn begin_load(&self, path: &str) -> Result<(), ModuleError> {
        let mut g = self.inner.lock().unwrap();
        if g.loading.iter().any(|p| p == path) {
            return Err(ModuleError::Circular {
                path: path.to_string(),
                stack: g.loading.join(" → "),
            });
        }
        g.loading.push(path.to_string());
        Ok(())
    }

    /// Finish loading `path` — remove from load stack, store final
    /// module bindings.
    pub fn finish_load(&self, module: Module) {
        let mut g = self.inner.lock().unwrap();
        g.loading.retain(|p| **p != *module.path);
        g.modules.insert(module.path.clone(), module);
    }

    /// Abort a load (e.g., after an error during eval). Drops the
    /// path from the load stack so retries can succeed.
    pub fn abort_load(&self, path: &str) {
        let mut g = self.inner.lock().unwrap();
        g.loading.retain(|p| p != path);
    }

    /// Number of fully-loaded modules. Useful for tests + tooling.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().modules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Internal access to the lock — used by the eval loop to stage
    /// exports during a module load.
    pub(crate) fn inner_lock(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        self.inner.lock().unwrap()
    }
}

/// Split a qualified name `foo/bar` into `(module-alias, member)`.
/// Returns `None` if there's no `/` separator (caller treats as a
/// plain unqualified name).
///
/// Multi-segment aliases like `lib/auth/validate-token` resolve to
/// alias = `lib/auth` and member = `validate-token` — i.e., the LAST
/// `/` is the separator. This matches Clojure semantics where
/// `lib.auth/validate-token` (using `.` for the alias and `/` for
/// the boundary) splits at the FINAL `/`.
pub fn split_qualified(name: &str) -> Option<(&str, &str)> {
    let idx = name.rfind('/')?;
    // A bare leading `/` (e.g. `/foo`) or trailing `/` (e.g. `foo/`)
    // isn't a qualified name.
    if idx == 0 || idx == name.len() - 1 {
        return None;
    }
    Some((&name[..idx], &name[idx + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_qualified_works() {
        assert_eq!(split_qualified("foo/bar"), Some(("foo", "bar")));
        assert_eq!(
            split_qualified("lib/auth/validate"),
            Some(("lib/auth", "validate"))
        );
        assert_eq!(split_qualified("plain"), None);
        assert_eq!(split_qualified("/leading"), None);
        assert_eq!(split_qualified("trailing/"), None);
    }

    #[test]
    fn map_loader_round_trips() {
        let mut l = MapLoader::new();
        l.insert("lib/auth", "(define x 42)");
        assert_eq!(l.load("lib/auth").unwrap(), "(define x 42)");
        assert!(matches!(l.load("missing"), Err(ModuleError::NotFound(_))));
    }

    #[test]
    fn registry_cycle_detection() {
        let r = ModuleRegistry::new();
        r.begin_load("a").unwrap();
        r.begin_load("b").unwrap();
        let err = r.begin_load("a").unwrap_err();
        assert!(matches!(err, ModuleError::Circular { .. }));
    }

    #[test]
    fn registry_finish_load_makes_module_visible() {
        let r = ModuleRegistry::new();
        r.begin_load("foo").unwrap();
        let mut m = Module::new("foo");
        m.define("x", Value::Int(42));
        m.add_export("x");
        r.finish_load(m);
        assert!(r.has("foo"));
        let exported = r.get("foo").unwrap().get_export("x");
        assert!(matches!(exported, Some(Value::Int(42))));
    }

    #[test]
    fn registry_finish_load_removes_from_loading() {
        let r = ModuleRegistry::new();
        r.begin_load("foo").unwrap();
        r.finish_load(Module::new("foo"));
        // Re-loading the same path should now succeed (not cyclic).
        r.begin_load("foo").unwrap();
        r.abort_load("foo");
    }

    #[test]
    fn module_get_export_respects_export_set() {
        let mut m = Module::new("test");
        m.define("public", Value::Int(1));
        m.define("private", Value::Int(2));
        m.add_export("public");
        assert!(matches!(m.get_export("public"), Some(Value::Int(1))));
        // private is bound but not exported.
        assert!(matches!(m.get_export("private"), None));
    }
}

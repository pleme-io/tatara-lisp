//! `TataraDomain` — a Rust type authorable as a Lisp `(<keyword> :k v …)` form.
//!
//! Apply `#[derive(TataraDomain)]` (from `tatara-lisp-derive`) and a plain
//! struct gains a full Lisp compiler: keyword dispatch, kwarg parsing, typed
//! field extraction.
//!
//! Also exposes a `DomainRegistry` + `linkme`-free `register_domain!` macro
//! so any crate that derives `TataraDomain` can auto-register itself; the
//! dispatcher then looks up unknown top-level forms by keyword at runtime.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::ast::Sexp;
use crate::error::{LispError, Result};

/// A Rust type compilable from a Lisp form.
pub trait TataraDomain: Sized {
    /// The Lisp keyword (e.g., `"defmonitor"`).
    const KEYWORD: &'static str;

    /// Parse the argument list (everything after the keyword) into Self.
    fn compile_from_args(args: &[Sexp]) -> Result<Self>;

    /// Parse a complete form; validates the head symbol matches `KEYWORD`.
    fn compile_from_sexp(form: &Sexp) -> Result<Self> {
        let list = form.as_list().ok_or_else(|| LispError::Compile {
            form: Self::KEYWORD.to_string(),
            message: "expected list form".into(),
        })?;
        let head = list
            .first()
            .and_then(|s| s.as_symbol())
            .ok_or_else(|| LispError::Compile {
                form: Self::KEYWORD.to_string(),
                message: "missing head symbol".into(),
            })?;
        if head != Self::KEYWORD {
            return Err(LispError::Compile {
                form: Self::KEYWORD.to_string(),
                message: format!("expected ({} ...), got ({} ...)", Self::KEYWORD, head),
            });
        }
        Self::compile_from_args(&list[1..])
    }
}

// ── kwarg parsing + typed extractors used by the derive macro ──────

pub type Kwargs<'a> = HashMap<String, &'a Sexp>;

pub fn parse_kwargs(args: &[Sexp]) -> Result<Kwargs<'_>> {
    let mut kw = HashMap::new();
    let mut i = 0;
    while i + 1 < args.len() {
        let key = args[i].as_keyword().ok_or_else(|| LispError::Compile {
            form: "kwargs".into(),
            message: format!("expected keyword at position {i}"),
        })?;
        kw.insert(key.to_string(), &args[i + 1]);
        i += 2;
    }
    if i < args.len() {
        return Err(LispError::OddKwargs);
    }
    Ok(kw)
}

pub fn required<'a>(kw: &'a Kwargs<'_>, key: &str) -> Result<&'a Sexp> {
    kw.get(key).copied().ok_or_else(|| LispError::Compile {
        form: format!(":{key}"),
        message: "required but not provided".into(),
    })
}

fn type_err(key: &str, expected: &str) -> LispError {
    LispError::Compile {
        form: format!(":{key}"),
        message: format!("expected {expected}"),
    }
}

pub fn extract_string<'a>(kw: &'a Kwargs<'a>, key: &str) -> Result<&'a str> {
    required(kw, key)?
        .as_string()
        .ok_or_else(|| type_err(key, "string"))
}

pub fn extract_optional_string<'a>(kw: &'a Kwargs<'a>, key: &str) -> Result<Option<&'a str>> {
    match kw.get(key) {
        None => Ok(None),
        Some(v) => match v.as_string() {
            Some(s) => Ok(Some(s)),
            None => Err(type_err(key, "string")),
        },
    }
}

pub fn extract_string_list(kw: &Kwargs<'_>, key: &str) -> Result<Vec<String>> {
    let v = kw.get(key).copied();
    let Some(v) = v else {
        return Ok(vec![]);
    };
    let list = v
        .as_list()
        .ok_or_else(|| type_err(key, "list of strings"))?;
    list.iter()
        .map(|s| {
            s.as_string()
                .map(String::from)
                .ok_or_else(|| type_err(key, "list of strings"))
        })
        .collect()
}

pub fn extract_int(kw: &Kwargs<'_>, key: &str) -> Result<i64> {
    required(kw, key)?
        .as_int()
        .ok_or_else(|| type_err(key, "int"))
}

pub fn extract_optional_int(kw: &Kwargs<'_>, key: &str) -> Result<Option<i64>> {
    match kw.get(key) {
        None => Ok(None),
        Some(v) => v.as_int().map(Some).ok_or_else(|| type_err(key, "int")),
    }
}

pub fn extract_float(kw: &Kwargs<'_>, key: &str) -> Result<f64> {
    required(kw, key)?
        .as_float()
        .ok_or_else(|| type_err(key, "number"))
}

pub fn extract_optional_float(kw: &Kwargs<'_>, key: &str) -> Result<Option<f64>> {
    match kw.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_float()
            .map(Some)
            .ok_or_else(|| type_err(key, "number")),
    }
}

pub fn extract_bool(kw: &Kwargs<'_>, key: &str) -> Result<bool> {
    required(kw, key)?
        .as_bool()
        .ok_or_else(|| type_err(key, "bool"))
}

pub fn extract_optional_bool(kw: &Kwargs<'_>, key: &str) -> Result<Option<bool>> {
    match kw.get(key) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| type_err(key, "bool")),
    }
}

// ── Domain registry (runtime-registered, callable by keyword) ───────

/// Erased handler that knows how to compile a form and hand back a typed
/// serde-JSON representation. JSON is the least-common-denominator typed
/// surface — every `TataraDomain` derives `serde::Serialize` by convention.
pub struct DomainHandler {
    pub keyword: &'static str,
    pub compile: fn(args: &[Sexp]) -> Result<serde_json::Value>,
}

static REGISTRY: OnceLock<Mutex<HashMap<&'static str, DomainHandler>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<&'static str, DomainHandler>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a `TataraDomain` type with the global dispatcher.
/// Idempotent — repeated registrations overwrite.
pub fn register<T>()
where
    T: TataraDomain + serde::Serialize,
{
    let handler = DomainHandler {
        keyword: T::KEYWORD,
        compile: |args| {
            let v = T::compile_from_args(args)?;
            serde_json::to_value(&v).map_err(|e| LispError::Compile {
                form: T::KEYWORD.to_string(),
                message: format!("serialize: {e}"),
            })
        },
    };
    registry().lock().unwrap().insert(T::KEYWORD, handler);
}

/// Look up a handler by keyword.
pub fn lookup(keyword: &str) -> Option<DomainHandler> {
    let reg = registry().lock().unwrap();
    reg.get(keyword).map(|h| DomainHandler {
        keyword: h.keyword,
        compile: h.compile,
    })
}

/// List currently registered keywords.
pub fn registered_keywords() -> Vec<&'static str> {
    registry().lock().unwrap().keys().copied().collect()
}

// ── Capability registries — compounding metadata layer ────────────
//
// Each registered domain can ALSO carry capability metadata —
// orthogonal concerns the rest of the platform needs to ask about
// the type without importing it. Today: `RenderMetadata` (used by
// tatara-render to emit Kubernetes CR YAML without a hard-coded
// match). Future: `ComplianceMetadata`, `DocumentationMetadata`,
// `AttestationMetadata` — same shape, additional concerns.
//
// Each metadata kind has its own static registry parallel to
// `REGISTRY` (the handler registry). Domain crates call
// `register_render::<T>()` alongside `register::<T>()` during
// boot; consumers like `tatara-render` look up by keyword.

/// Type that knows its Kubernetes-CR rendering metadata. Tiny —
/// just constants. Implementing crates derive nothing; they
/// `impl RenderableDomain for FooSpec { … }` with three lines.
pub trait RenderableDomain {
    /// Kubernetes apiVersion the resource lives under
    /// (`gateway.networking.k8s.io/v1`, `cilium.io/v2`, etc.).
    const API_VERSION: &'static str;
    /// Kubernetes kind (`Gateway`, `CiliumNetworkPolicy`).
    const KIND: &'static str;
    /// Field name (in the typed JSON) that supplies the CR's
    /// `metadata.name`. Most domains use `name`; gateway-api
    /// uses `gateway_class_name`. Defaults via `Default` impl.
    const NAME_FIELD: &'static str = "name";
}

/// Erased render metadata — what `tatara-render` consumes.
#[derive(Clone, Copy, Debug)]
pub struct RenderHandler {
    pub keyword: &'static str,
    pub api_version: &'static str,
    pub kind: &'static str,
    pub name_field: &'static str,
}

static RENDER_REGISTRY: OnceLock<Mutex<HashMap<&'static str, RenderHandler>>> = OnceLock::new();

fn render_registry() -> &'static Mutex<HashMap<&'static str, RenderHandler>> {
    RENDER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a `RenderableDomain`'s metadata. Idempotent.
/// Domain crates call this once at boot, alongside `register::<T>()`.
pub fn register_render<T>()
where
    T: TataraDomain + RenderableDomain,
{
    let handler = RenderHandler {
        keyword: T::KEYWORD,
        api_version: T::API_VERSION,
        kind: T::KIND,
        name_field: T::NAME_FIELD,
    };
    render_registry().lock().unwrap().insert(T::KEYWORD, handler);
}

/// Look up render metadata by keyword.
#[must_use]
pub fn lookup_render(keyword: &str) -> Option<RenderHandler> {
    render_registry().lock().unwrap().get(keyword).copied()
}

/// List every keyword that has render metadata registered.
#[must_use]
pub fn registered_render_keywords() -> Vec<&'static str> {
    render_registry().lock().unwrap().keys().copied().collect()
}

// ── Documented capability ─────────────────────────────────────────
//
// Third capability layer (compile / render / doc). Each domain
// can carry its struct-level + field-level documentation strings
// for catalog browsers, IDE hover-help, and the `tatara doc`
// CLI to consult uniformly.

/// Type that knows its human-readable documentation. Tiny: one
/// `&'static str` for the type-level summary, plus an array of
/// (field, doc) pairs.
pub trait DocumentedDomain {
    /// Top-level docstring for the type — what an embedder sees
    /// when hovering the keyword in a catalog browser.
    const DOCSTRING: &'static str;
    /// Per-field docstrings, in declaration order. Empty when no
    /// docs were captured upstream (typical for hand-written
    /// domains until they fill them in). Forge-generated domains
    /// populate this from CRD `description` fields.
    const FIELD_DOCS: &'static [(&'static str, &'static str)];
}

/// Erased doc handle.
#[derive(Clone, Copy, Debug)]
pub struct DocHandler {
    pub keyword: &'static str,
    pub docstring: &'static str,
    pub field_docs: &'static [(&'static str, &'static str)],
}

static DOC_REGISTRY: OnceLock<Mutex<HashMap<&'static str, DocHandler>>> = OnceLock::new();

fn doc_registry() -> &'static Mutex<HashMap<&'static str, DocHandler>> {
    DOC_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a `DocumentedDomain`'s metadata. Idempotent.
pub fn register_doc<T>()
where
    T: TataraDomain + DocumentedDomain,
{
    let handler = DocHandler {
        keyword: T::KEYWORD,
        docstring: T::DOCSTRING,
        field_docs: T::FIELD_DOCS,
    };
    doc_registry().lock().unwrap().insert(T::KEYWORD, handler);
}

/// Look up doc metadata by keyword.
#[must_use]
pub fn lookup_doc(keyword: &str) -> Option<DocHandler> {
    doc_registry().lock().unwrap().get(keyword).copied()
}

/// List every keyword that has doc metadata registered.
#[must_use]
pub fn registered_doc_keywords() -> Vec<&'static str> {
    doc_registry().lock().unwrap().keys().copied().collect()
}

// ── Dependent capability ──────────────────────────────────────────
//
// Fourth capability layer (compile / render / doc / deps). Each
// domain can declare which OTHER keywords its instances logically
// depend on. The rollout pipeline consumes this to topo-sort the
// `Plan` so deploys land in the right order — apply
// `defservice` before `defpodmonitor` before `defciliumnetworkpolicy`,
// drain in reverse.

/// Type-level dependency declarations. The strings are keywords
/// of OTHER domains this one expects to be present (e.g. a
/// `defciliumnetworkpolicy` depends on a `defservice` whose pods
/// it selects). The dependency relation is type-to-type, not
/// instance-to-instance — finer-grained refs live on the typed
/// resource value itself.
pub trait DependentDomain {
    /// Keywords this domain logically depends on. Empty by
    /// default for forge-generated domains since CRDs don't
    /// generally declare deps; hand-written domains override
    /// to capture real ordering constraints.
    const DEPENDS_ON: &'static [&'static str];
}

/// Erased dep handle — what the topo-sort consumer reads.
#[derive(Clone, Copy, Debug)]
pub struct DepsHandler {
    pub keyword: &'static str,
    pub depends_on: &'static [&'static str],
}

static DEPS_REGISTRY: OnceLock<Mutex<HashMap<&'static str, DepsHandler>>> = OnceLock::new();

fn deps_registry() -> &'static Mutex<HashMap<&'static str, DepsHandler>> {
    DEPS_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a `DependentDomain`'s deps. Idempotent.
pub fn register_deps<T>()
where
    T: TataraDomain + DependentDomain,
{
    let handler = DepsHandler {
        keyword: T::KEYWORD,
        depends_on: T::DEPENDS_ON,
    };
    deps_registry().lock().unwrap().insert(T::KEYWORD, handler);
}

/// Look up dep metadata by keyword.
#[must_use]
pub fn lookup_deps(keyword: &str) -> Option<DepsHandler> {
    deps_registry().lock().unwrap().get(keyword).copied()
}

/// List every keyword that has dep metadata registered.
#[must_use]
pub fn registered_deps_keywords() -> Vec<&'static str> {
    deps_registry().lock().unwrap().keys().copied().collect()
}

// ── Schematic capability ──────────────────────────────────────────
//
// Fifth capability layer: per-domain JSON Schema export. Forge-
// generated domains preserve the source CRD's openAPIV3Schema
// verbatim; hand-written domains can either skip the layer or
// hand-curate a schema. Consumers: IDE hover-help, web
// validators, openapi exporters, admin-UI form generators —
// everyone who wants the typed shape without depending on the
// Rust struct directly.

pub trait SchematicDomain {
    /// JSON Schema source for this type. Preserved verbatim from
    /// the CRD's openAPIV3Schema for forge-generated domains;
    /// hand-curated for non-CRD domains. Consumers parse this on
    /// demand — keeping it as a static string avoids paying
    /// serde_json::Value at startup for every domain.
    const SCHEMA_JSON: &'static str;
}

#[derive(Clone, Copy, Debug)]
pub struct SchemaHandler {
    pub keyword: &'static str,
    pub schema_json: &'static str,
}

static SCHEMA_REGISTRY: OnceLock<Mutex<HashMap<&'static str, SchemaHandler>>> = OnceLock::new();

fn schema_registry() -> &'static Mutex<HashMap<&'static str, SchemaHandler>> {
    SCHEMA_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_schema<T>()
where
    T: TataraDomain + SchematicDomain,
{
    let handler = SchemaHandler {
        keyword: T::KEYWORD,
        schema_json: T::SCHEMA_JSON,
    };
    schema_registry().lock().unwrap().insert(T::KEYWORD, handler);
}

#[must_use]
pub fn lookup_schema(keyword: &str) -> Option<SchemaHandler> {
    schema_registry().lock().unwrap().get(keyword).copied()
}

#[must_use]
pub fn registered_schema_keywords() -> Vec<&'static str> {
    schema_registry().lock().unwrap().keys().copied().collect()
}

// ── Sexp ↔ serde_json bridge (universal type support) ──────────────
//
// Lets the derive macro fall through to `serde_json::from_value` for any
// field type implementing `Deserialize`. Handles enums (via symbol→string),
// nested structs (via kwargs→object), and `Vec<T>` of either.

use crate::ast::Atom;
use serde_json::Value as JValue;

/// Convert a Sexp to its canonical JSON form.
///
/// Rules:
///   - Symbols + Keywords → `Value::String`
///     (symbols are enum discriminants; keywords prefix with `:`)
///   - Strings, ints, floats, bools → their JSON counterpart
///   - Lists that look like `:k v :k v …` → `Value::Object`
///   - Other lists → `Value::Array`
///   - Quote/Quasiquote/Unquote/UnquoteSplice → convert the inner (strips quote)
pub fn sexp_to_json(s: &Sexp) -> JValue {
    match s {
        Sexp::Nil => JValue::Null,
        Sexp::Atom(Atom::Symbol(s)) => JValue::String(s.clone()),
        Sexp::Atom(Atom::Keyword(s)) => JValue::String(format!(":{s}")),
        Sexp::Atom(Atom::Str(s)) => JValue::String(s.clone()),
        Sexp::Atom(Atom::Int(n)) => JValue::Number((*n).into()),
        Sexp::Atom(Atom::Float(n)) => serde_json::Number::from_f64(*n)
            .map(JValue::Number)
            .unwrap_or(JValue::Null),
        Sexp::Atom(Atom::Bool(b)) => JValue::Bool(*b),
        Sexp::List(items) => {
            if is_kwargs_list(items) {
                let mut map = serde_json::Map::with_capacity(items.len() / 2);
                let mut i = 0;
                while i + 1 < items.len() {
                    if let Some(k) = items[i].as_keyword() {
                        map.insert(kebab_to_camel(k), sexp_to_json(&items[i + 1]));
                        i += 2;
                    } else {
                        break;
                    }
                }
                JValue::Object(map)
            } else {
                JValue::Array(items.iter().map(sexp_to_json).collect())
            }
        }
        Sexp::Quote(inner)
        | Sexp::Quasiquote(inner)
        | Sexp::Unquote(inner)
        | Sexp::UnquoteSplice(inner) => sexp_to_json(inner),
    }
}

/// Convert serde_json back to Sexp — inverse of `sexp_to_json`.
/// Used by `rewrite_typed` to round-trip a typed value through Lisp forms.
pub fn json_to_sexp(v: &JValue) -> Sexp {
    match v {
        JValue::Null => Sexp::Nil,
        JValue::Bool(b) => Sexp::boolean(*b),
        JValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Sexp::int(i)
            } else if let Some(f) = n.as_f64() {
                Sexp::float(f)
            } else {
                Sexp::int(0)
            }
        }
        JValue::String(s) => Sexp::string(s.clone()),
        JValue::Array(items) => Sexp::List(items.iter().map(json_to_sexp).collect()),
        JValue::Object(map) => {
            let mut out = Vec::with_capacity(map.len() * 2);
            for (k, v) in map {
                out.push(Sexp::keyword(camel_to_kebab(k)));
                out.push(json_to_sexp(v));
            }
            Sexp::List(out)
        }
    }
}

fn is_kwargs_list(items: &[Sexp]) -> bool {
    !items.is_empty()
        && items.len() % 2 == 0
        && items.iter().step_by(2).all(|s| s.as_keyword().is_some())
}

/// `must-reach` → `mustReach`, `point-type` → `pointType`.
fn kebab_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper = false;
    for c in s.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `mustReach` → `must-reach` (inverse of `kebab_to_camel`).
fn camel_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// ── TypedRewriter — the self-optimization primitive ────────────────
//
// Takes a typed value, converts to Sexp, applies a Lisp rewrite, then
// re-enters the typed boundary via `compile_from_args`. Any rewrite that
// passes the typed re-validation is safe by construction — the Rust type
// system is the floor.

/// Rewrite a typed `T` through Lisp form and re-validate on the way back.
///
/// The rewriter receives the value's kwargs representation (a `Sexp::List`
/// of alternating keywords + values) and returns a modified kwargs list.
/// `T::compile_from_args` validates the result — any ill-formed rewrite
/// produces a typed error; any well-formed rewrite produces a valid `T`.
pub fn rewrite_typed<T, F>(input: T, rewrite: F) -> Result<T>
where
    T: TataraDomain + serde::Serialize,
    F: FnOnce(Sexp) -> Result<Sexp>,
{
    let json = serde_json::to_value(&input).map_err(|e| LispError::Compile {
        form: T::KEYWORD.to_string(),
        message: format!("serialize {}: {e}", T::KEYWORD),
    })?;
    let sexp = json_to_sexp(&json);
    let rewritten = rewrite(sexp)?;
    let args = match rewritten {
        Sexp::List(items) => items,
        other => {
            return Err(LispError::Compile {
                form: T::KEYWORD.to_string(),
                message: format!("rewriter must return a list; got {other}"),
            })
        }
    };
    T::compile_from_args(&args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::read;
    use serde::Serialize;
    use tatara_lisp_derive::TataraDomain as DeriveTataraDomain;

    /// Example domain authorable as Lisp — proves derive macro, trait, and
    /// registry all agree end-to-end.
    #[derive(DeriveTataraDomain, Serialize, Debug, PartialEq)]
    #[tatara(keyword = "defmonitor")]
    struct MonitorSpec {
        name: String,
        query: String,
        threshold: f64,
        window_seconds: Option<i64>,
        tags: Vec<String>,
        enabled: Option<bool>,
    }

    #[test]
    fn derive_emits_correct_keyword() {
        assert_eq!(MonitorSpec::KEYWORD, "defmonitor");
    }

    #[test]
    fn derive_compiles_full_form() {
        let forms = read(
            r#"(defmonitor
                 :name "prom-up"
                 :query "up{job='prometheus'}"
                 :threshold 0.99
                 :window-seconds 300
                 :tags ("prod" "observability")
                 :enabled #t)"#,
        )
        .unwrap();
        let spec = MonitorSpec::compile_from_sexp(&forms[0]).unwrap();
        assert_eq!(
            spec,
            MonitorSpec {
                name: "prom-up".into(),
                query: "up{job='prometheus'}".into(),
                threshold: 0.99,
                window_seconds: Some(300),
                tags: vec!["prod".into(), "observability".into()],
                enabled: Some(true),
            }
        );
    }

    #[test]
    fn derive_accepts_missing_optionals() {
        let forms = read(r#"(defmonitor :name "x" :query "q" :threshold 0.5)"#).unwrap();
        let spec = MonitorSpec::compile_from_sexp(&forms[0]).unwrap();
        assert_eq!(spec.name, "x");
        assert!(spec.window_seconds.is_none());
        assert!(spec.enabled.is_none());
        assert!(spec.tags.is_empty());
    }

    #[test]
    fn derive_errors_on_missing_required() {
        let forms = read(r#"(defmonitor :name "x" :query "q")"#).unwrap();
        assert!(MonitorSpec::compile_from_sexp(&forms[0]).is_err());
    }

    #[test]
    fn derive_errors_on_wrong_head() {
        let forms = read(r#"(not-a-monitor :name "x")"#).unwrap();
        let err = MonitorSpec::compile_from_sexp(&forms[0]).unwrap_err();
        assert!(format!("{err}").contains("expected (defmonitor"));
    }

    #[test]
    fn registry_dispatches_by_keyword() {
        register::<MonitorSpec>();
        assert!(registered_keywords().contains(&"defmonitor"));
        let handler = lookup("defmonitor").expect("registered");
        assert_eq!(handler.keyword, "defmonitor");
        let forms = read(r#"(ignored :name "prom" :query "q" :threshold 0.5)"#).unwrap();
        let args = forms[0].as_list().unwrap();
        let json = (handler.compile)(&args[1..]).unwrap();
        assert_eq!(json["name"], "prom");
        assert_eq!(json["query"], "q");
        assert_eq!(json["threshold"], 0.5);
    }
}

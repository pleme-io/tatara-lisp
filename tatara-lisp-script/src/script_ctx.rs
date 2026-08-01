//! `ScriptCtx` — the host context passed to every stdlib FFI call.
//!
//! Holds a shared HTTP agent (connection pooling across calls), the
//! command-line argv, the current file being evaluated (for relative-
//! path `require`), and the require cache (so repeated `(require …)`
//! calls are no-ops). Embedders that want to extend the stdlib should
//! either expose `ScriptCtx` directly or wrap it in their own type
//! and re-register fn's against that.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Default)]
pub struct ScriptCtx {
    /// Program arguments after the script path.
    /// e.g. for `tatara-script imports.tlisp lilitu_io --all` → `["lilitu_io", "--all"]`.
    pub argv: Vec<String>,

    /// The file currently being evaluated (if any). Used by `(require)`
    /// to resolve relative paths against the caller's directory.
    pub current_file: Option<PathBuf>,

    /// Set of canonical absolute paths already required. Prevents
    /// re-evaluation on the second (or third, …) `(require …)` of the
    /// same file — required forms define globals once.
    pub required: HashSet<PathBuf>,

    /// Lazily-initialized shared HTTP agent (connection pooling).
    http_agent: Option<ureq::Agent>,

    /// Recorded test cases when in `--test` mode. Each entry is a
    /// (name, thunk-closure) pair — the closure captures the test body
    /// for deferred execution.
    pub tests: Vec<TestCase>,

    /// Owns every temp path `(tmp-dir)` / `(tmp-file)` hands out, and removes
    /// them when this context drops.
    ///
    /// PRIVATE on purpose. The old stdlib built its own path inline and
    /// returned a bare string, so nothing owned it and nothing ever cleaned it
    /// up — 21,608 dirs / 13 GB on rio, into a tmpfs, i.e. into RAM. Routing
    /// every mint through [`ScriptCtx::scratch_dir`] / [`ScriptCtx::scratch_file`]
    /// is what makes "created but never cleaned" unrepresentable rather than
    /// merely discouraged: there is no longer a constructor that skips the
    /// registry. See `scratch.rs` for the full incident.
    scratch: crate::scratch::ScratchRegistry,
}

/// A collected `(deftest name body)` form awaiting `--test` execution.
pub struct TestCase {
    pub name: String,
    pub body: Vec<tatara_lisp::Spanned>,
}

impl ScriptCtx {
    /// Construct a context with the given argv. Used by the binary
    /// entry point; embedders may prefer to start from `Default` and
    /// populate argv directly.
    pub fn with_argv<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            current_file: None,
            required: HashSet::new(),
            http_agent: None,
            tests: Vec::new(),
            scratch: crate::scratch::ScratchRegistry::default(),
        }
    }

    /// Mint a scratch DIRECTORY owned by this context.
    ///
    /// The returned path is removed when the context drops. This is the only
    /// way the stdlib can obtain one.
    ///
    /// # Errors
    /// Propagates any `create_dir_all` failure.
    pub fn scratch_dir(&mut self) -> std::io::Result<std::path::PathBuf> {
        self.scratch.dir()
    }

    /// Mint a scratch FILE (created empty) owned by this context.
    ///
    /// The returned path is removed when the context drops. This is the only
    /// way the stdlib can obtain one.
    ///
    /// # Errors
    /// Propagates any write failure.
    pub fn scratch_file(&mut self) -> std::io::Result<std::path::PathBuf> {
        self.scratch.file()
    }

    /// How many scratch entries this context currently owns. Test surface.
    #[must_use]
    pub fn scratch_len(&self) -> usize {
        self.scratch.len()
    }

    /// Return a shared HTTP agent, initializing on first use.
    pub fn http(&mut self) -> &ureq::Agent {
        self.http_agent.get_or_insert_with(|| {
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(30)))
                .build()
                .new_agent()
        })
    }
}

//! `ScriptCtx` — the host context passed to every stdlib FFI call.
//!
//! Holds a shared HTTP agent (so connection pooling persists across
//! calls) and the command-line argv. Embedders that want to extend the
//! stdlib should either expose `ScriptCtx` directly or wrap it in their
//! own type and re-register fn's against that.

use std::time::Duration;

#[derive(Default)]
pub struct ScriptCtx {
    /// Program arguments after the script path.
    /// e.g. for `tatara-script imports.tlisp lilitu_io --all` → `["lilitu_io", "--all"]`.
    pub argv: Vec<String>,

    /// Lazily-initialized shared HTTP agent (connection pooling).
    http_agent: Option<ureq::Agent>,
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
            http_agent: None,
        }
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

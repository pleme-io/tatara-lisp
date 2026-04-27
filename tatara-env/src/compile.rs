//! Compile a Lisp program (a list of top-level forms) into a typed
//! `Env` snapshot.
//!
//! Walk pass:
//!
//!   1. Find the `(defenv …)` form. Parse via the registered
//!      `EnvSpec` handler.
//!   2. For every other form, look up its head symbol in the
//!      global domain registry. If found, dispatch — collect the
//!      typed `serde_json::Value` plus the form's keyword.
//!      If not found, skip silently (the form might be a macro
//!      def, a comment, an `eval`-only expr, etc.).
//!   3. Bundle into `Env` for downstream consumption.
//!
//! Multiple `(defenv …)` forms in one program is rejected — a
//! program describes one env. Multi-env programs use multiple
//! files plus an outer composition (future work).

use crate::spec::EnvSpec;
use serde::{Deserialize, Serialize};
use tatara_lisp::Sexp;
use thiserror::Error;

/// Errors from the compile pass.
#[derive(Debug, Error)]
pub enum CompileError {
    #[error("program declares no `(defenv …)` form")]
    MissingDefenv,
    #[error("program declares more than one `(defenv …)` — found {0}")]
    MultipleDefenvs(usize),
    #[error("`(defenv …)` form failed to compile: {0}")]
    BadDefenv(String),
    #[error("resource `({head} …)`: {message}")]
    ResourceCompile { head: String, message: String },
}

/// One typed resource collected from the program.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    /// The head keyword that produced it (`defgateway`,
    /// `defbpf-program`, etc.). Useful for downstream filtering.
    pub keyword: String,
    /// The compiled form, as the registry handler returned it.
    /// Different domains produce different shapes; treating them
    /// as opaque JSON keeps the env crate domain-agnostic.
    pub value: serde_json::Value,
}

/// A compiled environment — the env metadata plus every typed
/// resource form in the program. The pipeline consumes this
/// shape directly: arch-synthesizer reads it, FluxCD writers
/// emit Kustomizations for it, tameshi attests its BLAKE3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Env {
    pub spec: EnvSpec,
    pub resources: Vec<Resource>,
}

impl Env {
    /// Filter resources by head keyword. Convenient for
    /// downstream passes that only care about one domain
    /// (`env.resources_by_keyword("defbpf-policy")`).
    #[must_use]
    pub fn resources_by_keyword(&self, kw: &str) -> Vec<&Resource> {
        self.resources.iter().filter(|r| r.keyword == kw).collect()
    }

    /// All keywords represented in this env, in the order they
    /// first appeared. Useful for sanity-checking that every
    /// imported domain actually has at least one resource.
    #[must_use]
    pub fn keywords(&self) -> Vec<&str> {
        let mut seen = Vec::new();
        for r in &self.resources {
            if !seen.iter().any(|k: &&str| **k == *r.keyword) {
                seen.push(r.keyword.as_str());
            }
        }
        seen
    }
}

/// Walk a list of top-level forms, dispatching each through the
/// global domain registry. Returns the assembled `Env` on
/// success.
///
/// Error vs. silently-skipped:
///
/// - Missing `(defenv …)` → `MissingDefenv` error (an env-shaped
///   program MUST declare metadata).
/// - Multiple `(defenv …)` → `MultipleDefenvs(n)` error.
/// - A form whose head is a registered domain keyword fails to
///   compile → `ResourceCompile` error (loud).
/// - A form whose head is NOT a registered domain keyword →
///   silently skipped (it's a macro def, a comment-equivalent,
///   a host-eval form). The synthesizer pipeline cares about
///   typed resources, not arbitrary Lisp.
pub fn compile_into_env(forms: &[Sexp]) -> Result<Env, CompileError> {
    let mut spec: Option<EnvSpec> = None;
    let mut resources = Vec::new();
    let mut defenv_count = 0;

    for form in forms {
        let Some(list) = form.as_list() else {
            continue;
        };
        let Some(head) = list.first().and_then(Sexp::as_symbol) else {
            continue;
        };
        if head == "defenv" {
            defenv_count += 1;
            if defenv_count > 1 {
                continue;
            }
            let handler = tatara_lisp::domain::lookup(head).ok_or_else(|| {
                CompileError::BadDefenv(
                    "EnvSpec not registered — call tatara_env::register() before compiling"
                        .into(),
                )
            })?;
            let json = (handler.compile)(&list[1..])
                .map_err(|e| CompileError::BadDefenv(format!("{e}")))?;
            spec = Some(serde_json::from_value(json).map_err(|e| {
                CompileError::BadDefenv(format!("EnvSpec deserialize: {e}"))
            })?);
            continue;
        }
        // Any other form — dispatch through the registry. Skip
        // silently if no handler is registered for the keyword.
        if let Some(handler) = tatara_lisp::domain::lookup(head) {
            let value = (handler.compile)(&list[1..]).map_err(|e| {
                CompileError::ResourceCompile {
                    head: head.to_string(),
                    message: format!("{e}"),
                }
            })?;
            resources.push(Resource {
                keyword: head.to_string(),
                value,
            });
        }
    }

    if defenv_count > 1 {
        return Err(CompileError::MultipleDefenvs(defenv_count));
    }
    let spec = spec.ok_or(CompileError::MissingDefenv)?;
    Ok(Env { spec, resources })
}

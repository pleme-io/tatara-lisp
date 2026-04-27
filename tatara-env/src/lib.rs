//! tatara-env — typed environments that compose tatara-domain
//! resources into one validated stack snapshot.
//!
//! ## What it solves
//!
//! Domains in isolation are useful — `(defgateway …)` is a typed
//! gateway, `(defciliumnetworkpolicy …)` is a typed network
//! policy. But a real platform is **the composition** — gateway
//! + network policies + monitors + BPF programs + storage +
//! policy, all coherent, all referencing each other. Without a
//! composition layer, every consumer (arch-synthesizer, FluxCD
//! manifest writer, tameshi attestation builder) reinvents the
//! "collect heterogeneous resources" wheel.
//!
//! `tatara-env` is the layer. One typed `Env` snapshot per stack,
//! produced by walking a program's top-level forms and dispatching
//! each through the global domain registry. The output is a
//! stable, serializable graph downstream pipelines consume.
//!
//! ## Authoring shape
//!
//! ```lisp
//! (defenv
//!   :name "production"
//!   :description "Edge-protected, observed, gateway-API gated"
//!   :imports ("tatara-gateway-api"
//!             "tatara-cilium"
//!             "tatara-prometheus-operator"
//!             "tatara-ebpf"))
//!
//! (defgateway :gateway-class-name "nginx" :listeners ())
//! (defbpf-policy
//!   :name "edge_protection"
//!   :description "L4 SYN-flood mitigation."
//!   :programs ("drop_syn_flood")
//!   :maps ("syn_counter"))
//! ; …more resource forms…
//! ```
//!
//! `compile_into_env` reads the whole list, finds the one
//! `(defenv …)` form, treats every other form whose head is a
//! registered domain keyword as a resource, ignores comments +
//! macros + non-domain forms.
//!
//! ## What's compounding
//!
//! - **Cross-resource validation** — a future `validate` pass
//!   walks the resources looking for dangling refs (a `defservice`
//!   pointing at a `secret` that doesn't exist).
//! - **Stable serialization** — JSON Schema-friendly output for
//!   the rest of the pipeline.
//! - **Typed FluxCD / Helm / Pangea emission** — env →
//!   per-platform manifests via `arch-synthesizer`.
//! - **Diff-based rollouts** — BLAKE3 the env JSON; only changed
//!   resources reapply.
//!
//! Every one of those bolts on top of the typed graph this crate
//! produces.

pub mod compile;
pub mod lattice;
pub mod spec;
pub mod validate;

pub use compile::{compile_into_env, CompileError, Resource};
pub use lattice::ResourceKey;
pub use spec::EnvSpec;
pub use validate::{validate, ValidationError};

/// Register the `(defenv …)` keyword form. Embedders call this
/// once during boot, alongside their other domain `register()`
/// calls.
pub fn register() {
    tatara_lisp::domain::register::<EnvSpec>();
}

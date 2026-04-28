//! `(defenv …)` — the typed environment metadata form.
//!
//! Authoring shape:
//!
//! ```lisp
//! (defenv
//!   :name "production"
//!   :description "Edge-protected production cluster."
//!   :imports ("tatara-gateway-api" "tatara-cilium" "tatara-ebpf")
//!   :labels (:tier "prod" :region "us-east-1"))
//! ```
//!
//! Only the metadata lives here — the **resources** that make up
//! the env are sibling top-level forms picked up by
//! `compile::compile_into_env`. Keeping the metadata typed but
//! the body declarative is what lets the same env survive across
//! N programs and N synthesizer passes.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tatara_lisp_derive::TataraDomain;

/// The `(defenv …)` form. One per program — multiple defenvs in
/// one program is a structural error caught by `compile_into_env`.
#[derive(Debug, Clone, Serialize, Deserialize, TataraDomain)]
#[tatara(keyword = "defenv")]
pub struct EnvSpec {
    /// Env name — drives the synthesizer's output directory + the
    /// FluxCD Kustomization name + the tameshi attestation chain
    /// header. Must be a valid DNS-1123 label (caller validates).
    pub name: String,
    /// Human-readable description for catalog tooling.
    pub description: String,
    /// Domain crate names this env imports. Drives the
    /// `register()` call sequence — embedders consume this list
    /// to know which domain crates to load. Names match the
    /// crate's `[package].name`.
    #[serde(default)]
    pub imports: Vec<String>,
    /// Free-form key-value labels. Useful for synthesizer-side
    /// routing (env → cluster, env → namespace, env → tier).
    #[serde(default)]
    pub labels: IndexMap<String, String>,
}

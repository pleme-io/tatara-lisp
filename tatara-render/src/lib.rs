//! tatara-render — the RENDER phase morphism.
//!
//! ## Where it sits
//!
//! Phase 5 of the eight-phase convergence loop (THEORY §IV.3):
//!
//! ```text
//! 1. DECLARE    → tatara_env::compile_into_env  (Vec<Sexp> → Env)
//! 2. SIMULATE   → tatara_env::validate
//! 3. PROVE      → property tests on lattice laws
//! 4. REMEDIATE  → declared ⊔ remediations  (right-bias)
//! 5. RENDER     → tatara_render::Backend     ← THIS CRATE
//! 6. DEPLOY     → tatara_rollout::diff_envs + apply
//! 7. VERIFY     → observed ⊑ declared
//! 8. RECONVERGE → drifts_from → GOTO DECLARE
//! ```
//!
//! ## What it produces
//!
//! Each `Backend` impl turns a typed `Env` into a target-shaped
//! manifest set. The target ranges from "raw Kubernetes CR YAML
//! ready to apply" through "FluxCD Kustomization tree" to
//! "Pangea Ruby DSL" to "Terraform JSON" — every place the
//! existing pleme-io stack already accepts deployment input.
//!
//! Today this crate ships the simplest member of the family:
//! `KubernetesYaml`. It takes a typed env and produces, for each
//! resource, a YAML document in the target's expected shape:
//!
//!   - `defgateway` → `Gateway` CR (gateway.networking.k8s.io/v1)
//!   - `defciliumnetworkpolicy` → `CiliumNetworkPolicy` CR
//!   - `defpodmonitor` → `PodMonitor` CR
//!   - `defbpf-program` / `defbpf-map` / `defbpf-policy` →
//!     ConfigMaps describing the BPF spec, plus a sibling
//!     reference to the substrate-built `.bpf.o` (the actual
//!     loader is a separate Job/DaemonSet outside this crate).
//!
//! Every other backend (helm-chart-forge, pangea-forge, the
//! existing `iac-forge` family) is one trait impl away.
//!
//! ## Why a trait, not a function
//!
//! Adding a new target inherits all existing proofs. The
//! typescape types (`Env`, `Resource`) are where the
//! invariants live — `Backend` impls are pure projections. The
//! `Synthesizer` trait in `arch-synthesizer` is the same
//! shape; this crate's `Backend` is a narrower, env-specific
//! variant that lands faster than wiring through the full
//! typescape layer (which it can grow into).

pub mod backend;
pub mod kubernetes_yaml;

pub use backend::{Backend, Manifest, RenderError};
pub use kubernetes_yaml::KubernetesYaml;

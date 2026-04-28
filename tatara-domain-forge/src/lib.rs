//! tatara-domain-forge — turn typed inputs into tatara-lisp domain crates.
//!
//! Each generated crate exposes one or more `#[derive(TataraDomain)]`
//! structs + a `register()` function that wires their keyword forms
//! into a host `Interpreter`. Once registered, programs author the
//! domain via `(defwhatever :k v :k v …)` and the `tatara-lisp`
//! macroexpander dispatches to the typed Rust struct.
//!
//! Inputs supported:
//!
//!   - **K8s CRD YAML** (highest leverage — every CNCF project ships
//!     CRDs with OpenAPI v3 schemas baked in). One CRD → one struct
//!     + one keyword form. A multi-doc CRD bundle (e.g. the
//!     gateway-api install manifest) yields one struct per kind.
//!   - **OpenAPI 3.0 schema fragment** — for non-K8s sources
//!     (cloud APIs, Prometheus alert rules, OPA bundles).
//!   - **Hand-authored TOML** — escape hatch when the upstream has
//!     no machine-readable schema (HAProxy / nginx / iptables).
//!
//! The compounding play: this crate is written once. Every domain
//! after that is a config + a generate command. The first 8 layers
//! of the pleme-io cloud stack (eBPF, CNI, mesh, LB, storage, obs,
//! policy, compute) become mechanical to ship.
//!
//! Pillar 12 (generation over composition) operationalized for the
//! typescape itself.

pub mod ir;
pub mod source;
pub mod emit;

pub use ir::{Domain, DomainKind, Field, Resource, ScalarType, FieldType};
pub use source::{from_crd_yaml, from_crd_str, FromCrdError};
pub use emit::{emit_lib_rs, emit_cargo_toml, emit_readme, emit_register_fn, EmitOptions};

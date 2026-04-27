//! tatara-ebpf — author eBPF programs, maps, and policies in
//! tatara-lisp; build hermetically through Rust + aya.
//!
//! ## The merger
//!
//! eBPF is the canonical case where Rust + tatara-lisp earn their
//! keep. Three layers, each contributing what it does best:
//!
//! ```text
//!   tatara-lisp authoring      (defbpf-program drop-syn :kind :xdp …)
//!         ↓ typed surface
//!   typed Rust structs         BpfProgramSpec { kind, attach, … }
//!         ↓ codegen + build
//!   aya-compatible Rust src    #[xdp] fn drop_syn(ctx) -> u32 { … }
//!         ↓ rustc + libbpf
//!   BPF bytecode object        drop-syn.bpf.o   (content-addressed)
//!         ↓ runtime load
//!   kernel verifier + JIT      attached to eth0 ingress
//! ```
//!
//! This crate exposes the **typed surface** — three keyword forms
//! authorable from tatara-lisp once `register()` is called:
//!
//! - `(defbpf-program …)` — one BPF program (kind, attach point, source).
//! - `(defbpf-map …)` — one BPF map (kind, key/value, max-entries, pinning).
//! - `(defbpf-policy …)` — high-level composition: maps + programs +
//!   attach order — the IaC-style declaration that tools can apply.
//!
//! The runtime (`aya-runtime` feature) wires these to aya's loader,
//! attaching programs and surfacing maps for read/write. The codegen
//! tier (planned) lets you write the program **body** in tatara-lisp
//! and emits the matching aya Rust source automatically — the
//! "best merger of the two" the pleme-io theory points at.
//!
//! ## Why hand-written, not forge-generated
//!
//! BPF programs aren't CRDs. There's no upstream YAML schema we can
//! ingest mechanically — the authoring surface is itself the design
//! decision. Hand-written here, this crate is the canonical example
//! of the **non-CRD domain pattern**: any future "no-schema" wrapper
//! (HAProxy / nginx / iptables / WireGuard configs) follows the same
//! shape — typed structs + register() + tests.

pub mod bpf_fn;
pub mod codegen;
pub mod runtime;
pub mod spec;

pub use spec::{
    BpfAttachPoint, BpfMapKind, BpfMapSpec, BpfPolicySpec, BpfProgramKind, BpfProgramSpec,
};

/// Register every keyword form this domain exposes onto the host
/// interpreter. Embedders call this once during boot.
pub fn register() {
    tatara_lisp::domain::register::<BpfProgramSpec>();
    tatara_lisp::domain::register::<BpfMapSpec>();
    tatara_lisp::domain::register::<BpfPolicySpec>();
}

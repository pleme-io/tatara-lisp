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

// ── Capability registrations beyond the typed compile path ────────
//
// Hand-written domains declare their non-render capability metadata
// inline (the forge populates the trait impls for forge-generated
// domains; we do it ourselves here).

impl tatara_lisp::DocumentedDomain for BpfProgramSpec {
    const DOCSTRING: &'static str =
        "One BPF program — kind (XDP/TC/kprobe/...), attach point, source, license. \
         Loaded via aya at runtime; built hermetically through substrate's ebpf.nix.";
    const FIELD_DOCS: &'static [(&'static str, &'static str)] = &[
        ("name", "Program name — the symbol exported in the BPF object."),
        ("kind", "BPF program kind. Drives the aya `#[xdp]` etc. attribute."),
        ("attach", "Where the program attaches (interface, kernel symbol, cgroup, ...)"),
        ("source", "Path to the program body — `*.rs`, `*.bpf.o`, or `*.tlisp:fn`."),
        ("license", "SPDX license string. GPL required for most helpers."),
        ("pin_path", "Optional bpffs pin path so the program survives the loader."),
        ("uses_maps", "BPF maps this program reads or writes."),
    ];
}

impl tatara_lisp::DocumentedDomain for BpfMapSpec {
    const DOCSTRING: &'static str =
        "One BPF map — hash / array / per-cpu / ring-buf / etc. \
         The kernel-↔-userspace data plane for BPF programs.";
    const FIELD_DOCS: &'static [(&'static str, &'static str)] = &[
        ("name", "Map name."),
        ("kind", "Map kind — drives access pattern (hash/array/perf-event/...)"),
        ("key_size", "Key size in bytes (0 for keyless maps like RingBuf)."),
        ("value_size", "Value size in bytes."),
        ("max_entries", "Capacity. For RingBuf, total bytes (page-rounded)."),
        ("pin_path", "Optional bpffs pin path."),
    ];
}

impl tatara_lisp::DocumentedDomain for BpfPolicySpec {
    const DOCSTRING: &'static str =
        "Composition of programs + maps applied as one unit. The IaC-shape \
         arch-synthesizer + FluxCD consume.";
    const FIELD_DOCS: &'static [(&'static str, &'static str)] = &[
        ("name", "Policy name."),
        ("description", "Human-readable description."),
        ("programs", "Names of `defbpf-program`s composed in this policy."),
        ("maps", "Names of `defbpf-map`s composed in this policy."),
    ];
}

// Dependency layer — real edges this time. A policy is meaningful
// only when its constituent programs + maps are already declared,
// so it depends on both keywords. Programs depend on the maps they
// reference (captured per-instance via `uses_maps`; type-level
// they just depend on `defbpf-map`).
impl tatara_lisp::DependentDomain for BpfMapSpec {
    const DEPENDS_ON: &'static [&'static str] = &[];
}
impl tatara_lisp::DependentDomain for BpfProgramSpec {
    const DEPENDS_ON: &'static [&'static str] = &["defbpf-map"];
}
impl tatara_lisp::DependentDomain for BpfPolicySpec {
    const DEPENDS_ON: &'static [&'static str] = &["defbpf-program", "defbpf-map"];
}

/// Register every keyword form this domain exposes onto the host
/// interpreter, plus its non-compile capability metadata. Embedders
/// call this once during boot.
pub fn register() {
    tatara_lisp::domain::register::<BpfProgramSpec>();
    tatara_lisp::domain::register::<BpfMapSpec>();
    tatara_lisp::domain::register::<BpfPolicySpec>();
    // Doc layer — markdown hover help / catalog browser.
    tatara_lisp::domain::register_doc::<BpfProgramSpec>();
    tatara_lisp::domain::register_doc::<BpfMapSpec>();
    tatara_lisp::domain::register_doc::<BpfPolicySpec>();
    // Deps layer — typed topo-sort over the rollout plan.
    tatara_lisp::domain::register_deps::<BpfProgramSpec>();
    tatara_lisp::domain::register_deps::<BpfMapSpec>();
    tatara_lisp::domain::register_deps::<BpfPolicySpec>();
}

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

// Attestation layer — same namespace for all three bpf domains.
// The pleme.io group prefix prevents collision with the CNCF
// k8s.io namespace tree even if a future CRD picks the same
// keyword by accident.
impl tatara_lisp::AttestableDomain for BpfMapSpec {
    const ATTESTATION_NAMESPACE: &'static str = "pleme.io/ebpf";
}
impl tatara_lisp::AttestableDomain for BpfProgramSpec {
    const ATTESTATION_NAMESPACE: &'static str = "pleme.io/ebpf";
}
impl tatara_lisp::AttestableDomain for BpfPolicySpec {
    const ATTESTATION_NAMESPACE: &'static str = "pleme.io/ebpf";
}

// Validation layer — semantic checks the type system can't
// catch. The kernel verifier rejects programs that call GPL-only
// helpers without a GPL-compatible license; surfacing that at
// compile time is far friendlier than a runtime ENOPKG. We're
// conservative: any program that touches a map demands a
// GPL-compatible license, since `bpf_map_lookup_elem` is GPL.
impl tatara_lisp::ValidatedDomain for BpfProgramSpec {
    fn validate_value(value: &serde_json::Value) -> std::result::Result<(), String> {
        let obj = value
            .as_object()
            .ok_or_else(|| "expected JSON object".to_string())?;
        let license = obj
            .get("license")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let uses_maps = obj
            .get("uses_maps")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if uses_maps && !is_gpl_compatible(license) {
            return Err(format!(
                "BPF program declares `:uses-maps` but `:license` `{license}` \
                 is not GPL-compatible — kernel verifier will reject \
                 calls to bpf_map_lookup_elem etc."
            ));
        }
        // Per-kind sanity: XDP / TC need an interface in attach.target.
        if let Some(kind) = obj.get("kind").and_then(|v| v.as_str()) {
            let attach_target = obj
                .get("attach")
                .and_then(|a| a.get("target"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let needs_iface = matches!(kind, ":xdp" | ":tc");
            if needs_iface && attach_target.is_empty() {
                return Err(format!(
                    "BPF program kind `{kind}` requires `:attach (:target \"<iface>\")` — got empty target"
                ));
            }
        }
        Ok(())
    }
}

fn is_gpl_compatible(license: &str) -> bool {
    matches!(license, "GPL" | "GPL v2" | "Dual MIT/GPL" | "Dual BSD/GPL")
}

impl tatara_lisp::ValidatedDomain for BpfMapSpec {}
impl tatara_lisp::ValidatedDomain for BpfPolicySpec {}

// Compliance layer — BPF programs at the kernel boundary
// participate in NIST SC-7 (boundary protection) and CIS 5.1
// (network controls) when they enforce L4 policy. Programs
// alone don't satisfy a control; the policy DOES (it's the
// auditable unit). Maps are pure data, claim no controls.
impl tatara_lisp::CompliantDomain for BpfMapSpec {
    const FRAMEWORKS: &'static [&'static str] = &[];
    const CONTROLS: &'static [&'static str] = &[];
}
impl tatara_lisp::CompliantDomain for BpfProgramSpec {
    const FRAMEWORKS: &'static [&'static str] = &[];
    const CONTROLS: &'static [&'static str] = &[];
}
impl tatara_lisp::CompliantDomain for BpfPolicySpec {
    const FRAMEWORKS: &'static [&'static str] = &["NIST 800-53", "CIS"];
    const CONTROLS: &'static [&'static str] = &[
        "NIST SC-7",   // boundary protection
        "NIST SI-3",   // malicious code protection (when used as filter)
        "CIS 5.1",     // network access controls
    ];
}

// Lifecycle layer — kernel-attached programs need BlueGreen.
// The verifier rejects half-loaded state; the only safe shape
// is "load new program in parallel, atomically replace the
// attach point, unload the old one." Maps follow the same
// pattern when their key/value sizes change. Policies compose
// programs + maps so they inherit BlueGreen too.
impl tatara_lisp::LifecycleProtocol for BpfProgramSpec {
    const STRATEGY: tatara_lisp::RolloutStrategy = tatara_lisp::RolloutStrategy::BlueGreen;
    const DRAIN_SECONDS: u32 = 5;
}
impl tatara_lisp::LifecycleProtocol for BpfMapSpec {
    // Maps are state — recreating loses the contents. When the
    // shape changes we still need Recreate (no in-place resize),
    // but the drain is shorter since maps don't run code.
    const STRATEGY: tatara_lisp::RolloutStrategy = tatara_lisp::RolloutStrategy::Recreate;
    const DRAIN_SECONDS: u32 = 1;
}
impl tatara_lisp::LifecycleProtocol for BpfPolicySpec {
    const STRATEGY: tatara_lisp::RolloutStrategy = tatara_lisp::RolloutStrategy::BlueGreen;
    const DRAIN_SECONDS: u32 = 5;
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
    // Attestation layer — namespaced BLAKE3 for tameshi.
    tatara_lisp::domain::register_attest::<BpfProgramSpec>();
    tatara_lisp::domain::register_attest::<BpfMapSpec>();
    tatara_lisp::domain::register_attest::<BpfPolicySpec>();
    // Validation layer — semantic checks (license / attach target).
    tatara_lisp::domain::register_validate::<BpfProgramSpec>();
    tatara_lisp::domain::register_validate::<BpfMapSpec>();
    tatara_lisp::domain::register_validate::<BpfPolicySpec>();
    // Lifecycle layer — rollout strategy per resource kind.
    tatara_lisp::domain::register_lifecycle::<BpfProgramSpec>();
    tatara_lisp::domain::register_lifecycle::<BpfMapSpec>();
    tatara_lisp::domain::register_lifecycle::<BpfPolicySpec>();
    // Compliance layer — frameworks + controls per kind.
    tatara_lisp::domain::register_compliance::<BpfProgramSpec>();
    tatara_lisp::domain::register_compliance::<BpfMapSpec>();
    tatara_lisp::domain::register_compliance::<BpfPolicySpec>();
}

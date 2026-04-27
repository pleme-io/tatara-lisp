//! Typed declaration surface — what an embedder authors when they
//! say `(defbpf-program …)`, `(defbpf-map …)`, `(defbpf-policy …)`.
//!
//! Each struct is the **canonical IR** for its concept. Both the
//! authoring side (tatara-lisp keyword forms) and the runtime side
//! (aya loader, when `aya-runtime` is enabled) read from the same
//! struct. No coercion, no DTO mismatches.
//!
//! Naming: BpfProgramSpec is the spec; BpfProgram (in `runtime.rs`)
//! is the live, attached handle. We never confuse the two — Rust's
//! type system carries the distinction.

use serde::{Deserialize, Serialize};
use tatara_lisp_derive::TataraDomain;

/// Where a BPF program attaches in the kernel.
///
/// Each kind dictates the program's calling convention, what context
/// pointer it receives, what return values mean, and which kernel
/// helpers it can call. The aya code generator emits the matching
/// `#[xdp]` / `#[classifier]` / `#[kprobe]` etc. attribute when it
/// produces source — so the kind selected here is load-bearing for
/// every downstream pass.
/// Note: serde rename strings include the leading `:` so they
/// round-trip through tatara-lisp's keyword wire form
/// (`Atom::Keyword(s) → ":<s>"` in `sexp_to_json`). When the
/// struct is consumed via plain JSON (e.g. from a manifest), the
/// `:` is part of the string literal too — keep it consistent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BpfProgramKind {
    /// XDP — eXpress Data Path. Runs on the NIC RX path before
    /// `skbuff` allocation. Returns XDP_PASS / DROP / TX / REDIRECT.
    /// Lowest latency; can't access skbuff metadata.
    #[serde(rename = ":xdp")]
    Xdp,
    /// TC ingress / egress classifier. Runs on the qdisc layer.
    /// Receives `__sk_buff`, can mark + redirect packets.
    #[serde(rename = ":tc")]
    Tc,
    /// Socket filter — classic packet capture / cgroup-bound socket
    /// observability. Used by tcpdump / strace-style tooling.
    #[serde(rename = ":socket-filter")]
    SocketFilter,
    /// Kprobe — attach to a kernel function entry / exit. The
    /// attach-point names a kernel symbol.
    #[serde(rename = ":kprobe")]
    Kprobe,
    /// Tracepoint — attach to a stable kernel tracepoint (under
    /// `/sys/kernel/debug/tracing/events/`). More stable than
    /// kprobes across kernel versions.
    #[serde(rename = ":tracepoint")]
    Tracepoint,
    /// Cgroup skb — runs on socket buffers entering / leaving a
    /// cgroup. Pairs with the cgroup id in the attach point.
    #[serde(rename = ":cgroup-skb")]
    CgroupSkb,
    /// LSM (Linux Security Module) hooks — fired on access-control
    /// decisions. The attach-point names an LSM hook.
    #[serde(rename = ":lsm")]
    Lsm,
    /// Perf event — runs on a perf-event sample (CPU cycles,
    /// instructions, cache misses). Powers profilers like
    /// `pyroscope`.
    #[serde(rename = ":perf-event")]
    PerfEvent,
}

/// Where a BPF program attaches at runtime.
///
/// The string format is kind-dependent: `eth0` for xdp/tc, the
/// kernel symbol for kprobe, the tracepoint path for tracepoint,
/// the cgroup path for cgroup-skb, the LSM hook name for lsm, etc.
/// We keep it as a `String` here and validate at runtime — the
/// authoring surface stays uniform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BpfAttachPoint {
    /// Kind-specific target identifier.
    pub target: String,
    /// Optional direction (`ingress` / `egress`) — only meaningful
    /// for tc and cgroup-skb. None elsewhere.
    #[serde(default)]
    pub direction: Option<String>,
}

/// One BPF program declaration.
///
/// Authored as:
///
/// ```lisp
/// (defbpf-program drop-syn-flood
///   :kind :xdp
///   :attach {:target "eth0"}
///   :source "bpf/drop_syn.rs"
///   :license "GPL"
///   :pin-path "/sys/fs/bpf/drop-syn-flood")
/// ```
///
/// Holds enough metadata for both the authoring tools (caixa-lint,
/// arch-synthesizer) and the runtime loader (aya feature). The
/// `source` field is intentionally generic — it can be a Rust file
/// path, a path to a pre-compiled `.bpf.o` object, or (in the
/// future) a tatara-lisp `(bpf-fn …)` form lifted from the same
/// program file via codegen.
#[derive(Debug, Clone, Serialize, Deserialize, TataraDomain)]
#[tatara(keyword = "defbpf-program")]
pub struct BpfProgramSpec {
    /// Program name — becomes the symbol exported in the BPF
    /// object. Must be a valid Rust identifier; the codegen pass
    /// uses it verbatim.
    pub name: String,
    /// Kind selects the BPF program type — drives the aya
    /// `#[xdp]` / `#[classifier]` / `#[kprobe]` attribute the
    /// codegen emits.
    pub kind: BpfProgramKind,
    /// Where the program attaches at load time.
    pub attach: BpfAttachPoint,
    /// Source for the program body. Three accepted shapes:
    ///   - `path/to/program.rs` — Rust source aya will compile.
    ///   - `path/to/program.bpf.o` — pre-compiled BPF object.
    ///   - `path/to/program.tlisp:bpf-fn-name` — tatara-lisp form
    ///     compiled to Rust via the planned codegen pass.
    /// The runtime + build pipeline disambiguates by extension /
    /// suffix.
    pub source: String,
    /// SPDX license string. BPF programs that call certain GPL-only
    /// helpers (most of `bpf_helpers.h`) MUST declare a
    /// GPL-compatible license; the kernel verifier checks this
    /// at load time. Defaults to "GPL" when omitted.
    #[serde(default = "default_license")]
    pub license: String,
    /// Optional bpffs pin path. When set, the program is pinned at
    /// the given path so it survives the loader process. Used
    /// when an external controller (e.g. a service mesh agent)
    /// owns lifetime separately from the loader.
    #[serde(default)]
    pub pin_path: Option<String>,
    /// Optional list of map names this program reads or writes.
    /// Cross-checked at validation time against the policy's
    /// `maps` declaration so a typo here surfaces before runtime.
    #[serde(default)]
    pub uses_maps: Vec<String>,
}

fn default_license() -> String {
    "GPL".into()
}

/// Kind of a BPF map.
///
/// Maps are the kernel-↔-user-space data plane for BPF programs.
/// Pick the kind to match access pattern: Hash for lookup-by-key,
/// PerCpu* for shard-per-CPU counters, RingBuf for streaming events,
/// Array for fixed-size dense indexing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BpfMapKind {
    #[serde(rename = ":hash")]
    Hash,
    #[serde(rename = ":lru-hash")]
    LruHash,
    #[serde(rename = ":array")]
    Array,
    #[serde(rename = ":per-cpu-hash")]
    PerCpuHash,
    #[serde(rename = ":per-cpu-array")]
    PerCpuArray,
    #[serde(rename = ":ring-buf")]
    RingBuf,
    #[serde(rename = ":perf-event-array")]
    PerfEventArray,
    #[serde(rename = ":stack-trace")]
    StackTrace,
    #[serde(rename = ":lpm-trie")]
    LpmTrie,
}

/// One BPF map declaration.
///
/// Authored as:
///
/// ```lisp
/// (defbpf-map syn-counter
///   :kind :per-cpu-array
///   :key-size 4
///   :value-size 8
///   :max-entries 1
///   :pin-path "/sys/fs/bpf/syn-counter")
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, TataraDomain)]
#[tatara(keyword = "defbpf-map")]
pub struct BpfMapSpec {
    pub name: String,
    pub kind: BpfMapKind,
    /// Key size in bytes. Some kinds (`RingBuf`, `PerfEventArray`)
    /// don't use a key — set to 0.
    #[serde(default)]
    pub key_size: u32,
    /// Value size in bytes. For `RingBuf`, this is the entry size;
    /// for `PerfEventArray`, the per-event payload limit.
    pub value_size: u32,
    /// Capacity. For `RingBuf`, this is total bytes (rounded up to
    /// page size). For dense kinds, it's the index range.
    pub max_entries: u32,
    /// Optional bpffs pin path — same role as on programs.
    #[serde(default)]
    pub pin_path: Option<String>,
}

/// A composition of programs + maps applied as a unit.
///
/// Policies are the IaC-shaped object the rest of the pleme-io
/// stack consumes — arch-synthesizer reads `BpfPolicySpec`,
/// validates ref-coherence (every `uses_maps` entry resolves to a
/// declared `BpfMapSpec`), then hands off to the substrate Nix
/// builder to compile each program's source into a BPF object.
///
/// Authored as:
///
/// ```lisp
/// (defbpf-policy edge-protection
///   :description "L4 SYN-flood mitigation on edge interfaces."
///   :programs ["drop-syn-flood" "rate-limit-tcp"]
///   :maps ["syn-counter" "tcp-state-table"])
/// ```
///
/// The programs and maps reference declarations made elsewhere by
/// name — typically in the same authoring file. The validator
/// resolves the names against the host registry.
#[derive(Debug, Clone, Serialize, Deserialize, TataraDomain)]
#[tatara(keyword = "defbpf-policy")]
pub struct BpfPolicySpec {
    pub name: String,
    pub description: String,
    /// Names of `BpfProgramSpec`s composed in this policy.
    pub programs: Vec<String>,
    /// Names of `BpfMapSpec`s composed in this policy. Programs
    /// reference these via their own `uses_maps` field.
    #[serde(default)]
    pub maps: Vec<String>,
}

impl BpfPolicySpec {
    /// Validate the policy's internal coherence — every program in
    /// `programs` exists in `programs_by_name`, every map referenced
    /// by a program (via its `uses_maps`) is declared in `maps`.
    /// Pure function — caller provides the lookup tables.
    pub fn validate(
        &self,
        programs_by_name: &std::collections::HashMap<String, BpfProgramSpec>,
        maps_by_name: &std::collections::HashMap<String, BpfMapSpec>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let known_maps: std::collections::HashSet<&str> =
            self.maps.iter().map(String::as_str).collect();
        for prog_name in &self.programs {
            let Some(prog) = programs_by_name.get(prog_name) else {
                errors.push(format!(
                    "policy `{}`: program `{}` not declared",
                    self.name, prog_name
                ));
                continue;
            };
            for m in &prog.uses_maps {
                if !known_maps.contains(m.as_str()) {
                    errors.push(format!(
                        "policy `{}`: program `{}` uses map `{}`, but the policy doesn't declare it",
                        self.name, prog_name, m
                    ));
                }
                if !maps_by_name.contains_key(m) {
                    errors.push(format!(
                        "policy `{}`: program `{}` uses map `{}`, which has no BpfMapSpec",
                        self.name, prog_name, m
                    ));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

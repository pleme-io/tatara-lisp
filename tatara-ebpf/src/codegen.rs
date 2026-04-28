//! Lisp → aya-Rust source generation.
//!
//! ## What this module does today (MVP)
//!
//! Given a `BpfProgramSpec`, emit the **aya wrapper** for the
//! program — the right `#[xdp]` / `#[classifier]` / `#[kprobe]`
//! attribute, the right context type, the right return type — and
//! splice in the program body provided by the embedder.
//!
//! Two body-source modes are supported:
//!
//! 1. **Rust verbatim** (`source: "path/to/program.rs"`) — the
//!    file is included as-is via `include_str!`. The codegen pass
//!    asserts the body has a `pub fn body(ctx)` shape and wraps it
//!    in the aya attribute. This is the canonical mode today.
//!
//! 2. **Object-only** (`source: "path/to/program.bpf.o"`) — the
//!    program is already compiled. Codegen emits a tiny stub that
//!    just describes the metadata; the substrate Nix builder
//!    pulls the object directly into the build closure.
//!
//! ## What this module will do (next phase)
//!
//! 3. **Tatara-lisp body** (`source: "path/to/program.tlisp:name"`)
//!    — the body is authored as a `(bpf-fn name [args] body)` form
//!    in tatara-lisp. The codegen pass walks the form and lowers
//!    it to aya-flavored Rust source. The lowering is restricted
//!    (no heap, no recursion, no calls outside `bpf_helpers`) — the
//!    BPF verifier sets the rules. Pillar-1-flavored: typed Rust
//!    primitives, declarative Lisp surface, hermetic Nix build.
//!
//! ## Why a generator and not a tatara-lisp → BPF backend
//!
//! Implementing a full Lisp → BPF bytecode backend is a real
//! compiler project (verifier-aware codegen, register allocation
//! under 11-register constraint, helper-call linking, BTF type
//! emission). Routing through aya means we inherit the entire
//! Rust BPF compilation chain (`bpf-linker`, `cargo-bpf`,
//! debug-symbol round-trip) for free. The Lisp surface stays a
//! thin layer over Rust where the heavy lifting already lives —
//! exactly the pattern Pillar 1 prescribes.

use crate::spec::{BpfProgramKind, BpfProgramSpec};
use std::fmt::Write;

/// Errors from the codegen pass.
#[derive(Debug, thiserror::Error)]
pub enum CodegenError {
    #[error("source `{0}`: shape not recognized — expected `*.rs`, `*.bpf.o`, or `*.tlisp:<fn>`")]
    UnrecognizedSource(String),
    #[error("tatara-lisp body source `{0}` is not yet supported in this codegen MVP — the Lisp → Rust lowering pass lands next phase")]
    LispBodyNotYet(String),
}

/// What aya attribute and signature shape to emit for a given
/// program kind. Centralized here so the rest of the pipeline can
/// stay agnostic to BPF-program-type taxonomy.
#[must_use]
pub fn aya_attribute(kind: &BpfProgramKind) -> &'static str {
    match kind {
        BpfProgramKind::Xdp => "#[xdp]",
        BpfProgramKind::Tc => "#[classifier]",
        BpfProgramKind::SocketFilter => "#[socket_filter]",
        BpfProgramKind::Kprobe => "#[kprobe]",
        BpfProgramKind::Tracepoint => "#[tracepoint]",
        BpfProgramKind::CgroupSkb => "#[cgroup_skb]",
        BpfProgramKind::Lsm => "#[lsm]",
        BpfProgramKind::PerfEvent => "#[perf_event]",
    }
}

/// Context type the program body receives — the Rust newtype aya
/// generates for that kind.
#[must_use]
pub fn aya_context_type(kind: &BpfProgramKind) -> &'static str {
    match kind {
        BpfProgramKind::Xdp => "aya_ebpf::programs::XdpContext",
        BpfProgramKind::Tc => "aya_ebpf::programs::TcContext",
        BpfProgramKind::SocketFilter => "aya_ebpf::programs::SkBuffContext",
        BpfProgramKind::Kprobe => "aya_ebpf::programs::ProbeContext",
        BpfProgramKind::Tracepoint => "aya_ebpf::programs::TracePointContext",
        BpfProgramKind::CgroupSkb => "aya_ebpf::programs::SkBuffContext",
        BpfProgramKind::Lsm => "aya_ebpf::programs::LsmContext",
        BpfProgramKind::PerfEvent => "aya_ebpf::programs::PerfEventContext",
    }
}

/// Conventional return type — what the verifier expects.
#[must_use]
pub fn aya_return_type(kind: &BpfProgramKind) -> &'static str {
    match kind {
        BpfProgramKind::Xdp => "u32",
        BpfProgramKind::Tc => "i32",
        BpfProgramKind::SocketFilter => "u32",
        BpfProgramKind::Kprobe | BpfProgramKind::Tracepoint => "u32",
        BpfProgramKind::CgroupSkb => "i32",
        BpfProgramKind::Lsm => "i32",
        BpfProgramKind::PerfEvent => "u32",
    }
}

/// Classify a program's `source` field by extension. Drives
/// codegen mode + the substrate Nix builder's input handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceShape {
    /// Hand-written or codegen-produced Rust source for aya.
    RustFile(String),
    /// Already-compiled BPF object — substrate just consumes it.
    PrecompiledObject(String),
    /// `path/to/file.tlisp:fn-name` — Lisp body to lower (planned).
    LispBody { path: String, body_fn: String },
}

/// Parse a `source` field into a structured shape. Pure function;
/// no I/O (the caller checks file existence later).
pub fn classify_source(source: &str) -> Result<SourceShape, CodegenError> {
    if source.ends_with(".rs") {
        return Ok(SourceShape::RustFile(source.to_string()));
    }
    if source.ends_with(".bpf.o") || source.ends_with(".o") {
        return Ok(SourceShape::PrecompiledObject(source.to_string()));
    }
    if let Some((path, body_fn)) = source.split_once(':') {
        if path.ends_with(".tlisp") {
            return Ok(SourceShape::LispBody {
                path: path.to_string(),
                body_fn: body_fn.to_string(),
            });
        }
    }
    Err(CodegenError::UnrecognizedSource(source.to_string()))
}

/// Render the aya Rust wrapper for a program. The `body_block`
/// argument is the function body (everything between the `{` and
/// `}` of the body fn) — the codegen pass embeds it verbatim. For
/// `RustFile` sources, the substrate Nix builder reads the file
/// and passes its `body` fn body in here. For `LispBody`, the
/// (planned) lowering produces the block.
///
/// Returns one Rust string ready to drop into `src/main.rs` of an
/// aya BPF crate.
#[must_use]
pub fn emit_aya_program(spec: &BpfProgramSpec, body_block: &str) -> String {
    let attr = aya_attribute(&spec.kind);
    let ctx_ty = aya_context_type(&spec.kind);
    let ret_ty = aya_return_type(&spec.kind);
    let mut out = String::new();
    let _ = writeln!(out, "// Auto-generated by tatara-ebpf. Do not hand-edit.");
    let _ = writeln!(
        out,
        "// Source spec: {} ({:?})",
        spec.name, spec.kind
    );
    let _ = writeln!(out, "// License: {}", spec.license);
    let _ = writeln!(out);
    let _ = writeln!(out, "{attr}");
    let _ = writeln!(out, "pub fn {name}(ctx: {ctx_ty}) -> {ret_ty} {{", name = spec.name);
    for line in body_block.lines() {
        let _ = writeln!(out, "    {line}");
    }
    let _ = writeln!(out, "}}");
    out
}

/// Convenience: emit the wrapper for a precompiled-object source.
/// Doesn't include a body — for these programs, the substrate Nix
/// builder pulls the `.bpf.o` directly into the build closure and
/// codegen only emits a metadata stub the loader can read.
#[must_use]
pub fn emit_precompiled_stub(spec: &BpfProgramSpec, object_path: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "// Auto-generated stub for precompiled BPF object.");
    let _ = writeln!(
        out,
        "// Spec: {} ({:?})  Object: {}",
        spec.name, spec.kind, object_path
    );
    let _ = writeln!(
        out,
        "pub const {}_OBJECT_PATH: &str = {object_path:?};",
        spec.name.to_uppercase().replace('-', "_")
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{BpfAttachPoint, BpfProgramKind};

    fn sample_spec(kind: BpfProgramKind) -> BpfProgramSpec {
        BpfProgramSpec {
            name: "drop_syn".into(),
            kind,
            attach: BpfAttachPoint {
                target: "eth0".into(),
                direction: None,
            },
            source: "bpf/drop_syn.rs".into(),
            license: "GPL".into(),
            pin_path: None,
            uses_maps: vec!["syn_counter".into()],
        }
    }

    #[test]
    fn classifies_rust_source() {
        assert_eq!(
            classify_source("bpf/drop_syn.rs").unwrap(),
            SourceShape::RustFile("bpf/drop_syn.rs".into())
        );
    }

    #[test]
    fn classifies_precompiled_object() {
        assert_eq!(
            classify_source("bpf/drop_syn.bpf.o").unwrap(),
            SourceShape::PrecompiledObject("bpf/drop_syn.bpf.o".into())
        );
    }

    #[test]
    fn classifies_lisp_body() {
        assert_eq!(
            classify_source("bpf/policies.tlisp:drop-syn").unwrap(),
            SourceShape::LispBody {
                path: "bpf/policies.tlisp".into(),
                body_fn: "drop-syn".into(),
            }
        );
    }

    #[test]
    fn rejects_unknown_source() {
        assert!(matches!(
            classify_source("bpf/drop_syn.bin").unwrap_err(),
            CodegenError::UnrecognizedSource(_)
        ));
    }

    #[test]
    fn emits_xdp_wrapper_with_correct_attribute() {
        let spec = sample_spec(BpfProgramKind::Xdp);
        let src = emit_aya_program(&spec, "Ok(xdp_action::XDP_PASS)");
        assert!(src.contains("#[xdp]"));
        assert!(src.contains("pub fn drop_syn(ctx: aya_ebpf::programs::XdpContext) -> u32"));
        assert!(src.contains("Ok(xdp_action::XDP_PASS)"));
    }

    #[test]
    fn emits_kprobe_wrapper_with_probe_context() {
        let spec = sample_spec(BpfProgramKind::Kprobe);
        let src = emit_aya_program(&spec, "0");
        assert!(src.contains("#[kprobe]"));
        assert!(src.contains("ProbeContext"));
    }

    #[test]
    fn precompiled_stub_exposes_object_path_const() {
        let spec = sample_spec(BpfProgramKind::Xdp);
        let stub = emit_precompiled_stub(&spec, "/nix/store/xxx-drop-syn.bpf.o");
        assert!(stub.contains("DROP_SYN_OBJECT_PATH"));
        assert!(stub.contains("/nix/store/xxx-drop-syn.bpf.o"));
    }
}

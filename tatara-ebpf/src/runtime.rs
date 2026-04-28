//! Runtime program loading + map lifecycle.
//!
//! Two-tier design:
//!
//! 1. **Trait surface** (`BpfRuntime`) — host-agnostic interface
//!    describing the operations a BPF runtime supports: load /
//!    attach / detach a program, open / read / write a map. This
//!    is always compiled. Embedders write code against the trait;
//!    different backends slot in.
//!
//! 2. **aya backend** (gated by the `aya-runtime` feature) — the
//!    canonical implementation on Linux, wrapping `aya::Bpf` and
//!    `aya::programs::Xdp` etc. Off by default because aya pulls
//!    in `libbpf-sys` which only builds on Linux + ties into
//!    kernel headers; gating keeps darwin / WASM consumers happy
//!    while they're using the typed surface declaratively.
//!
//! ## Why the trait
//!
//! Two non-aya backends are realistic:
//! - **Mock runtime** for tests + dry-run policy validation
//!   (`SimulatedRuntime` records load / attach calls instead of
//!   making syscalls — useful in CI without a kernel).
//! - **Remote runtime** for off-host loading (e.g. an in-cluster
//!   agent that owns the bpffs and exposes a gRPC surface).
//!
//! Same trait, different impl. Keeps the authoring surface +
//! validation logic identical across deployment shapes.

use crate::spec::{BpfMapSpec, BpfProgramSpec};

/// What kinds of operations a runtime supports. Implementations
/// MAY return `RuntimeError::Unsupported` for kinds outside the
/// backend's reach (e.g. a gRPC remote runtime might not expose
/// `read_map_entry` for low-level performance reasons).
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("operation not supported by this runtime backend")]
    Unsupported,
    #[error("kernel rejected program at load: {0}")]
    VerifierRejected(String),
    #[error("attach failed: {0}")]
    AttachFailed(String),
    #[error("map operation failed: {0}")]
    MapError(String),
    #[error("io: {0}")]
    Io(String),
}

/// Opaque handle to a loaded program. Backend-specific contents.
pub struct LoadedProgram {
    /// Spec-side name — survives across reloads.
    pub name: String,
    /// Backend-specific token. Unused by trait-aware code; backends
    /// downcast or pattern-match internally.
    pub token: u64,
}

/// The trait every runtime implements. Object-safe so embedders
/// can stash a `Box<dyn BpfRuntime>` in their config.
pub trait BpfRuntime: Send + Sync {
    /// Load a program into the kernel (or mock kernel). Returns
    /// the loaded handle on success. Verifier rejection produces
    /// `RuntimeError::VerifierRejected` with the kernel's diag
    /// string included.
    fn load_program(&mut self, spec: &BpfProgramSpec) -> Result<LoadedProgram, RuntimeError>;

    /// Attach a previously-loaded program to its declared
    /// `attach` point. Idempotent — re-attaching the same handle
    /// is a no-op when already live.
    fn attach_program(&mut self, prog: &LoadedProgram) -> Result<(), RuntimeError>;

    /// Detach + unload. After this, the handle is invalid.
    fn detach_program(&mut self, prog: LoadedProgram) -> Result<(), RuntimeError>;

    /// Create + pin a map. Idempotent — re-creating with the same
    /// name + spec is a no-op; mismatched specs error.
    fn create_map(&mut self, spec: &BpfMapSpec) -> Result<(), RuntimeError>;
}

/// Test / dry-run backend. Records every operation in a `Vec` so
/// callers can assert what the runtime *would* have done. Useful
/// in CI + during arch-synthesizer dry-runs.
#[derive(Debug, Default)]
pub struct SimulatedRuntime {
    pub loaded_programs: Vec<BpfProgramSpec>,
    pub attached_programs: Vec<String>,
    pub created_maps: Vec<BpfMapSpec>,
    next_token: u64,
}

impl SimulatedRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl BpfRuntime for SimulatedRuntime {
    fn load_program(&mut self, spec: &BpfProgramSpec) -> Result<LoadedProgram, RuntimeError> {
        self.loaded_programs.push(spec.clone());
        self.next_token += 1;
        Ok(LoadedProgram {
            name: spec.name.clone(),
            token: self.next_token,
        })
    }

    fn attach_program(&mut self, prog: &LoadedProgram) -> Result<(), RuntimeError> {
        self.attached_programs.push(prog.name.clone());
        Ok(())
    }

    fn detach_program(&mut self, prog: LoadedProgram) -> Result<(), RuntimeError> {
        self.attached_programs.retain(|n| n != &prog.name);
        Ok(())
    }

    fn create_map(&mut self, spec: &BpfMapSpec) -> Result<(), RuntimeError> {
        self.created_maps.push(spec.clone());
        Ok(())
    }
}

#[cfg(feature = "aya-runtime")]
mod aya_backend {
    //! Real aya-based runtime. Compiled only when `aya-runtime`
    //! is enabled. Lives in its own module so the public surface
    //! stays uniform regardless of the feature flag.
    //!
    //! Phase 1 (this MVP): stub. Phase 2: full Bpf::load_file +
    //! program-by-name + attach lifecycle.
    use super::*;

    pub struct AyaRuntime;

    impl BpfRuntime for AyaRuntime {
        fn load_program(&mut self, _spec: &BpfProgramSpec) -> Result<LoadedProgram, RuntimeError> {
            // Phase 2 wiring — bpf::load_file + bpf.program_mut(name)
            // + program.load(). The current stub keeps the trait
            // shape so consumers can compile + integrate the rest.
            Err(RuntimeError::Unsupported)
        }
        fn attach_program(&mut self, _prog: &LoadedProgram) -> Result<(), RuntimeError> {
            Err(RuntimeError::Unsupported)
        }
        fn detach_program(&mut self, _prog: LoadedProgram) -> Result<(), RuntimeError> {
            Err(RuntimeError::Unsupported)
        }
        fn create_map(&mut self, _spec: &BpfMapSpec) -> Result<(), RuntimeError> {
            Err(RuntimeError::Unsupported)
        }
    }
}

#[cfg(feature = "aya-runtime")]
pub use aya_backend::AyaRuntime;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{BpfAttachPoint, BpfMapKind, BpfProgramKind};

    fn sample_program() -> BpfProgramSpec {
        BpfProgramSpec {
            name: "drop_syn".into(),
            kind: BpfProgramKind::Xdp,
            attach: BpfAttachPoint {
                target: "eth0".into(),
                direction: None,
            },
            source: "bpf/drop_syn.rs".into(),
            license: "GPL".into(),
            pin_path: None,
            uses_maps: vec![],
        }
    }

    #[test]
    fn simulated_runtime_records_lifecycle() {
        let mut rt = SimulatedRuntime::new();
        let map = BpfMapSpec {
            name: "syn_counter".into(),
            kind: BpfMapKind::PerCpuArray,
            key_size: 4,
            value_size: 8,
            max_entries: 1,
            pin_path: None,
        };
        rt.create_map(&map).unwrap();
        assert_eq!(rt.created_maps.len(), 1);

        let spec = sample_program();
        let prog = rt.load_program(&spec).unwrap();
        assert_eq!(prog.name, "drop_syn");
        assert_eq!(rt.loaded_programs.len(), 1);

        rt.attach_program(&prog).unwrap();
        assert_eq!(rt.attached_programs, vec!["drop_syn"]);

        rt.detach_program(prog).unwrap();
        assert!(rt.attached_programs.is_empty());
    }
}

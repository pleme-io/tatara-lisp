//! tatara-rollout — edge-of-differences rollout planning.
//!
//! ## What it solves
//!
//! Real platforms reapply too much. FluxCD reconciles every
//! manifest in its tree; arch-synthesizer regenerates IaC for
//! every resource; helm rolls every release. When 1 resource
//! changed in an env of 200, you don't want all 200 reapplying.
//! That's the cost of operating without a typed diff.
//!
//! `tatara-rollout` computes the diff once, in typed Rust,
//! against `tatara-env::Env` snapshots. The output is a
//! `Plan` describing exactly which resources moved, in what way,
//! ready for the synthesizer + FluxCD + tameshi pipeline to
//! consume.
//!
//! ## How it works
//!
//! Each resource has a stable identity (`(keyword, name)`) and a
//! BLAKE3 fingerprint over its canonical JSON. Diffing two envs
//! is then a straight set-merge:
//!
//! - `id ∈ new ∧ id ∉ old`         → Add
//! - `id ∉ new ∧ id ∈ old`         → Remove
//! - `id ∈ both ∧ hash differs`    → Change
//! - `id ∈ both ∧ hash equal`      → Unchanged (skip; this is
//!                                   where the savings come from)
//!
//! Emit-time the synthesizer only walks Add + Change + Remove.
//! Unchanged resources don't produce output, don't restart,
//! don't trigger downstream reconciles. The "no interruption to
//! services" property the user asked about is the consequence of
//! this property holding all the way through the pipeline.
//!
//! ## What it doesn't do
//!
//! Rollout *protocol* — drain old before bringing up new, leader
//! election handoff, side-by-side blue/green — is per-shape and
//! lives elsewhere (`tatara-shape::reload::*`). The diff just
//! tells you what moved; the protocol decides how to move it.

pub mod diff;
pub mod fingerprint;
pub mod plan;

pub use diff::diff_envs;
pub use fingerprint::{fingerprint_env, fingerprint_resource, ResourceFingerprint};
pub use plan::{Change, Plan, ResourceId};

//! `Backend` trait — the morphism interface.
//!
//! Pleme-io's `arch-synthesizer/Backend` (the IaC-forge family)
//! takes typed-IR resources and produces target-language source.
//! This crate's `Backend` is the env-shaped variant: takes a
//! `tatara_env::Env`, produces a list of typed `Manifest`s.

use serde::{Deserialize, Serialize};
use tatara_env::compile::Env;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("unsupported keyword `{0}` — backend has no renderer for this domain")]
    Unsupported(String),
    #[error("resource `{keyword}/{name}`: {message}")]
    Resource {
        keyword: String,
        name: String,
        message: String,
    },
    #[error("yaml emit: {0}")]
    Yaml(String),
}

/// One rendered manifest. The `kind` is the target's idea of a
/// document type (`yaml`, `helm-template`, `pangea-rb`,
/// `terraform-json`); the `path` is where the manifest expects
/// to land in a Kustomization-style tree; the `content` is the
/// rendered bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Target document kind. Backends pick stable strings here
    /// so downstream consumers can fan out per-kind.
    pub kind: String,
    /// Suggested filesystem path inside the output tree, e.g.
    /// `gateways/production-gateway.yaml`. Backends produce paths
    /// the typical FluxCD / Kustomize layout prefers.
    pub path: String,
    /// Rendered bytes. UTF-8 by convention for text targets.
    pub content: String,
}

/// The morphism. Stateless by convention — backend impls hold
/// only their config (e.g. namespace defaults, label conventions),
/// not state derived from the env.
pub trait Backend {
    /// Render the env. Each domain handled by this backend
    /// produces zero-or-more manifests; unhandled domains return
    /// `RenderError::Unsupported` so the caller can decide
    /// whether to fail-soft or fail-hard.
    fn render(&self, env: &Env) -> Result<Vec<Manifest>, RenderError>;
}

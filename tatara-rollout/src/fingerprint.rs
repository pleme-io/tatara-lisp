//! BLAKE3 fingerprints over `Env` and individual `Resource`s.
//!
//! Two stable properties make these fingerprints actually useful:
//!
//! 1. **Canonical JSON** — `serde_json` doesn't guarantee field
//!    order across versions, but the resource values that flow
//!    through here are produced by `tatara-domain-forge`'s
//!    deterministic emit pass (struct field order = source order).
//!    We re-serialize through `serde_json::to_string` which keeps
//!    the IndexMap ordering, so the same typed value always
//!    fingerprints the same.
//! 2. **Hash one resource at a time, hash the env separately** —
//!    if you change one resource, only that resource's
//!    fingerprint moves. The env-level fingerprint is the BLAKE3
//!    of the Merkle tree of resource fingerprints, so both
//!    "what's the env hash" and "what's THIS resource's hash"
//!    are O(1) lookups once computed.

use crate::plan::ResourceId;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tatara_env::compile::{Env, Resource};

/// Fingerprint of one typed resource.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceFingerprint {
    pub id: ResourceId,
    /// Hex-encoded BLAKE3 of the resource's canonical JSON.
    pub blake3: String,
}

/// Compute the fingerprint for one resource.
///
/// Namespaced via the registered `AttestableDomain` namespace
/// when present (Layer 6). Falls back to a non-namespaced BLAKE3
/// if no attestation metadata is registered — the diff still
/// works correctly within a single domain, but cross-domain
/// equality of identical JSON shapes is possible only in the
/// fallback path. Forge-generated + hand-curated domains all
/// register attestation namespaces, so the fallback is rare.
#[must_use]
pub fn fingerprint_resource(r: &Resource) -> ResourceFingerprint {
    let blake3 = match tatara_lisp::domain::lookup_attest(&r.keyword) {
        Some(meta) => tatara_lisp::domain::attest_value(meta.namespace, &r.value),
        None => {
            let json = serde_json::to_string(&r.value).unwrap_or_else(|_| "<unserializable>".into());
            blake3::hash(json.as_bytes()).to_hex().to_string()
        }
    };
    ResourceFingerprint {
        id: ResourceId::from_resource(r),
        blake3,
    }
}

/// All fingerprints in an env, keyed by `ResourceId` so the
/// diff pass can look up by id directly. `IndexMap` preserves
/// resource declaration order — useful for stable display.
#[must_use]
pub fn fingerprint_env(env: &Env) -> IndexMap<ResourceId, ResourceFingerprint> {
    let mut out = IndexMap::with_capacity(env.resources.len());
    for r in &env.resources {
        let fp = fingerprint_resource(r);
        out.insert(fp.id.clone(), fp);
    }
    out
}

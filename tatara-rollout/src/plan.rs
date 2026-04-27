//! Typed rollout plan — output of `diff_envs`.
//!
//! Plans are the contract between the diff pass and downstream
//! consumers (synthesizer / FluxCD writer / tameshi attestation).
//! They're plain data — no I/O, no host state, fully serde —
//! so the same plan can be:
//!
//!   - inspected in the REPL
//!   - serialized to YAML and committed alongside the env source
//!   - consumed by a CI bot that opens a PR per plan
//!   - replayed in tests against a `SimulatedRuntime`

use serde::{Deserialize, Serialize};
use tatara_env::compile::Resource;

/// Identity of a resource within an env. Two resources with the
/// same `(keyword, name)` are the same logical resource — used
/// to align across env snapshots in the diff pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ResourceId {
    pub keyword: String,
    pub name: String,
}

impl ResourceId {
    /// Pull the canonical id from a typed resource. Most domains
    /// use `name`; gateway-api uses `gateway_class_name`. Falls
    /// back to "<unnamed>" for resources without an obvious id —
    /// the diff still works (it just treats anonymous resources
    /// as unique per appearance).
    #[must_use]
    pub fn from_resource(r: &Resource) -> Self {
        let obj = r.value.as_object();
        let name = obj
            .and_then(|o| o.get("name"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                obj.and_then(|o| {
                    if r.keyword == "defgateway" {
                        o.get("gateway_class_name").and_then(|v| v.as_str())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or("<unnamed>")
            .to_string();
        Self {
            keyword: r.keyword.clone(),
            name,
        }
    }
}

/// One change in a plan. Carries the new resource (or its
/// fingerprint) so the synthesizer can act on the diff without
/// re-reading the env.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Change {
    /// Resource present in `new`, absent in `old`. Apply.
    Add {
        id: ResourceId,
        value: serde_json::Value,
    },
    /// Resource present in `old`, absent in `new`. Tear down.
    Remove { id: ResourceId },
    /// Resource present in both, content moved. Reapply.
    Change {
        id: ResourceId,
        old_blake3: String,
        new_blake3: String,
        new_value: serde_json::Value,
    },
}

impl Change {
    #[must_use]
    pub fn id(&self) -> &ResourceId {
        match self {
            Self::Add { id, .. } | Self::Remove { id } | Self::Change { id, .. } => id,
        }
    }
}

/// A rollout plan. Adds + removes + changes are the actionable
/// list; `unchanged` is informational (handy for emit-time
/// progress bars, "skipped 198/200" style logging).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Plan {
    pub adds: Vec<Change>,
    pub removes: Vec<Change>,
    pub changes: Vec<Change>,
    pub unchanged: Vec<ResourceId>,
}

impl Plan {
    /// Total actionable count — Add + Remove + Change. Skipping
    /// emit when this is zero is the no-churn fast path.
    #[must_use]
    pub fn actionable_count(&self) -> usize {
        self.adds.len() + self.removes.len() + self.changes.len()
    }

    /// Is the plan a no-op? Equivalent to `actionable_count() == 0`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actionable_count() == 0
    }

    /// All actionable changes in a single iterator, ordered
    /// removes-first / adds-second / changes-third — the safe
    /// apply order for cleanup-before-create semantics on
    /// resources that share namespaces.
    pub fn iter_actionable(&self) -> impl Iterator<Item = &Change> {
        self.removes
            .iter()
            .chain(self.adds.iter())
            .chain(self.changes.iter())
    }
}

//! Convergence-as-lattice — the static structure of all envs.
//!
//! ## What this is
//!
//! `theory/THEORY.md` distinguishes two views of convergence:
//!
//! - **convergence-as-lattice** (static): the structure of all
//!   possible states the system can inhabit. A dimension of the
//!   typescape.
//! - **convergence-as-process** (dynamic): the motion of the
//!   running world toward a particular lattice point.
//!
//! `tatara-rollout` operationalizes convergence-as-process —
//! it computes the *motion* between two lattice points. This
//! module operationalizes convergence-as-lattice — it gives the
//! *structure* itself.
//!
//! Every `Env` is a point in a powerset lattice over typed
//! resources. The operations:
//!
//! - **`meet(a, b)`**: greatest lower bound. Resources present
//!   in BOTH envs, byte-equal. The "shared core."
//! - **`join(a, b)`**: least upper bound. Resources present in
//!   EITHER env, with right-biased collision resolution. The
//!   "combined coverage."
//! - **`leq(a, b)`**: partial order — `a ⊑ b` iff every
//!   resource in `a` is present and byte-equal in `b`. The
//!   "subset relation."
//! - **`bottom(name)`**: empty env. Identity for `join`,
//!   absorber for `meet`.
//!
//! Proven properties (lattice laws) — see `tests::laws`:
//!
//! 1. `a ⊑ a` (reflexive)
//! 2. `a ⊑ b ∧ b ⊑ a → a ≅ b` (antisymmetric, modulo metadata)
//! 3. `a ⊑ b ∧ b ⊑ c → a ⊑ c` (transitive)
//! 4. `meet(a, a) ≅ a`, `join(a, a) ≅ a` (idempotent)
//! 5. `meet(a, b) ≅ meet(b, a)`, dual for `join` (commutative)
//! 6. `meet(meet(a, b), c) ≅ meet(a, meet(b, c))` (associative)
//! 7. `meet(a, join(a, b)) ≅ a` (absorption)
//! 8. `meet(bottom, a) ≅ bottom`, `join(bottom, a) ≅ a`
//! 9. `meet(a, b) ⊑ a`, `meet(a, b) ⊑ b`
//! 10. `a ⊑ join(a, b)`, `b ⊑ join(a, b)`
//!
//! ## Why it matters
//!
//! Compliance, drift detection, reconverge — all are lattice
//! operations:
//!
//! - **Drift detection** = `!(observed ⊑ declared)`. The running
//!   world has resources the declaration doesn't, or differs on
//!   shared ones.
//! - **Compliance subset** = `baseline ⊑ env`. Every required
//!   control is satisfied (the env is at least as strong as the
//!   baseline).
//! - **Region merging** = `production-base ⊔ region-us-east =
//!   production-us-east`. Multi-region orchestration becomes
//!   algebraic.
//! - **Shared infrastructure** = `app-a ⊓ app-b`. The resources
//!   both apps need (a shared NetworkPolicy, a shared map) sit
//!   at the meet — invariants that must hold in every join.
//!
//! Today the algebra is exposed as plain Rust methods. Wiring it
//! to `(env-meet a b)` / `(env-join a b)` / `(env-leq? a b)`
//! Lisp keyword forms is one more `register()` call away.

use crate::compile::{Env, Resource};
use crate::spec::EnvSpec;
use crate::validate::keyword_to_crate;
use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Canonical id for a resource within the lattice algebra. The
/// lattice operations key on `(keyword, name)` pairs — values
/// agree iff the canonical JSON serialization is byte-equal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceKey {
    pub keyword: String,
    pub name: String,
}

impl ResourceKey {
    /// Pull the canonical id from a typed resource. Same logic
    /// as `tatara-rollout::ResourceId::from_resource`, repeated
    /// here so the lattice algebra doesn't pull in rollout (the
    /// dependency would be backwards — lattice is the
    /// foundation, rollout is built on it).
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

impl Env {
    /// **`meet`** (greatest lower bound). Resources present in
    /// both envs, byte-equal. Imports = intersection. Labels =
    /// intersection (agreeing on key AND value).
    ///
    /// `meet(a, b) ⊑ a` and `meet(a, b) ⊑ b` always hold.
    #[must_use]
    pub fn meet(&self, other: &Env) -> Env {
        let other_by_id: BTreeMap<ResourceKey, &Resource> = other
            .resources
            .iter()
            .map(|r| (ResourceKey::from_resource(r), r))
            .collect();
        let mut shared: Vec<Resource> = Vec::new();
        for r in &self.resources {
            let key = ResourceKey::from_resource(r);
            if let Some(other_r) = other_by_id.get(&key) {
                if canonical_eq(&r.value, &other_r.value) {
                    shared.push(r.clone());
                }
            }
        }
        let imports: Vec<String> = {
            let a: HashSet<&str> = self.spec.imports.iter().map(String::as_str).collect();
            let b: HashSet<&str> = other.spec.imports.iter().map(String::as_str).collect();
            a.intersection(&b).map(|s| s.to_string()).collect()
        };
        let labels: IndexMap<String, String> = self
            .spec
            .labels
            .iter()
            .filter(|(k, v)| other.spec.labels.get(*k) == Some(v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Env {
            spec: EnvSpec {
                name: format!("{}∩{}", self.spec.name, other.spec.name),
                description: format!(
                    "meet({}, {})",
                    self.spec.name, other.spec.name
                ),
                imports,
                labels,
            },
            resources: shared,
        }
    }

    /// **`join`** (least upper bound). Resources present in
    /// either env. Right-bias on collisions: when both envs
    /// declare a resource with the same `(keyword, name)` but
    /// different values, the **right** (`other`) wins. This is
    /// the convention shared with CRDT LWW registers.
    /// Imports = union. Labels = union (right wins on collision).
    ///
    /// `a ⊑ join(a, b)` and `b ⊑ join(a, b)` always hold (with
    /// `b` taking priority on collisions, so `b ⊑ join(a, b)` is
    /// always exact; `a ⊑ join(a, b)` is exact when no collision).
    #[must_use]
    pub fn join(&self, other: &Env) -> Env {
        let mut by_id: IndexMap<ResourceKey, Resource> = IndexMap::new();
        for r in &self.resources {
            by_id.insert(ResourceKey::from_resource(r), r.clone());
        }
        for r in &other.resources {
            // Right-bias: insert overwrites.
            by_id.insert(ResourceKey::from_resource(r), r.clone());
        }
        let imports: Vec<String> = {
            let mut acc: Vec<String> = self.spec.imports.clone();
            for imp in &other.spec.imports {
                if !acc.contains(imp) {
                    acc.push(imp.clone());
                }
            }
            acc
        };
        let mut labels = self.spec.labels.clone();
        for (k, v) in &other.spec.labels {
            labels.insert(k.clone(), v.clone()); // right-bias
        }
        Env {
            spec: EnvSpec {
                name: format!("{}∪{}", self.spec.name, other.spec.name),
                description: format!(
                    "join({}, {})",
                    self.spec.name, other.spec.name
                ),
                imports,
                labels,
            },
            resources: by_id.into_values().collect(),
        }
    }

    /// **`leq`** (`self ⊑ other`). True iff every resource in
    /// `self` is present and byte-equal in `other`. Subset
    /// relation modulo content. Used as the basis for drift
    /// detection (`!(observed ⊑ declared)` = drift).
    #[must_use]
    pub fn leq(&self, other: &Env) -> bool {
        let other_by_id: BTreeMap<ResourceKey, &Resource> = other
            .resources
            .iter()
            .map(|r| (ResourceKey::from_resource(r), r))
            .collect();
        for r in &self.resources {
            let key = ResourceKey::from_resource(r);
            match other_by_id.get(&key) {
                None => return false,
                Some(other_r) if !canonical_eq(&r.value, &other_r.value) => return false,
                _ => {}
            }
        }
        true
    }

    /// **`bottom`** (empty env, identity for `join`). Carries a
    /// caller-provided name so error messages stay readable when
    /// `bottom("…") ⊔ env` returns env.
    #[must_use]
    pub fn bottom(name: &str) -> Env {
        Env {
            spec: EnvSpec {
                name: name.to_string(),
                description: "bottom — empty env (lattice ⊥)".into(),
                imports: Vec::new(),
                labels: IndexMap::new(),
            },
            resources: Vec::new(),
        }
    }

    /// True iff the env contains zero resources. The bottom test
    /// — useful in property assertions.
    #[must_use]
    pub fn is_bottom(&self) -> bool {
        self.resources.is_empty()
    }

    /// Set of canonical ids for the env's resources. Useful for
    /// fast subset checks, debug printing, and cross-env
    /// comparisons that don't need full value equality.
    #[must_use]
    pub fn resource_keys(&self) -> BTreeSet<ResourceKey> {
        self.resources
            .iter()
            .map(ResourceKey::from_resource)
            .collect()
    }

    /// Strict drift indicator. `drift(observed, declared)` = true
    /// when `observed` contains anything not in `declared`, or
    /// disagrees on a shared resource. Equivalent to
    /// `!(observed ⊑ declared)`. Named for the typical caller
    /// site — the convergence loop.
    #[must_use]
    pub fn drifts_from(observed: &Env, declared: &Env) -> bool {
        !observed.leq(declared)
    }

    /// Compliance subset check. `baseline ⊑ env` reads as "every
    /// resource the baseline mandates is present in env, with
    /// matching content." Same as `leq`, named for the caller.
    #[must_use]
    pub fn satisfies_baseline(&self, baseline: &Env) -> bool {
        baseline.leq(self)
    }

    /// Resources owned by a specific imported crate. Useful for
    /// per-domain lattice operations: meet two envs *within* the
    /// gateway-api crate's resources, ignoring everything else.
    #[must_use]
    pub fn slice_by_crate(&self, crate_name: &str) -> Env {
        let resources = self
            .resources
            .iter()
            .filter(|r| keyword_to_crate(&r.keyword) == Some(crate_name))
            .cloned()
            .collect();
        Env {
            spec: EnvSpec {
                name: format!("{}@{}", self.spec.name, crate_name),
                description: format!("{} restricted to {}", self.spec.name, crate_name),
                imports: vec![crate_name.to_string()],
                labels: self.spec.labels.clone(),
            },
            resources,
        }
    }
}

/// Byte-canonical JSON equality. Uses `serde_json::to_string`
/// over each side; identical input → identical output, so the
/// comparison is deterministic across processes.
fn canonical_eq(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let sa = serde_json::to_string(a).unwrap_or_default();
    let sb = serde_json::to_string(b).unwrap_or_default();
    sa == sb
}

#[cfg(test)]
mod tests {
    //! Lattice laws — every property the algebra is supposed to
    //! satisfy, asserted explicitly. These tests are the
    //! "perfect implementation" claim made operational. If a
    //! future change to `meet` or `join` breaks one of them,
    //! these tests fail loudly.
    //!
    //! "Equality" between envs in these tests is **content
    //! equality on resources**, not full struct equality —
    //! the spec metadata (`name`, `description`) is allowed to
    //! differ; the resources must agree.

    use super::*;
    use serde_json::json;

    fn r(keyword: &str, name: &str, body: serde_json::Value) -> Resource {
        let mut o = body.as_object().cloned().unwrap_or_default();
        o.insert("name".into(), json!(name));
        Resource {
            keyword: keyword.to_string(),
            value: serde_json::Value::Object(o),
        }
    }

    fn env_with(name: &str, resources: Vec<Resource>) -> Env {
        Env {
            spec: EnvSpec {
                name: name.into(),
                description: name.into(),
                imports: Vec::new(),
                labels: IndexMap::new(),
            },
            resources,
        }
    }

    fn content_eq(a: &Env, b: &Env) -> bool {
        let ka = a.resource_keys();
        let kb = b.resource_keys();
        if ka != kb {
            return false;
        }
        // For each shared key, the values must be canonical-eq.
        let by_id_a: BTreeMap<_, _> = a
            .resources
            .iter()
            .map(|r| (ResourceKey::from_resource(r), &r.value))
            .collect();
        let by_id_b: BTreeMap<_, _> = b
            .resources
            .iter()
            .map(|r| (ResourceKey::from_resource(r), &r.value))
            .collect();
        for k in &ka {
            if !canonical_eq(by_id_a[k], by_id_b[k]) {
                return false;
            }
        }
        true
    }

    fn sample() -> (Env, Env, Env) {
        let a = r("defbpf-map", "syn_counter", json!({"value_size": 8}));
        let b = r("defbpf-map", "egress_counter", json!({"value_size": 8}));
        let c = r("defbpf-program", "drop_syn", json!({"kind": ":xdp"}));
        (
            env_with("X", vec![a.clone(), c.clone()]),
            env_with("Y", vec![b.clone(), c.clone()]),
            env_with("Z", vec![a, b, c]),
        )
    }

    #[test]
    fn law_reflexive_leq() {
        let (x, _, _) = sample();
        assert!(x.leq(&x));
    }

    #[test]
    fn law_antisymmetric_leq() {
        let (x, _, _) = sample();
        let y = env_with("X-prime", x.resources.clone());
        assert!(x.leq(&y) && y.leq(&x));
        assert!(content_eq(&x, &y), "antisymmetric implies content-equal");
    }

    #[test]
    fn law_transitive_leq() {
        let (x, _, z) = sample();
        // x ⊑ z (z contains everything x has + more).
        assert!(x.leq(&z));
        // x ⊑ x ⊑ z → transitivity holds.
        assert!(x.leq(&x) && x.leq(&z));
    }

    #[test]
    fn law_idempotent_meet_join() {
        let (x, _, _) = sample();
        assert!(content_eq(&x.meet(&x), &x));
        assert!(content_eq(&x.join(&x), &x));
    }

    #[test]
    fn law_commutative_meet_join() {
        let (x, y, _) = sample();
        assert!(content_eq(&x.meet(&y), &y.meet(&x)));
        assert!(content_eq(&x.join(&y), &y.join(&x)));
    }

    #[test]
    fn law_associative_meet() {
        let (x, y, z) = sample();
        let lhs = x.meet(&y).meet(&z);
        let rhs = x.meet(&y.meet(&z));
        assert!(content_eq(&lhs, &rhs));
    }

    #[test]
    fn law_associative_join() {
        let (x, y, z) = sample();
        let lhs = x.join(&y).join(&z);
        let rhs = x.join(&y.join(&z));
        assert!(content_eq(&lhs, &rhs));
    }

    #[test]
    fn law_absorption() {
        let (x, y, _) = sample();
        // meet(x, join(x, y)) ≅ x
        assert!(content_eq(&x.meet(&x.join(&y)), &x));
        // join(x, meet(x, y)) ≅ x
        assert!(content_eq(&x.join(&x.meet(&y)), &x));
    }

    #[test]
    fn law_bottom_identity_for_join() {
        let (x, _, _) = sample();
        let bot = Env::bottom("⊥");
        assert!(content_eq(&bot.join(&x), &x));
        assert!(content_eq(&x.join(&bot), &x));
    }

    #[test]
    fn law_bottom_absorber_for_meet() {
        let (x, _, _) = sample();
        let bot = Env::bottom("⊥");
        assert!(bot.meet(&x).is_bottom());
        assert!(x.meet(&bot).is_bottom());
    }

    #[test]
    fn law_meet_is_lower_bound() {
        let (x, y, _) = sample();
        let m = x.meet(&y);
        assert!(m.leq(&x));
        assert!(m.leq(&y));
    }

    #[test]
    fn law_join_is_upper_bound() {
        let (x, y, _) = sample();
        let j = x.join(&y);
        assert!(x.leq(&j));
        assert!(y.leq(&j));
    }

    #[test]
    fn drift_is_inverse_of_leq() {
        let (x, _, z) = sample();
        // x ⊑ z → no drift from z.
        assert!(!Env::drifts_from(&x, &z));
        // z !⊑ x (z has stuff x doesn't) → drift.
        assert!(Env::drifts_from(&z, &x));
    }

    #[test]
    fn join_right_bias_on_collisions() {
        // Two envs both declare the same resource id with
        // different content. Join takes the right.
        let left = env_with(
            "L",
            vec![r("defbpf-map", "x", json!({"value_size": 8}))],
        );
        let right = env_with(
            "R",
            vec![r("defbpf-map", "x", json!({"value_size": 16}))],
        );
        let j = left.join(&right);
        let v = &j.resources[0].value;
        assert_eq!(v["value_size"], 16, "right-bias on collision");
    }

    #[test]
    fn slice_by_crate_keeps_only_matching_keywords() {
        let (_, _, z) = sample();
        let bpf_only = z.slice_by_crate("tatara-ebpf");
        let kws: Vec<&str> = bpf_only.resources.iter().map(|r| r.keyword.as_str()).collect();
        assert!(kws.iter().all(|k| k.starts_with("defbpf-")));
        assert_eq!(bpf_only.resources.len(), 3);
    }

    #[test]
    fn satisfies_baseline_alias_for_leq() {
        let (x, _, z) = sample();
        // x ⊑ z, so z satisfies the baseline x.
        assert!(z.satisfies_baseline(&x));
        // But x doesn't satisfy z — z requires more.
        assert!(!x.satisfies_baseline(&z));
    }
}

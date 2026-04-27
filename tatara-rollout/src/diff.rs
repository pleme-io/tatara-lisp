//! Compute the diff between two `Env` snapshots.

use crate::fingerprint::fingerprint_env;
use crate::plan::{Change, Plan, ResourceId};
use std::collections::HashMap;
use tatara_env::compile::Env;

/// Produce a typed `Plan` describing the move from `old` to
/// `new`. Pure: identical inputs always produce identical plans.
#[must_use]
pub fn diff_envs(old: &Env, new: &Env) -> Plan {
    let old_prints = fingerprint_env(old);
    let new_prints = fingerprint_env(new);

    // Look up new resources by id for value retrieval.
    let new_by_id: HashMap<ResourceId, &tatara_env::compile::Resource> = new
        .resources
        .iter()
        .map(|r| (ResourceId::from_resource(r), r))
        .collect();

    let mut plan = Plan::default();

    for (id, fp_new) in &new_prints {
        match old_prints.get(id) {
            None => {
                plan.adds.push(Change::Add {
                    id: id.clone(),
                    value: new_by_id[id].value.clone(),
                });
            }
            Some(fp_old) if fp_old.blake3 != fp_new.blake3 => {
                plan.changes.push(Change::Change {
                    id: id.clone(),
                    old_blake3: fp_old.blake3.clone(),
                    new_blake3: fp_new.blake3.clone(),
                    new_value: new_by_id[id].value.clone(),
                });
            }
            Some(_) => {
                plan.unchanged.push(id.clone());
            }
        }
    }

    for id in old_prints.keys() {
        if !new_prints.contains_key(id) {
            plan.removes.push(Change::Remove { id: id.clone() });
        }
    }

    plan
}

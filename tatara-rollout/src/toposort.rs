//! Type-level topological sort over a `Plan`.
//!
//! Each domain registers `DEPENDS_ON: &[&'static str]` (Layer 4
//! capability — see `tatara_lisp::domain::DependentDomain`).
//! At apply time, the rollout pipeline calls `ordered_apply` to
//! get an ordering of `Change` items where:
//!
//!   1. **Adds + Changes** are sorted so dependents come AFTER
//!      their dependencies. A `defbpf-policy` (depends on
//!      `defbpf-program` + `defbpf-map`) lands after both.
//!   2. **Removes** are sorted in reverse — dependents first,
//!      dependencies last — so we don't pull a map out from
//!      under a still-running program.
//!
//! Type-level (not instance-level) — the dependency relation is
//! between keywords, not between specific resource ids. Finer
//! ordering (this `defciliumnetworkpolicy` after that
//! `defservice`) lives in a future per-instance pass that walks
//! the typed values for cross-references.

use crate::plan::{Change, Plan};
use std::collections::{HashMap, HashSet};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ToposortError {
    #[error("cycle detected involving keyword `{0}` — declarations form a loop")]
    Cycle(String),
}

/// Return every actionable change in dependency-aware order.
/// Removes go first (in reverse-topological order — dependents
/// torn down before dependencies), then Adds + Changes (in
/// forward-topological order — dependencies created before
/// dependents). Within the same toposort tier, original
/// declaration order is preserved.
pub fn ordered_apply(plan: &Plan) -> Result<Vec<Change>, ToposortError> {
    // Collect every keyword that appears in the plan.
    let mut keywords: Vec<&str> = Vec::new();
    for c in plan.iter_actionable() {
        let kw = c.id().keyword.as_str();
        if !keywords.contains(&kw) {
            keywords.push(kw);
        }
    }

    let order = topo_sort(&keywords)?;
    let kw_index: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, k)| (*k, i))
        .collect();

    // Reverse-topo for removes — torn down dependent-first.
    let mut removes = plan.removes.clone();
    removes.sort_by_key(|c| {
        std::cmp::Reverse(*kw_index.get(c.id().keyword.as_str()).unwrap_or(&usize::MAX))
    });
    // Forward-topo for adds + changes — dependencies first.
    let mut adds = plan.adds.clone();
    adds.sort_by_key(|c| *kw_index.get(c.id().keyword.as_str()).unwrap_or(&usize::MAX));
    let mut changes = plan.changes.clone();
    changes.sort_by_key(|c| *kw_index.get(c.id().keyword.as_str()).unwrap_or(&usize::MAX));

    let mut out = Vec::with_capacity(plan.actionable_count());
    out.extend(removes);
    out.extend(adds);
    out.extend(changes);
    Ok(out)
}

/// Topologically sort a slice of keywords by their registered
/// `DEPENDS_ON` edges. Output: dependencies first, dependents
/// last. Errors on cycles.
fn topo_sort<'a>(keywords: &'a [&str]) -> Result<Vec<&'a str>, ToposortError> {
    let kw_set: HashSet<&str> = keywords.iter().copied().collect();
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    for kw in keywords {
        // Pull deps from the registry, restrict to keywords
        // present in THIS plan (deps to keywords not in the
        // plan are ignored — nothing to order against).
        let edges: Vec<&str> = tatara_lisp::domain::lookup_deps(kw)
            .map(|h| {
                h.depends_on
                    .iter()
                    .copied()
                    .filter(|d| kw_set.contains(d))
                    .collect()
            })
            .unwrap_or_default();
        deps.insert(kw, edges);
    }

    // Kahn's algorithm — repeatedly remove nodes with no
    // unsatisfied deps. Cycle iff some nodes remain.
    let mut in_degree: HashMap<&str, usize> =
        keywords.iter().map(|k| (*k, 0)).collect();
    for (_kw, edges) in &deps {
        for &dep in edges {
            *in_degree.entry(dep).or_insert(0) += 0; // ensure exists
        }
        // For each (a depends on b), b → a edge. Increment a's
        // in-degree by 1 for each dep a has on b that we'll honor.
    }
    // Recompute in_degree correctly: in_degree[a] = number of
    // b's such that b ∈ keywords AND a depends on b.
    for kw in keywords {
        in_degree.insert(kw, 0);
    }
    for (kw, edges) in &deps {
        in_degree.insert(kw, edges.len());
    }

    let mut queue: Vec<&str> = keywords
        .iter()
        .filter(|k| in_degree.get(*k).copied().unwrap_or(0) == 0)
        .copied()
        .collect();
    let mut out = Vec::with_capacity(keywords.len());
    while let Some(kw) = queue.pop() {
        out.push(kw);
        // Remove kw from every other node's deps. If any node's
        // in_degree drops to 0, enqueue it.
        let kws: Vec<&str> = deps.keys().copied().collect();
        for other in kws {
            if other == kw {
                continue;
            }
            if let Some(edges) = deps.get_mut(other) {
                if let Some(pos) = edges.iter().position(|d| *d == kw) {
                    edges.remove(pos);
                    let degree = in_degree.entry(other).or_insert(0);
                    if *degree > 0 {
                        *degree -= 1;
                    }
                    if *degree == 0 && !out.contains(&other) && !queue.contains(&other) {
                        queue.push(other);
                    }
                }
            }
        }
    }

    if out.len() != keywords.len() {
        // Anything left has a non-zero in_degree → cycle.
        let stuck = keywords
            .iter()
            .find(|k| !out.contains(k))
            .copied()
            .unwrap_or("?");
        return Err(ToposortError::Cycle(stuck.to_string()));
    }

    Ok(out)
}

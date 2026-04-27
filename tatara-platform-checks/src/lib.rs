//! tatara-platform-checks — platform-wide invariant runner.
//!
//! Each `Invariant` is a typed check function over the registered
//! catalog. The runner walks them all, collecting pass/fail for
//! every keyword. No invariant assumes any specific domain — they
//! all read from the global registries directly, so adding a new
//! catalog domain inherits the full suite for free.
//!
//! Adding a new invariant is a single function + an entry in
//! `default_invariants()`. The compounding curve flattens once
//! more: new layers describe themselves; new invariants describe
//! the platform-wide properties those layers must satisfy.

use std::collections::HashMap;

/// Outcome of one invariant for one keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(String),
    Skip(String),
}

/// One platform-wide invariant. The function is given the full
/// list of registered keywords; it returns one Outcome per
/// keyword (or per-platform Outcome reported under a synthetic
/// keyword like `"<platform>"` when the check is global).
pub struct Invariant {
    pub name: &'static str,
    pub description: &'static str,
    pub run: fn(&[&'static str]) -> HashMap<String, Outcome>,
}

/// Result of running every invariant. The shape is
/// `{ invariant_name → { keyword → outcome } }`. Useful both
/// for human-readable summaries and for downstream
/// programmatic consumers.
#[derive(Debug, Default)]
pub struct CheckRun {
    pub by_invariant: HashMap<&'static str, HashMap<String, Outcome>>,
}

impl CheckRun {
    /// Total number of failures across all invariants × keywords.
    #[must_use]
    pub fn fail_count(&self) -> usize {
        self.by_invariant
            .values()
            .flat_map(|m| m.values())
            .filter(|o| matches!(o, Outcome::Fail(_)))
            .count()
    }

    /// All failing (invariant, keyword, message) triples — useful
    /// for CI failure messages.
    #[must_use]
    pub fn failures(&self) -> Vec<(&str, String, String)> {
        let mut out = Vec::new();
        for (inv, m) in &self.by_invariant {
            for (kw, oc) in m {
                if let Outcome::Fail(msg) = oc {
                    out.push((*inv, kw.clone(), msg.clone()));
                }
            }
        }
        out.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        out
    }

    /// Render a tab-aligned text report.
    #[must_use]
    pub fn report(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let mut names: Vec<&&'static str> = self.by_invariant.keys().collect();
        names.sort_unstable();
        for name in names {
            let m = &self.by_invariant[name];
            let pass = m.values().filter(|o| matches!(o, Outcome::Pass)).count();
            let fail = m.values().filter(|o| matches!(o, Outcome::Fail(_))).count();
            let skip = m.values().filter(|o| matches!(o, Outcome::Skip(_))).count();
            let _ = writeln!(
                out,
                "{name:42} pass {pass:3}  fail {fail:3}  skip {skip:3}"
            );
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_unstable();
            for k in keys {
                if let Outcome::Fail(msg) = &m[k] {
                    let _ = writeln!(out, "    FAIL  {k:30} {msg}");
                }
            }
        }
        out
    }
}

/// Run every invariant. Caller is responsible for having called
/// `register()` on every domain crate before this — the checks
/// read from the global registries.
#[must_use]
pub fn run_all(invariants: &[Invariant]) -> CheckRun {
    let kws_owned: Vec<&'static str> = tatara_lisp::domain::registered_keywords();
    let mut run = CheckRun::default();
    for inv in invariants {
        let outcomes = (inv.run)(&kws_owned);
        run.by_invariant.insert(inv.name, outcomes);
    }
    run
}

/// The canonical invariant suite shipped with this crate.
/// Adding a new check: write a fn that takes `&[&str]` of
/// keywords and returns `HashMap<String, Outcome>`, then push
/// an `Invariant { … }` entry here.
#[must_use]
pub fn default_invariants() -> Vec<Invariant> {
    vec![
        Invariant {
            name: "always-required-layers-present",
            description:
                "Every registered keyword has handlers for the 9 always-required \
                 capability layers (Compile + Doc + Deps + Validate + Lifecycle + \
                 Compliance + Observability + Help + Stability).",
            run: check_always_required_layers,
        },
        Invariant {
            name: "no-duplicate-keywords",
            description:
                "Two domains claiming the same keyword silently overwrite each \
                 other's compile handler. Detects collisions before they cause \
                 runtime mysteries.",
            run: check_no_duplicate_keywords,
        },
        Invariant {
            name: "deps-resolve-to-registered-keywords",
            description:
                "Every entry in a domain's `DEPENDS_ON` resolves to another \
                 keyword in the registry. Dangling deps mean the rollout \
                 plan's topo-sort silently ignores the constraint.",
            run: check_deps_resolve,
        },
        Invariant {
            name: "schemas-parse-as-json",
            description:
                "Every registered SCHEMA_JSON parses as a JSON object. Malformed \
                 schemas would silently fail downstream IDE / web validator \
                 consumers.",
            run: check_schemas_parse,
        },
        Invariant {
            name: "compliance-frameworks-are-known",
            description:
                "Every claimed compliance framework appears in the recognized \
                 set. Catches typos like 'NIT 800-53' that would silently \
                 miss compliance reports.",
            run: check_compliance_frameworks,
        },
        Invariant {
            name: "stability-values-are-known",
            description:
                "Every STABILITY value is one of {experimental, alpha, beta, \
                 stable, deprecated}. Catches typos that would skew CI gates.",
            run: check_stability_values,
        },
    ]
}

// ── Concrete invariants ───────────────────────────────────────────

fn check_always_required_layers(keywords: &[&'static str]) -> HashMap<String, Outcome> {
    keywords
        .iter()
        .map(|kw| {
            let mut missing: Vec<&'static str> = Vec::new();
            if tatara_lisp::domain::lookup_doc(kw).is_none() {
                missing.push("Doc");
            }
            if tatara_lisp::domain::lookup_deps(kw).is_none() {
                missing.push("Deps");
            }
            if tatara_lisp::domain::lookup_validate(kw).is_none() {
                missing.push("Validate");
            }
            if tatara_lisp::domain::lookup_lifecycle(kw).is_none() {
                missing.push("Lifecycle");
            }
            if tatara_lisp::domain::lookup_compliance(kw).is_none() {
                missing.push("Compliance");
            }
            if tatara_lisp::domain::lookup_observability(kw).is_none() {
                missing.push("Observability");
            }
            if tatara_lisp::domain::lookup_help(kw).is_none() {
                missing.push("Help");
            }
            if tatara_lisp::domain::lookup_stability(kw).is_none() {
                missing.push("Stability");
            }
            let outcome = if missing.is_empty() {
                Outcome::Pass
            } else {
                Outcome::Fail(format!("missing layers: {}", missing.join(", ")))
            };
            (kw.to_string(), outcome)
        })
        .collect()
}

fn check_no_duplicate_keywords(keywords: &[&'static str]) -> HashMap<String, Outcome> {
    // Registry stores one entry per keyword by construction (HashMap key).
    // The check here is meaningful when run against a synthetic union —
    // for the live registry it's always Pass per keyword, but we still
    // surface the per-keyword result for uniformity.
    let mut seen = HashMap::new();
    let mut out = HashMap::new();
    for kw in keywords {
        let count = seen.entry(*kw).or_insert(0);
        *count += 1;
    }
    for kw in keywords {
        let outcome = if seen[kw] > 1 {
            Outcome::Fail(format!("duplicate registration ({} occurrences)", seen[kw]))
        } else {
            Outcome::Pass
        };
        out.insert(kw.to_string(), outcome);
    }
    out
}

fn check_deps_resolve(keywords: &[&'static str]) -> HashMap<String, Outcome> {
    let kw_set: std::collections::HashSet<&'static str> = keywords.iter().copied().collect();
    keywords
        .iter()
        .map(|kw| {
            let outcome = match tatara_lisp::domain::lookup_deps(kw) {
                None => Outcome::Skip("no deps registered".into()),
                Some(d) => {
                    let dangling: Vec<&'static str> = d
                        .depends_on
                        .iter()
                        .copied()
                        .filter(|d| !kw_set.contains(d))
                        .collect();
                    if dangling.is_empty() {
                        Outcome::Pass
                    } else {
                        Outcome::Fail(format!(
                            "dangling deps: {}",
                            dangling.join(", ")
                        ))
                    }
                }
            };
            (kw.to_string(), outcome)
        })
        .collect()
}

fn check_schemas_parse(keywords: &[&'static str]) -> HashMap<String, Outcome> {
    keywords
        .iter()
        .map(|kw| {
            let outcome = match tatara_lisp::domain::lookup_schema(kw) {
                None => Outcome::Skip("no schema registered".into()),
                Some(s) => match serde_json::from_str::<serde_json::Value>(s.schema_json) {
                    Ok(v) if v.is_object() => Outcome::Pass,
                    Ok(_) => Outcome::Fail("schema parsed but is not an object".into()),
                    Err(e) => Outcome::Fail(format!("parse error: {e}")),
                },
            };
            (kw.to_string(), outcome)
        })
        .collect()
}

const KNOWN_COMPLIANCE_FRAMEWORKS: &[&str] = &[
    "NIST 800-53",
    "NIST CSF",
    "CIS",
    "FedRAMP",
    "PCI DSS 4.0",
    "SOC 2",
    "HIPAA",
    "ISO 27001",
];

fn check_compliance_frameworks(keywords: &[&'static str]) -> HashMap<String, Outcome> {
    keywords
        .iter()
        .map(|kw| {
            let outcome = match tatara_lisp::domain::lookup_compliance(kw) {
                None => Outcome::Skip("no compliance registered".into()),
                Some(c) if c.frameworks.is_empty() => Outcome::Skip("no frameworks claimed".into()),
                Some(c) => {
                    let unknown: Vec<&'static str> = c
                        .frameworks
                        .iter()
                        .copied()
                        .filter(|f| !KNOWN_COMPLIANCE_FRAMEWORKS.contains(f))
                        .collect();
                    if unknown.is_empty() {
                        Outcome::Pass
                    } else {
                        Outcome::Fail(format!("unknown frameworks: {}", unknown.join(", ")))
                    }
                }
            };
            (kw.to_string(), outcome)
        })
        .collect()
}

const KNOWN_STABILITY_VALUES: &[&str] = &["experimental", "alpha", "beta", "stable", "deprecated"];

fn check_stability_values(keywords: &[&'static str]) -> HashMap<String, Outcome> {
    keywords
        .iter()
        .map(|kw| {
            let outcome = match tatara_lisp::domain::lookup_stability(kw) {
                None => Outcome::Skip("no stability registered".into()),
                Some(s) if KNOWN_STABILITY_VALUES.contains(&s.stability) => Outcome::Pass,
                Some(s) => {
                    Outcome::Fail(format!("unknown stability `{}`", s.stability))
                }
            };
            (kw.to_string(), outcome)
        })
        .collect()
}

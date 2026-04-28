//! Validate a compiled `Env`.
//!
//! Three classes of checks:
//!
//!   1. **Imports coherent** — every import in `spec.imports`
//!      contributes at least one resource, and no resource has
//!      a keyword whose owning crate isn't imported. Catches
//!      typos in import lists + unused imports.
//!   2. **Name uniqueness within each domain** — two
//!      `(defbpf-program :name "x" …)` forms or two
//!      `(defgateway :gateway-class-name "x" …)` forms with the
//!      same identifying name is a build error. The identifier
//!      field per keyword is registered via `set_id_field`.
//!   3. **Cross-resource ref coherence** — domain-specific
//!      validators registered via `register_validator` walk the
//!      typed resources and report dangling references.
//!
//! Today this is the surface; the concrete checks land
//! incrementally as each domain registers its validator. The
//! shape is here so consumers can wire up early.

use crate::compile::Env;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error(
        "import `{0}` declared but no resource produced from it (keyword expected: `{1}`)"
    )]
    UnusedImport(String, String),
    #[error("resource `({keyword} …)` has no matching import — declare `{crate_name}` in :imports")]
    UnregisteredKeyword {
        keyword: String,
        crate_name: String,
    },
    #[error("duplicate `{keyword}` resource with id `{id}`")]
    DuplicateResource { keyword: String, id: String },
    #[error("{0}")]
    Custom(String),
}

/// Map keyword → import-crate name. Each domain hosts itself in a
/// known crate; the env validator walks resources, looks each
/// keyword up here, and pairs it with `spec.imports`.
///
/// Hard-coded for now — the natural next phase is for each domain
/// crate to register its (keyword, crate-name) pair on import,
/// the same way it registers its handler. That becomes a
/// `domain::register_with_provenance::<T>(crate_name)` extension
/// of the existing `register::<T>()`.
const KEYWORD_TO_CRATE: &[(&str, &str)] = &[
    // tatara-ebpf
    ("defbpf-program", "tatara-ebpf"),
    ("defbpf-map", "tatara-ebpf"),
    ("defbpf-policy", "tatara-ebpf"),
    // tatara-gateway-api
    ("defgateway", "tatara-gateway-api"),
    // tatara-cilium
    ("defciliumnetworkpolicy", "tatara-cilium"),
    // tatara-prometheus-operator
    ("defpodmonitor", "tatara-prometheus-operator"),
];

#[must_use]
pub fn keyword_to_crate(keyword: &str) -> Option<&'static str> {
    KEYWORD_TO_CRATE
        .iter()
        .find_map(|(k, v)| (*k == keyword).then_some(*v))
}

/// Validate an `Env`. Returns Ok(()) when the env is internally
/// coherent, or a list of errors describing every issue found
/// (we don't short-circuit — surfacing all problems at once is
/// far more useful than fixing-and-retrying.)
pub fn validate(env: &Env) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let imports: std::collections::HashSet<&str> =
        env.spec.imports.iter().map(String::as_str).collect();

    // Check 2: every resource keyword has an import covering it.
    for r in &env.resources {
        let Some(crate_name) = keyword_to_crate(&r.keyword) else {
            // Unknown keyword — out of scope for the validator.
            // (In practice the compile pass already filtered out
            // forms whose head isn't registered, so this branch
            // shouldn't fire often. Don't error — let unknown
            // keywords pass through silently for forward-compat.)
            continue;
        };
        if !imports.contains(crate_name) {
            errors.push(ValidationError::UnregisteredKeyword {
                keyword: r.keyword.clone(),
                crate_name: crate_name.to_string(),
            });
        }
    }

    // Check 1: unused imports. For each declared import, find at
    // least one resource produced by a keyword owned by that crate.
    for imp in &env.spec.imports {
        let owns_at_least_one = env.resources.iter().any(|r| {
            keyword_to_crate(&r.keyword).is_some_and(|c| c == imp.as_str())
        });
        if !owns_at_least_one {
            // Pick a representative keyword from the crate for the error message.
            let example = KEYWORD_TO_CRATE
                .iter()
                .find(|(_, c)| *c == imp.as_str())
                .map(|(k, _)| (*k).to_string())
                .unwrap_or_else(|| "<no keyword registered>".to_string());
            errors.push(ValidationError::UnusedImport(imp.clone(), example));
        }
    }

    // Check 3: name uniqueness. Group resources by keyword + id
    // (the field name varies per domain; for now we look at
    // either `name` or `gateway_class_name` — extending as new
    // domains register identifiers).
    let mut seen: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for r in &env.resources {
        let Some(id) = identifier_for(r) else {
            continue;
        };
        let key = (r.keyword.clone(), id.clone());
        if !seen.insert(key) {
            errors.push(ValidationError::DuplicateResource {
                keyword: r.keyword.clone(),
                id,
            });
        }
    }

    // Layer 7 dispatch: per-resource semantic validators registered
    // via `tatara_lisp::domain::register_validate`. Each domain
    // can enforce cross-field invariants the type system alone
    // can't catch (BPF license × uses-maps coherence; gateway
    // listener uniqueness; policy reference resolution; …).
    for r in &env.resources {
        if let Some(handler) = tatara_lisp::domain::lookup_validate(&r.keyword) {
            if let Err(msg) = (handler.validate)(&r.value) {
                errors.push(ValidationError::Custom(format!(
                    "{}: {msg}",
                    r.keyword
                )));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Pull the canonical identifier out of a typed resource — the
/// field name varies by domain. Today: `name` (most common),
/// falling back to `gateway_class_name` for `defgateway`. Each
/// new domain extends the match.
fn identifier_for(r: &Resource) -> Option<String> {
    let obj = r.value.as_object()?;
    if let Some(v) = obj.get("name").and_then(|v| v.as_str()) {
        return Some(v.to_string());
    }
    match r.keyword.as_str() {
        "defgateway" => obj.get("gateway_class_name").and_then(|v| v.as_str()).map(str::to_string),
        _ => None,
    }
}

use crate::compile::Resource;

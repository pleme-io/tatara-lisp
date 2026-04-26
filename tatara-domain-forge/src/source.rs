//! Source ingestors — turn typed inputs into the IR.
//!
//! Currently supported: Kubernetes CRD YAML. The CRD's
//! `spec.versions[*].schema.openAPIV3Schema` is a JSON-Schema
//! fragment we walk recursively and lower into our IR. Every CRD
//! kind in a multi-doc bundle becomes a `Resource`.
//!
//! OpenAPI 3.0 and TOML inputs land here next, sharing the IR.

use crate::ir::{Domain, DomainKind, Field, FieldType, Resource, ScalarType};
use indexmap::IndexMap;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FromCrdError {
    #[error("yaml parse: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported schema at `{path}`: {message}")]
    Unsupported { path: String, message: String },
    #[error("CRD missing `spec.names.kind`")]
    MissingKind,
    #[error("CRD missing `spec.versions[*].schema.openAPIV3Schema`")]
    MissingSchema,
    #[error("expected at least one CRD in input — got zero")]
    Empty,
}

/// Read a CRD bundle from a file path. Multi-doc YAML is accepted —
/// every doc that looks like a `CustomResourceDefinition` becomes a
/// resource in the resulting domain. Non-CRD docs (e.g. namespaces,
/// service-accounts that get bundled with the CRDs) are ignored.
pub fn from_crd_yaml(path: &std::path::Path, domain_name: &str) -> Result<Domain, FromCrdError> {
    let bytes = std::fs::read_to_string(path)?;
    from_crd_str(&bytes, domain_name)
}

/// In-memory variant — useful for tests + downstream pipelines that
/// already have the YAML in hand.
pub fn from_crd_str(yaml: &str, domain_name: &str) -> Result<Domain, FromCrdError> {
    let mut resources = Vec::new();
    for doc in serde_yaml_ng::Deserializer::from_str(yaml) {
        let value: serde_yaml_ng::Value = serde_yaml_ng::Value::deserialize(doc)?;
        if !looks_like_crd(&value) {
            continue;
        }
        resources.push(crd_to_resource(&value)?);
    }
    if resources.is_empty() {
        return Err(FromCrdError::Empty);
    }
    Ok(Domain {
        name: domain_name.to_string(),
        description: format!("Tatara domain wrapping {} CRD(s).", resources.len()),
        kind: DomainKind::Kubernetes,
        resources,
    })
}

fn looks_like_crd(v: &serde_yaml_ng::Value) -> bool {
    v.get("kind").and_then(|k| k.as_str()) == Some("CustomResourceDefinition")
}

fn crd_to_resource(v: &serde_yaml_ng::Value) -> Result<Resource, FromCrdError> {
    let names = v
        .get("spec")
        .and_then(|s| s.get("names"))
        .ok_or(FromCrdError::MissingKind)?;
    let kind = names
        .get("kind")
        .and_then(|k| k.as_str())
        .ok_or(FromCrdError::MissingKind)?;
    // Pick the first served+stored version that has a schema. Most
    // CRDs only have one served version; this picks deterministically
    // for the multi-version case.
    let versions = v
        .get("spec")
        .and_then(|s| s.get("versions"))
        .and_then(|x| x.as_sequence())
        .ok_or(FromCrdError::MissingSchema)?;
    let schema = versions
        .iter()
        .find_map(|ver| {
            ver.get("schema")
                .and_then(|s| s.get("openAPIV3Schema"))
        })
        .ok_or(FromCrdError::MissingSchema)?;
    // The K8s CRD root schema typically has a `spec` sub-property —
    // that's the user-facing payload. We forge the Rust struct from
    // it. If the CRD has no `.spec` (rare), fall back to the root.
    let spec_schema = schema
        .get("properties")
        .and_then(|p| p.get("spec"))
        .unwrap_or(schema);
    let struct_name = format!("{kind}Spec");
    let keyword = Resource::default_keyword(&struct_name);
    let fields = lower_object(spec_schema, &kind.to_string())?;
    let doc = schema
        .get("description")
        .and_then(|d| d.as_str())
        .map(str::to_string);
    Ok(Resource {
        struct_name,
        keyword,
        doc,
        fields,
    })
}

/// Walk an OpenAPI v3 object schema → `IndexMap<rust_name, Field>`.
/// Top-level entry point — also called recursively for nested
/// `object` schemas.
fn lower_object(
    schema: &serde_yaml_ng::Value,
    name_prefix: &str,
) -> Result<IndexMap<String, Field>, FromCrdError> {
    let required: std::collections::HashSet<String> = schema
        .get("required")
        .and_then(|r| r.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut out = IndexMap::new();
    let Some(props) = schema.get("properties").and_then(|p| p.as_mapping()) else {
        return Ok(out);
    };
    for (k, v) in props {
        let key = match k.as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let rust_name = json_key_to_rust(&key);
        let ty = lower_type(v, &format!("{name_prefix}_{rust_name}"))?;
        let doc = v
            .get("description")
            .and_then(|d| d.as_str())
            .map(str::to_string);
        let req = required.contains(&key);
        out.insert(
            rust_name.clone(),
            Field {
                rust_name,
                ty,
                doc,
                required: req,
            },
        );
    }
    Ok(out)
}

/// Lower one schema fragment to a `FieldType`. Recursive — nested
/// objects lower to `Nested`, arrays to `List`, etc.
fn lower_type(
    schema: &serde_yaml_ng::Value,
    nested_name_seed: &str,
) -> Result<FieldType, FromCrdError> {
    // Honor `x-kubernetes-preserve-unknown-fields` — these are
    // free-form payloads we represent as `serde_json::Value`.
    if schema
        .get("x-kubernetes-preserve-unknown-fields")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return Ok(FieldType::Untyped);
    }
    let ty_str = schema.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match ty_str {
        "string" => {
            // Enum-of-strings? Lower to Rust enum.
            if let Some(seq) = schema.get("enum").and_then(|e| e.as_sequence()) {
                let variants: Vec<String> = seq
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect();
                if !variants.is_empty() {
                    return Ok(FieldType::Enum {
                        type_name: pascal(&format!("{nested_name_seed}_kind")),
                        variants,
                    });
                }
            }
            Ok(FieldType::Scalar(ScalarType::String))
        }
        "boolean" => Ok(FieldType::Scalar(ScalarType::Bool)),
        "integer" => Ok(FieldType::Scalar(ScalarType::I64)),
        "number" => Ok(FieldType::Scalar(ScalarType::F64)),
        "array" => {
            let items = schema.get("items").ok_or_else(|| FromCrdError::Unsupported {
                path: nested_name_seed.to_string(),
                message: "array missing `items`".into(),
            })?;
            let inner = lower_type(items, &format!("{nested_name_seed}_item"))?;
            Ok(FieldType::List(Box::new(inner)))
        }
        "object" => {
            // additionalProperties → Map<String, V>
            if let Some(ap) = schema.get("additionalProperties") {
                if let Some(b) = ap.as_bool() {
                    if b {
                        return Ok(FieldType::Untyped);
                    }
                } else {
                    let inner = lower_type(ap, &format!("{nested_name_seed}_value"))?;
                    return Ok(FieldType::Map(Box::new(inner)));
                }
            }
            // properties → nested struct
            if schema.get("properties").is_some() {
                let fields = lower_object(schema, nested_name_seed)?;
                return Ok(FieldType::Nested {
                    struct_name: pascal(nested_name_seed),
                    fields,
                });
            }
            // Bare object with no schema — escape hatch.
            Ok(FieldType::Untyped)
        }
        "" => {
            // No `type` → either x-kubernetes-int-or-string, or a
            // free-form union. Treat as untyped.
            Ok(FieldType::Untyped)
        }
        other => Err(FromCrdError::Unsupported {
            path: nested_name_seed.to_string(),
            message: format!("unknown OpenAPI type `{other}`"),
        }),
    }
}

/// JSON keys often arrive in camelCase; Rust wants snake_case.
/// Conservative: lowercase + insert `_` before each non-leading
/// uppercase rune. Leading underscores stripped (rare anyway).
/// Rust reserved words are emitted as raw identifiers (`type` →
/// `r#type`, `match` → `r#match`) so the wire form survives
/// round-trips — `r#type` as a field name serializes as `"type"`
/// in serde, which preserves the source schema exactly.
fn json_key_to_rust(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else if c == '-' || c == '.' || c == '/' {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    let trimmed = out.trim_start_matches('_').to_string();
    if is_rust_keyword(&trimmed) {
        format!("r#{trimmed}")
    } else {
        trimmed
    }
}

/// Rust strict + reserved keywords that can't be bare identifiers.
/// Source: <https://doc.rust-lang.org/reference/keywords.html>.
fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "do"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "final"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "override"
            | "priv"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "try"
            | "type"
            | "typeof"
            | "union"
            | "unsafe"
            | "unsized"
            | "use"
            | "virtual"
            | "where"
            | "while"
            | "yield"
    )
}

/// Snake-or-mixed → PascalCase. Used to mint nested struct names
/// from prefix seeds.
fn pascal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            up = true;
            continue;
        }
        if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_CRD: &str = r#"
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: monitors.test.io
spec:
  group: test.io
  names:
    kind: Monitor
    plural: monitors
  versions:
  - name: v1
    served: true
    storage: true
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            required: [name, query]
            properties:
              name: {type: string}
              query: {type: string}
              threshold: {type: number}
              window_seconds: {type: integer}
              enabled: {type: boolean}
              labels:
                type: object
                additionalProperties: {type: string}
              severity:
                type: string
                enum: [info, warn, critical]
"#;

    #[test]
    fn parses_simple_crd_into_one_resource() {
        let domain = from_crd_str(TINY_CRD, "tatara-test-monitors").unwrap();
        assert_eq!(domain.resources.len(), 1);
        assert_eq!(domain.kind, DomainKind::Kubernetes);
        let r = &domain.resources[0];
        assert_eq!(r.struct_name, "MonitorSpec");
        assert_eq!(r.keyword, "defmonitor");
        // Required fields land first because the source schema lists
        // them in declaration order, and IndexMap preserves that.
        let names: Vec<&str> = r.fields.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            vec!["name", "query", "threshold", "window_seconds", "enabled", "labels", "severity"]
        );
        assert!(r.fields["name"].required);
        assert!(!r.fields["threshold"].required);
        // labels is a Map<String, String>.
        assert!(matches!(r.fields["labels"].ty, FieldType::Map(_)));
        // severity is an enum.
        assert!(matches!(r.fields["severity"].ty, FieldType::Enum { .. }));
    }

    #[test]
    fn json_key_to_rust_handles_camel_and_dashes() {
        assert_eq!(json_key_to_rust("camelCaseField"), "camel_case_field");
        assert_eq!(json_key_to_rust("kebab-name"), "kebab_name");
        assert_eq!(json_key_to_rust("simple"), "simple");
        assert_eq!(json_key_to_rust("HostIP"), "host_i_p");
    }

    #[test]
    fn pascal_case_collapses_separators() {
        assert_eq!(pascal("monitor_severity"), "MonitorSeverity");
        assert_eq!(pascal("foo-bar-baz"), "FooBarBaz");
    }

    #[test]
    fn empty_crd_input_errors_loudly() {
        let err = from_crd_str("", "tatara-empty").unwrap_err();
        assert!(matches!(err, FromCrdError::Empty));
    }

    #[test]
    fn non_crd_docs_are_ignored() {
        let mixed = format!(
            "{}\n---\napiVersion: v1\nkind: Namespace\nmetadata:\n  name: x\n",
            TINY_CRD
        );
        let domain = from_crd_str(&mixed, "tatara-test").unwrap();
        assert_eq!(domain.resources.len(), 1, "Namespace skipped, only Monitor lifted");
    }
}

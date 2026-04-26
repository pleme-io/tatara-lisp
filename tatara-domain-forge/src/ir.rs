//! Typed IR — the platform-independent shape every input lowers into,
//! and the platform-independent shape every emit reads from.
//!
//! A `Domain` is a crate's worth of resources: one or more `Resource`s
//! that each become a `#[derive(TataraDomain)]` struct + a Lisp keyword
//! form. A `Resource` is a struct: a name, a keyword form, and a list
//! of fields. A `Field` carries a Rust type the emitter can render.
//!
//! Keep this IR as a *shape* description, not a behavior description —
//! validation and actual schema semantics live upstream (in the parser
//! that produced the IR) or downstream (in the runtime / module that
//! consumes the registered keyword). The IR's job is to be a faithful
//! middle-form so emit + source are decoupled.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// A complete domain crate's worth of typed resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domain {
    /// Crate name — by convention `tatara-{kind}`. Becomes the
    /// `[package].name` in the emitted Cargo.toml.
    pub name: String,
    /// Short human-readable description for the crate manifest.
    pub description: String,
    /// What kind of upstream this domain wraps. Drives some emission
    /// choices (e.g. K8s domains pull `kube` as a dependency, OpenAPI
    /// pull `serde_json`, TOML pull only the basics).
    pub kind: DomainKind,
    /// One entry per typed resource the domain exposes.
    pub resources: Vec<Resource>,
}

/// The provenance + flavor of the domain. Drives dependency choices
/// in the emitted `Cargo.toml` and import lines in the emitted
/// `lib.rs` — not behavior. K8s domains additionally need `kube` to
/// be useful at runtime; OpenAPI domains might want `reqwest`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DomainKind {
    /// Wraps a Kubernetes CustomResourceDefinition. Each `Resource`
    /// here corresponds to one CRD `kind`.
    Kubernetes,
    /// Wraps an OpenAPI 3.0 `components.schemas` entry — a typed
    /// payload but not necessarily a K8s CRD.
    OpenApi,
    /// Hand-authored TOML resources — a fully manual escape hatch.
    Hand,
}

/// One typed resource — becomes one struct + one keyword form in the
/// emitted Lisp surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    /// Rust struct name, PascalCase, ends in `Spec` by convention.
    /// `(defmonitor …)` ↔ `MonitorSpec`.
    pub struct_name: String,
    /// Lisp keyword form, kebab-case. Conventionally `def<thing>`.
    pub keyword: String,
    /// Optional documentation comment — emitted as a /// doc on the
    /// generated struct.
    pub doc: Option<String>,
    /// Fields, in declaration order. `IndexMap` preserves stable
    /// emission ordering across runs.
    pub fields: IndexMap<String, Field>,
}

/// One typed field of a resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    /// Snake-cased Rust field name (no leading underscores).
    pub rust_name: String,
    /// Type — either a scalar or a structural shape.
    pub ty: FieldType,
    /// Optional documentation comment.
    pub doc: Option<String>,
    /// `true` if the source schema marks this field as required.
    /// Wraps the emitted Rust type in `Option<…>` if false (with
    /// `#[serde(default)]` for cleaner defaults at deserialization).
    pub required: bool,
}

/// Type of a single field. Recursive — Object/Vec/Map nest other
/// FieldTypes. The emit pass turns these into Rust syntax.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    Scalar(ScalarType),
    /// `Vec<inner>`.
    List(Box<FieldType>),
    /// `HashMap<String, inner>` — typical for K8s labels, env, etc.
    Map(Box<FieldType>),
    /// Nested struct emitted alongside its parent. The inner Vec is
    /// the field list of the nested struct, in declaration order.
    Nested {
        /// Generated nested struct name (PascalCase, parent-prefixed
        /// to avoid sibling collisions).
        struct_name: String,
        fields: IndexMap<String, Field>,
    },
    /// String enum — emit a `#[derive(Serialize, Deserialize)]` enum
    /// with one variant per allowed value. Variants are PascalCase
    /// of the source value with `#[serde(rename = "…")]` to preserve
    /// the wire form.
    Enum {
        /// Generated enum name.
        type_name: String,
        /// Allowed string values, in source order.
        variants: Vec<String>,
    },
    /// `serde_json::Value` — escape hatch for unschematized payloads.
    /// Use sparingly; the whole point of the typed boundary is to
    /// avoid this. Emitted when the source schema can't be
    /// destructured (e.g. `additionalProperties: true` on a free-form
    /// object).
    Untyped,
}

/// Primitive scalar types. Maps directly to Rust primitives the
/// `#[derive(TataraDomain)]` macro understands natively.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ScalarType {
    String,
    Bool,
    I64,
    F64,
}

impl ScalarType {
    /// Render to a Rust type string. Used by the emitter when
    /// stringifying a field's declaration.
    #[must_use]
    pub fn rust_str(self) -> &'static str {
        match self {
            Self::String => "String",
            Self::Bool => "bool",
            Self::I64 => "i64",
            Self::F64 => "f64",
        }
    }
}

impl Resource {
    /// Default keyword for a Resource, derived from struct name —
    /// `MonitorSpec` → `defmonitor`. Mirrors the `default_keyword`
    /// fallback in `tatara-lisp-derive`.
    #[must_use]
    pub fn default_keyword(struct_name: &str) -> String {
        let stripped = struct_name.strip_suffix("Spec").unwrap_or(struct_name);
        let mut out = String::from("def");
        for c in stripped.chars() {
            if c.is_uppercase() {
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }
}

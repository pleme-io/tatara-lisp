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
    /// Named types referenced by [`FieldType::Ref`], keyed by the name the
    /// emitter will give them.
    ///
    /// **Empty for every source that inlines its shapes** — the CRD path
    /// lowers nested objects straight into [`FieldType::Nested`] and never
    /// touches this, so it stays byte-identical. It exists for schema
    /// *graphs*: a document whose definitions are shared and mutually
    /// referential rather than a tree. Vector's `generate-schema` output is
    /// the motivating case — 351 definitions with heavy reuse, where inlining
    /// would duplicate `TlsConfig`/`BatchConfig`/`RequestConfig` into a
    /// hundred call sites and produce a crate nobody can compile.
    ///
    /// An inline tree is the special case of a graph with no sharing, which
    /// is why one IR can serve both rather than two forges existing.
    #[serde(default)]
    pub types: IndexMap<String, NamedType>,
}

/// A type that is emitted once, at module scope, and referred to by name.
///
/// Distinct from [`FieldType`], which describes a type *in field position*.
/// The split is what lets a `$ref` graph lower without duplication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NamedType {
    /// A plain struct.
    Struct {
        doc: Option<String>,
        fields: IndexMap<String, Field>,
    },
    /// A closed set of string values.
    Enum {
        doc: Option<String>,
        variants: Vec<String>,
    },
    /// A sum type.
    ///
    /// `tag` carries the wire field that discriminates the variants
    /// (`Some("type")` → `#[serde(tag = "type")]`); `None` means the source
    /// gave no discriminator and the emitter must use `#[serde(untagged)]`.
    /// The distinction is load-bearing rather than cosmetic: an untagged
    /// union deserializes by trial and can silently pick the wrong arm, so
    /// a source that *does* carry a discriminator must never be lowered to
    /// `None` for convenience.
    Union {
        doc: Option<String>,
        tag: Option<String>,
        variants: Vec<UnionVariant>,
    },
    /// A single-field wrapper around another type.
    ///
    /// Exists so a schema can say "this `String` is not any old string" —
    /// e.g. a Vector template or a VRL program — and have that survive into
    /// the Rust types instead of being flattened back to `String`.
    Newtype {
        doc: Option<String>,
        inner: FieldType,
    },
}

/// One arm of a [`NamedType::Union`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnionVariant {
    /// Rust variant name, PascalCase.
    pub name: String,
    /// The wire value of the parent's tag field for this arm (`"file"`,
    /// `"kubernetes_logs"`). `None` when the union is untagged.
    pub tag_value: Option<String>,
    /// The payload this arm carries, or `None` for a **unit variant**.
    ///
    /// A tagged union routinely mixes the two — `codec: "bytes"` carries no
    /// options at all while `codec: "json"` carries a config struct — and the
    /// distinction has to survive into Rust, because `Bytes` and
    /// `Bytes(EmptyStruct)` do not serialize the same way. Modelling every
    /// arm as carrying a payload forces an empty struct that then appears on
    /// the wire.
    pub ty: Option<FieldType>,
    pub doc: Option<String>,
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
    /// Kubernetes apiVersion, e.g. `gateway.networking.k8s.io/v1`.
    /// Lifted from the CRD's `spec.group` + the served version's
    /// `name`. None for non-CRD sources (OpenAPI, hand-authored
    /// TOML); the emitter skips the `RenderableDomain` impl when
    /// metadata isn't available.
    #[serde(default)]
    pub api_version: Option<String>,
    /// Kubernetes kind, e.g. `Gateway`. Lifted from the CRD's
    /// `spec.names.kind`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Field name (in the generated struct, snake_case) that
    /// supplies the CR's `metadata.name`. Most domains use
    /// `name`; gateway-api uses `gateway_class_name`. The forge
    /// auto-detects by looking at the CRD's required-fields list
    /// and falling back to `"name"` if a `name` field exists.
    #[serde(default)]
    pub name_field: Option<String>,
    /// Source JSON Schema preserved verbatim — the CRD's
    /// `spec.versions[*].schema.openAPIV3Schema`, normalized to
    /// JSON. Embedded as `SchematicDomain::SCHEMA_JSON` in the
    /// emitted impl. None for non-CRD sources where no schema
    /// exists at ingestion time.
    #[serde(default)]
    pub raw_schema: Option<serde_json::Value>,
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
    /// Reference to a [`Domain::types`] entry by key.
    ///
    /// The graph half of the IR. A source that inlines everything never
    /// produces one; a source lowering a `$ref` document produces little
    /// else.
    Ref(String),
    /// `serde_json::Value` — escape hatch for unschematized payloads.
    /// Use sparingly; the whole point of the typed boundary is to
    /// avoid this. Emitted when the source schema can't be
    /// destructured (e.g. `additionalProperties: true` on a free-form
    /// object).
    ///
    /// ★ Sources may refuse to produce this. `Strictness::Strict` turns
    /// every would-be `Untyped` into a hard error, because "we could not
    /// type this, so here is a `Value`" is indistinguishable at the call
    /// site from "this is genuinely free-form" — and the first is a gap
    /// that should stop the build.
    Untyped,
}

/// Whether a source may fall back to [`FieldType::Untyped`].
///
/// The CRD path is [`Strictness::Permissive`]: `x-kubernetes-preserve-unknown-fields`
/// is a legitimate, deliberate "this really is free-form" marker, and CRDs use
/// it constantly. A generated *component catalog* is the opposite case — every
/// hole is a component whose options silently stop being checked — so those
/// sources pass [`Strictness::Strict`] and get an error instead.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Strictness {
    /// `Untyped` is allowed wherever the source decides it is appropriate.
    #[default]
    Permissive,
    /// Any construct that would lower to `Untyped` is an error naming its
    /// path in the source document.
    Strict,
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
    /// Unsigned. JSON Schema's `type: integer` cannot express signedness,
    /// so this is only reachable from a source that carries the width out
    /// of band — e.g. Vector's `_metadata.docs::numeric_type`, where 291 of
    /// 357 numeric fields are `uint` and lowering them all to `i64` would
    /// let a negative `batch.max_events` deserialize cleanly and fail in
    /// the executor instead of at the parse boundary.
    U64,
    /// Narrow unsigned, same provenance as [`ScalarType::U64`].
    U32,
    /// Narrow signed, same provenance as [`ScalarType::U64`].
    I32,
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
            Self::U64 => "u64",
            Self::U32 => "u32",
            Self::I32 => "i32",
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

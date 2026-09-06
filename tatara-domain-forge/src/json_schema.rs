//! JSON-Schema front door — lower a `$ref` **graph** into the IR.
//!
//! The sibling of [`crate::source`], which lowers a Kubernetes CRD. The two
//! differ in shape rather than in goal, and that difference is the reason this
//! is a separate ingestor instead of a flag on the old one:
//!
//! | | CRD (`source.rs`) | JSON Schema (here) |
//! |---|---|---|
//! | topology | a **tree** — one root per kind, nothing shared | a **graph** — definitions reference each other |
//! | reuse | none; every nested object is inlined | heavy; one `TlsConfig` has ~100 referents |
//! | unions | absent | the whole point (`oneOf` of components) |
//! | free-form | legitimate (`x-kubernetes-preserve-unknown-fields`) | a gap that must fail the build |
//!
//! Inlining a graph duplicates every shared definition at every referent and
//! produces a crate nobody can compile, so this ingestor emits
//! [`FieldType::Ref`] into [`Domain::types`]. An inline tree is the special
//! case of a graph with no sharing, which is why one IR serves both.
//!
//! # The motivating document
//!
//! `vector generate-schema`. Measured against vector 0.52.0 (2026-08-17):
//! 1.5 MB, **351 definitions**, **129 components** (63 sink / 49 source /
//! 17 transform). Three traps in that document, each handled below and each
//! costly to rediscover:
//!
//! 1. It declares `$schema: draft/2019-09` but puts definitions under
//!    **`definitions`** with `$ref: "#/definitions/…"` — a draft-07 keyword —
//!    for schemars back-compatibility. A 2019-09 reader looking for `$defs`
//!    finds nothing and concludes the document is empty.
//! 2. The component discriminator is **not** a `discriminator` keyword and not
//!    an `enum`. Each variant is `allOf: [{$ref: Config}, {properties: {type:
//!    {const: "file"}}, required: ["type"]}]` — the tag is a `const` buried in
//!    the second `allOf` branch. Off-the-shelf generators produce
//!    `serde_json::Value` soup here.
//! 3. Type information that JSON Schema cannot express is carried in a
//!    `_metadata` bag: `docs::numeric_type` (signedness — 291 `uint` vs 5
//!    `int`), `docs::templateable` (which `String`s are interpolated), and
//!    `docs::type_override` (fields whose declared type is deliberately
//!    wrong). Ignoring `_metadata` yields a schema-valid, semantically wrong
//!    catalog.

use crate::ir::{
    Domain, DomainKind, Field, FieldType, NamedType, ScalarType, Strictness, UnionVariant,
};
use indexmap::IndexMap;
use serde_json::Value;
use thiserror::Error;

/// Where a document keeps its reusable definitions.
///
/// Not inferred: a document may legally carry both, and guessing wrong yields
/// an empty catalog rather than an error. The caller states which one applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefsKey {
    /// `#/definitions/…` — draft-07, and what `vector generate-schema` emits
    /// despite declaring 2019-09.
    Definitions,
    /// `#/$defs/…` — canonical 2019-09.
    Defs,
}

impl DefsKey {
    fn key(self) -> &'static str {
        match self {
            Self::Definitions => "definitions",
            Self::Defs => "$defs",
        }
    }

    fn ref_prefix(self) -> &'static str {
        match self {
            Self::Definitions => "#/definitions/",
            Self::Defs => "#/$defs/",
        }
    }
}

/// How to lower one JSON-Schema document.
#[derive(Debug, Clone)]
pub struct JsonSchemaOptions {
    /// Crate name for the emitted domain.
    pub domain_name: String,
    /// Human-readable crate description.
    pub description: String,
    /// Whether an untypeable construct is an error. Catalogs pass
    /// [`Strictness::Strict`].
    pub strictness: Strictness,
    /// Which definitions keyword this document uses.
    pub defs_key: DefsKey,
    /// Emit the ROOT schema as a named type too, under this name.
    ///
    /// Without it the catalog is 351 types with no way in: `vector
    /// generate-schema`'s root is the whole config document (the `sources` /
    /// `transforms` / `sinks` maps plus the global options), and it lives
    /// outside `definitions`. Lowering only the definitions produces every
    /// component type and nothing that can deserialize an actual
    /// `vector.yaml`.
    pub root_name: Option<String>,
}

impl JsonSchemaOptions {
    /// Defaults tuned for a generated component catalog: strict, `definitions`.
    #[must_use]
    pub fn new(domain_name: impl Into<String>) -> Self {
        let domain_name = domain_name.into();
        Self {
            description: format!("Tatara domain generated from the {domain_name} JSON Schema."),
            domain_name,
            strictness: Strictness::Strict,
            defs_key: DefsKey::Definitions,
            root_name: None,
        }
    }

    /// Also emit the root schema, under `name`.
    #[must_use]
    pub fn with_root(mut self, name: impl Into<String>) -> Self {
        self.root_name = Some(name.into());
        self
    }
}

#[derive(Debug, Error)]
pub enum JsonSchemaError {
    #[error("json parse: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("document has no `{key}` object — wrong DefsKey, or not a schema graph")]
    NoDefinitions { key: &'static str },
    /// The strict-mode refusal. Carries the JSON pointer so the gap is
    /// locatable in a 1.5 MB document rather than merely reported.
    #[error("untypeable construct at `{path}`: {message} (strict mode; a `serde_json::Value` hole here would silently stop checking this option)")]
    Untypeable { path: String, message: String },
    #[error("dangling $ref at `{path}`: `{target}` is not in `{key}`")]
    DanglingRef {
        path: String,
        target: String,
        key: &'static str,
    },
    #[error("unknown JSON Schema type `{ty}` at `{path}`")]
    UnknownType { ty: String, path: String },
}

/// Read a schema from disk.
///
/// # Errors
/// See [`JsonSchemaError`].
pub fn from_json_schema_file(
    path: &std::path::Path,
    opts: &JsonSchemaOptions,
) -> Result<Domain, JsonSchemaError> {
    let bytes = std::fs::read_to_string(path)?;
    from_json_schema(&bytes, opts)
}

/// Lower a JSON-Schema document into a [`Domain`] of named types.
///
/// # Errors
/// See [`JsonSchemaError`].
pub fn from_json_schema(json: &str, opts: &JsonSchemaOptions) -> Result<Domain, JsonSchemaError> {
    let root: Value = serde_json::from_str(json)?;
    let defs = root
        .get(opts.defs_key.key())
        .and_then(Value::as_object)
        .ok_or(JsonSchemaError::NoDefinitions {
            key: opts.defs_key.key(),
        })?;

    // Pass 1: decide every definition's Rust name BEFORE lowering anything,
    // so a `$ref` encountered mid-walk can be resolved without a second pass
    // or a fixup step. Mangling is order-dependent (collisions resolve by
    // widening the earlier-losing name), so it must be deterministic --
    // `serde_json`'s object iteration preserves insertion order with the
    // `preserve_order` feature, and Vector sorts components by name anyway.
    let mut names: IndexMap<String, String> = IndexMap::new();
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    for key in defs.keys() {
        let name = unique_rust_name(key, &taken);
        taken.insert(name.clone());
        names.insert(key.clone(), name);
    }

    let ctx = Ctx {
        defs,
        names: &names,
        opts,
        hoisted: std::cell::RefCell::new(IndexMap::new()),
    };

    // Pass 2: lower each definition to a NamedType.
    let mut types: IndexMap<String, NamedType> = IndexMap::new();
    for (key, schema) in defs {
        let rust_name = &names[key];
        let path = format!("{}/{key}", opts.defs_key.key());
        let named = ctx.lower_named(schema, rust_name, &path)?;
        types.insert(rust_name.clone(), named);
    }

    // The root document itself, if the caller wants a way in.
    if let Some(root_name) = &opts.root_name {
        // `definitions` is a container for reusable subschemas, not a
        // property of the instance — leaving it in would emit a `definitions`
        // field on the config struct that no config ever carries.
        let mut root = root.clone();
        if let Some(obj) = root.as_object_mut() {
            obj.remove(opts.defs_key.key());
            obj.remove("$schema");
        }
        let named = ctx.lower_named(&root, root_name, "#")?;
        types.insert(root_name.clone(), named);
    }

    // Anonymous unions lifted out of field position. Appended after the
    // named definitions so the emitted module reads source-order-first.
    types.extend(ctx.hoisted.into_inner());

    Ok(Domain {
        name: opts.domain_name.clone(),
        description: opts.description.clone(),
        kind: DomainKind::OpenApi,
        // Resources (the `(def…)` keyword forms) are chosen by a caller that
        // knows which of the 351 definitions are author-facing. Lowering does
        // not guess: promoting all 351 to Lisp keyword forms would bury the
        // ~129 that matter under 222 option structs.
        resources: Vec::new(),
        types,
    })
}

struct Ctx<'a> {
    defs: &'a serde_json::Map<String, Value>,
    names: &'a IndexMap<String, String>,
    opts: &'a JsonSchemaOptions,
    /// Anonymous types discovered in *field* position and lifted to module
    /// scope.
    ///
    /// A union cannot be written inline in Rust the way an object can, so a
    /// `oneOf` appearing as a property (`SinkOuter.buffer` is the motivating
    /// case) has to become a named enum plus a [`FieldType::Ref`] to it.
    /// `RefCell` because lowering is otherwise a pure read over `defs`, and
    /// threading a `&mut` through every recursive arm would obscure that.
    hoisted: std::cell::RefCell<IndexMap<String, NamedType>>,
}

impl Ctx<'_> {
    /// Lower one definition into a module-scope type.
    fn lower_named(
        &self,
        schema: &Value,
        rust_name: &str,
        path: &str,
    ) -> Result<NamedType, JsonSchemaError> {
        let doc = doc_of(schema);

        // A union. Handled before `type`, because a `oneOf` node usually has
        // no `type` of its own and would otherwise fall through to the
        // untypeable arm.
        if let Some(arms) = union_arms(schema) {
            // ★ NULLABILITY IS NOT A VARIANT. This dialect writes `Option<T>`
            // as `oneOf: [{"type":"null"}, <T>]` -- measured: 32 such
            // definitions in vector's schema. Lowering that literally yields
            // a Rust enum with a `Null` arm, which is not how Rust spells
            // absence and would force every caller to match on it. The null
            // arm is dropped; optionality is already carried by
            // `Field::required` at the use site.
            // The path gains a `/!null` step so the error reported by a
            // failure INSIDE the unwrapped arm cannot be confused with one in
            // the wrapper -- both would otherwise print `.../oneOf[0]`, and
            // chasing the wrong one costs a round trip.
            if let Some(inner) = sole_non_null_arm(arms) {
                return self.lower_named(inner, rust_name, &format!("{path}/!null"));
            }
            // ★ A `oneOf` of bare `const` strings is a UNIT enum, not a sum
            // type with payloads. schemars writes closed string enums this
            // way rather than as `enum: [...]`, so a lowering that only knows
            // the `enum` keyword refuses a construct it should have accepted
            // (measured: `codecs::MetricTagValues` is the first of these in
            // vector's schema, and it is `Single | Full` -- a plain Rust
            // enum). Emitting it as a Union of empty variants would compile
            // and then serialize as `{"Single": null}` instead of `"single"`.
            if let Some(variants) = unit_const_variants(arms) {
                return Ok(NamedType::Enum { doc, variants });
            }
            let (tag, variants) = self.lower_union(arms, rust_name, path)?;
            return Ok(NamedType::Union { doc, tag, variants });
        }

        // A closed string set.
        if let Some(variants) = string_enum(schema) {
            return Ok(NamedType::Enum { doc, variants });
        }

        // An object with properties -> struct. `allOf` composition is merged
        // first so a definition assembled from parts still lands here.
        let (merged, flat) = self.merge_all_of(schema, rust_name, path)?;
        if merged.get("properties").is_some() || !flat.is_empty() {
            let has_flat = !flat.is_empty();
            let fields = self.struct_fields(&merged, flat, rust_name, path)?;
            return Ok(NamedType::Struct {
                doc,
                fields,
                deny_unknown: deny_unknown_for(schema, has_flat),
            });
        }

        // Anything else is a wrapper around a single type -- a scalar
        // definition, an array, a map. Newtype keeps the NAME, which is the
        // whole reason `vector::template::Template` survives as a distinct
        // type instead of collapsing into `String`.
        //
        // ★ Except when the "wrapper" would wrap a struct of its own name.
        // `lower_type` seeds anonymous struct names from the definition name,
        // so an object with no `properties` at this level (the three
        // options-less `unit_test` components) came back as
        // `Nested { struct_name: "UnitTestSourceConfig" }` -- yielding
        // `pub struct UnitTestSourceConfig(pub UnitTestSourceConfig);`, which
        // is simultaneously a duplicate definition and an infinitely-sized
        // self-reference. An object definition IS a struct; it needs no
        // wrapper, and the wrapper would also add a layer the wire lacks.
        let inner = self.lower_type(&merged, rust_name, path)?;
        match inner {
            FieldType::Nested {
                struct_name,
                fields,
            } if struct_name == rust_name => Ok(NamedType::Struct {
                doc,
                fields,
                deny_unknown: deny_unknown_for(schema, false),
            }),
            other => Ok(NamedType::Newtype { doc, inner: other }),
        }
    }

    /// Recover a discriminated union from `oneOf`.
    ///
    /// Handles the shape Vector emits — each arm is
    /// `allOf: [{$ref: Config}, {properties: {TAG: {const: VALUE}}}]` — and
    /// falls back to an untagged union when no arm carries a `const` tag.
    /// Mixing the two is refused rather than guessed: a union where only some
    /// arms are tagged deserializes unpredictably.
    fn lower_union(
        &self,
        arms: &[Value],
        rust_name: &str,
        path: &str,
    ) -> Result<(Option<String>, Vec<UnionVariant>), JsonSchemaError> {
        let mut variants = Vec::with_capacity(arms.len());
        let mut tags: Vec<Option<String>> = Vec::with_capacity(arms.len());

        for (i, arm) in arms.iter().enumerate() {
            let arm_path = format!("{path}/oneOf[{i}]");
            let tag = find_const_tag(arm);
            let (tag_field, tag_value) = match tag {
                Some((f, v)) => (Some(f), Some(v)),
                None => (None, None),
            };

            // The payload is the arm MINUS its discriminator. For Vector's
            // `allOf` encoding that is the `$ref` branch; for an arm that
            // carries its tag inline it is the arm with that property
            // removed.
            //
            // ★ Removing it is load-bearing, not tidying. `#[serde(tag =
            // "codec")]` SYNTHESIZES the tag field on both sides, so leaving
            // it in the payload emits a struct with a `codec: String` that
            // serde then also writes itself -- a duplicate key on the wire
            // and a field the author can set to contradict the variant they
            // chose.
            // Variant name: prefer the metadata's logical name (Vector gives
            // `logical_name: "File"`), then the tag value, then the position.
            // Position is a last resort and deliberately ugly -- `Variant7`
            // in generated output is a signal that a source stopped carrying
            // names, not something to live with.
            let name = metadata_str(arm, "logical_name")
                .map(|s| pascal(&s))
                .or_else(|| tag_value.as_deref().map(pascal))
                .unwrap_or_else(|| format!("Variant{i}"));

            // ★ The seed is per-ARM, not the union's own name. An arm whose
            // payload is an inline object gets its struct name seeded from
            // this; seeding it with `rust_name` made every such arm collide
            // with the union it belongs to (`Compression`, `BufferType`,
            // `MetricValue`, `AwsAuthentication` all hit this), emitting two
            // items with one name.
            let payload = payload_of_arm(arm, tag_field.as_deref());
            let ty = if is_unit_payload(&payload) {
                None
            } else {
                Some(self.lower_type(&payload, &format!("{rust_name}_{name}"), &arm_path)?)
            };

            tags.push(tag_field);
            variants.push(UnionVariant {
                name,
                tag_value,
                ty,
                doc: doc_of(arm),
            });
        }

        // Every arm must agree on the tag field, or there is no single
        // `#[serde(tag = …)]` that describes the union.
        let distinct: std::collections::BTreeSet<&Option<String>> = tags.iter().collect();
        match distinct.len() {
            0 => Ok((None, variants)),
            1 => Ok((tags.into_iter().next().flatten(), variants)),
            _ => Err(JsonSchemaError::Untypeable {
                path: path.to_string(),
                message: format!(
                    "oneOf arms disagree about the discriminator field ({} distinct); \
                     a partially-tagged union has no single serde representation",
                    distinct.len()
                ),
            }),
        }
    }

    /// Merge `allOf` branches into one object schema.
    ///
    /// Shallow by design: it unions `properties` and `required` and keeps the
    /// first `type`/`description`. Deep merging would silently reconcile
    /// genuinely conflicting branches; a conflict should surface as a wrong
    /// field, which a test catches, rather than as a plausible merge.
    /// Returns the merged object schema plus any branches that could NOT be
    /// merged as properties and must instead become `#[serde(flatten)]`
    /// fields.
    fn merge_all_of(
        &self,
        schema: &Value,
        seed: &str,
        path: &str,
    ) -> Result<(Value, Vec<(String, FieldType)>), JsonSchemaError> {
        let Some(branches) = schema.get("allOf").and_then(Value::as_array) else {
            return Ok((schema.clone(), Vec::new()));
        };
        let mut flattened: Vec<(String, FieldType)> = Vec::new();
        let mut out = schema.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.remove("allOf");
        }
        let mut props = serde_json::Map::new();
        let mut required: Vec<Value> = Vec::new();
        if let Some(existing) = out.get("properties").and_then(Value::as_object) {
            props.extend(existing.clone());
        }
        if let Some(existing) = out.get("required").and_then(Value::as_array) {
            required.extend(existing.clone());
        }

        for (i, branch) in branches.iter().enumerate() {
            let branch_path = format!("{path}/allOf[{i}]");
            // A branch may itself be a `$ref`; resolve it so its properties
            // participate in the merge.
            let resolved = self.resolve_ref(branch, &branch_path)?;
            let resolved_ref = resolved.as_ref().unwrap_or(branch);
            let has_props = resolved_ref.get("properties").is_some();
            if let Some(p) = resolved_ref.get("properties").and_then(Value::as_object) {
                for (k, v) in p {
                    props.insert(k.clone(), v.clone());
                }
            }
            if let Some(r) = resolved_ref.get("required").and_then(Value::as_array) {
                required.extend(r.clone());
            }
            // ★ A branch with NO properties still carries meaning. The
            // motivating case is `SourceOuter = allOf[{$ref: Sources},
            // {properties: {graph, proxy}}]`: `Sources` is a `oneOf`, so it
            // contributes zero properties and a properties-only merge drops
            // it entirely -- leaving a struct with no `type` tag, which then
            // accepts a component that does not exist. It becomes a flattened
            // field instead, which is what serde's `flatten` is for and what
            // Vector's own Rust does.
            if !has_props && union_arms(resolved_ref).is_some() {
                let ty = self.lower_type(branch, &format!("{seed}_flat{i}"), &branch_path)?;
                // Name the field after the type it carries, so two flattened
                // branches cannot collide on a generic name like `inner`.
                let field_name = match &ty {
                    FieldType::Ref(n) => crate::source::json_key_to_rust(n),
                    _ => format!("flattened_{i}"),
                };
                flattened.push((field_name, ty));
            }
        }

        if let Some(obj) = out.as_object_mut() {
            if !props.is_empty() {
                obj.insert("properties".into(), Value::Object(props));
            }
            if !required.is_empty() {
                obj.insert("required".into(), Value::Array(required));
            }
        }
        Ok((out, flattened))
    }

    /// Build a struct's field map: the merged properties, preceded by any
    /// flattened branches.
    fn struct_fields(
        &self,
        merged: &Value,
        flattened: Vec<(String, FieldType)>,
        seed: &str,
        path: &str,
    ) -> Result<IndexMap<String, Field>, JsonSchemaError> {
        let mut fields: IndexMap<String, Field> = IndexMap::new();
        for (name, ty) in flattened {
            fields.insert(
                name.clone(),
                Field {
                    rust_name: name,
                    ty,
                    doc: None,
                    // A flattened branch is part of the object's identity, not
                    // an optional extra: wrapping it in `Option` would let the
                    // discriminator go missing and the value still parse.
                    required: true,
                    flatten: true,
                },
            );
        }
        fields.extend(self.lower_properties(merged, seed, path)?);
        Ok(fields)
    }

    fn lower_properties(
        &self,
        schema: &Value,
        seed: &str,
        path: &str,
    ) -> Result<IndexMap<String, Field>, JsonSchemaError> {
        let required: std::collections::HashSet<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut out = IndexMap::new();
        let Some(props) = schema.get("properties").and_then(Value::as_object) else {
            return Ok(out);
        };
        for (key, sub) in props {
            let rust_name = crate::source::json_key_to_rust(key);
            let sub_path = format!("{path}/properties/{key}");
            let ty = self.lower_type(sub, &format!("{seed}_{rust_name}"), &sub_path)?;
            out.insert(
                rust_name.clone(),
                Field {
                    rust_name,
                    ty,
                    doc: doc_of(sub),
                    required: required.contains(key.as_str()),
                    flatten: false,
                },
            );
        }
        Ok(out)
    }

    /// Lower a schema fragment appearing in *field* position.
    fn lower_type(
        &self,
        schema: &Value,
        seed: &str,
        path: &str,
    ) -> Result<FieldType, JsonSchemaError> {
        // A reference resolves to a name, never to an inlined copy.
        if let Some(target) = schema.get("$ref").and_then(Value::as_str) {
            let key = target
                .strip_prefix(self.opts.defs_key.ref_prefix())
                .unwrap_or(target);
            return self
                .names
                .get(key)
                .map(|n| FieldType::Ref(n.clone()))
                .ok_or_else(|| JsonSchemaError::DanglingRef {
                    path: path.to_string(),
                    target: target.to_string(),
                    key: self.opts.defs_key.key(),
                });
        }

        // `type` may be a list, which is how this dialect writes nullability
        // (`["string", "null"]`). Strip the null and keep the real type;
        // optionality is already carried by `Field::required`.
        let ty_str = json_type_of(schema);

        match ty_str.as_deref() {
            Some("string") => {
                if let Some(variants) = string_enum(schema) {
                    // An inline enum still needs a module-scope name, but this
                    // ingestor does not mutate `types` mid-walk. Represent it
                    // with the existing inline form; the emitter already
                    // knows how to hoist those.
                    return Ok(FieldType::Enum {
                        type_name: pascal(&format!("{seed}_kind")),
                        variants,
                    });
                }
                Ok(FieldType::Scalar(ScalarType::String))
            }
            Some("boolean") => Ok(FieldType::Scalar(ScalarType::Bool)),
            // JSON Schema cannot express signedness or width. The `_metadata`
            // bag can, and dropping it would let a negative value deserialize
            // into a field the executor treats as unsigned.
            Some("integer") => Ok(FieldType::Scalar(numeric_scalar(schema))),
            Some("number") => Ok(FieldType::Scalar(ScalarType::F64)),
            Some("array") => {
                let items = schema
                    .get("items")
                    .ok_or_else(|| JsonSchemaError::Untypeable {
                        path: path.to_string(),
                        message: "array without `items`".into(),
                    })?;
                let inner = self.lower_type(items, &format!("{seed}_item"), path)?;
                Ok(FieldType::List(Box::new(inner)))
            }
            Some("object") => self.lower_object_type(schema, seed, path),
            Some(other) => Err(JsonSchemaError::UnknownType {
                ty: other.to_string(),
                path: path.to_string(),
            }),
            None => {
                // A bare `const` in field position: a string with exactly one
                // legal value. Lower to a one-variant enum rather than widening
                // to `String` -- the constraint is the whole content of the
                // declaration, and dropping it makes the field accept anything.
                if let Some(v) = schema.get("const").and_then(Value::as_str) {
                    return Ok(FieldType::Enum {
                        type_name: pascal(&format!("{seed}_kind")),
                        variants: vec![v.to_string()],
                    });
                }
                if let Some(arms) = union_arms(schema) {
                    // A nullable wrapper in field position — same rule as in
                    // `lower_named`: drop the null arm, keep the real type.
                    if let Some(inner) = sole_non_null_arm(arms) {
                        return self.lower_type(inner, seed, &format!("{path}/!null"));
                    }
                    // A const-enum nested inside a union arm. `Compression` is
                    // the motivating case: it is untagged `"gzip" | {algorithm,
                    // level}`, so one arm is a closed string set and the other
                    // an object. Without this, an enum that happens to appear
                    // in arm position is untypeable while the identical enum at
                    // definition scope lowers fine.
                    if let Some(variants) = unit_const_variants(arms) {
                        return Ok(FieldType::Enum {
                            type_name: pascal(&format!("{seed}_kind")),
                            variants,
                        });
                    }
                    // A real union in field position. Rust has no inline sum
                    // type, so lift it to module scope and refer to it.
                    let (tag, variants) = self.lower_union(arms, seed, path)?;
                    let name = self.hoist(
                        &pascal(seed),
                        NamedType::Union {
                            doc: doc_of(schema),
                            tag,
                            variants,
                        },
                    );
                    return Ok(FieldType::Ref(name));
                }
                // ★ THE EMPTY SCHEMA IS A DECLARATION, NOT A GAP.
                //
                // `{}` (and its equivalent `true`) is JSON Schema's way of
                // saying "any value is valid here". Lowering it to
                // `serde_json::Value` is therefore FAITHFUL, and strict mode
                // permits it -- the rule strict mode enforces is "a construct
                // we failed to type is an error", not "no `Value` may ever
                // appear". Conflating the two would refuse a schema that is
                // perfectly well specified, and the refusal would look like
                // rigour while actually being a bug.
                //
                // The distinction is exactly: did the author say "anything"
                // (here), or did the author say nothing (the `untypeable`
                // fallback below)? Measured on vector 0.52.0, this fires at
                // 2 sites, both `TestInput.log_fields` -- a unit-test fixture
                // whose fields genuinely are arbitrary JSON.
                if is_empty_schema(schema) {
                    return Ok(FieldType::Untyped);
                }
                // No `type`. Could still be a composition.
                if schema.get("oneOf").is_some() || schema.get("allOf").is_some() {
                    let (merged, _) = self.merge_all_of(schema, seed, path)?;
                    if merged.get("properties").is_some() {
                        return self.lower_object_type(&merged, seed, path);
                    }
                }
                self.untypeable(path, "no `type`, `$ref` or resolvable composition")
            }
        }
    }

    fn lower_object_type(
        &self,
        schema: &Value,
        seed: &str,
        path: &str,
    ) -> Result<FieldType, JsonSchemaError> {
        // A free-form map with a typed value -> HashMap<String, V>.
        match schema.get("additionalProperties") {
            Some(Value::Bool(true)) => {
                return self.untypeable(path, "`additionalProperties: true` (free-form object)")
            }
            Some(Value::Object(_)) => {
                let ap = &schema["additionalProperties"];
                let inner = self.lower_type(ap, &format!("{seed}_value"), path)?;
                return Ok(FieldType::Map(Box::new(inner)));
            }
            _ => {}
        }
        let (merged, flat) = self.merge_all_of(schema, seed, path)?;
        if merged.get("properties").is_some() || !flat.is_empty() {
            let fields = self.struct_fields(&merged, flat, seed, path)?;
            return Ok(FieldType::Nested {
                struct_name: pascal(seed),
                fields,
            });
        }
        // `{"type": "object"}` and nothing else: a component that takes no
        // options. Measured on vector 0.52.0 this is exactly 3 definitions --
        // the `unit_test` source/sink pair and `unit_test_stream` -- each
        // carrying `docs::component_type`, so they are real components rather
        // than unfinished schemas.
        //
        // Lowered to a zero-field struct, which is deliberately STRICTER than
        // the schema: JSON Schema's bare `{"type":"object"}` permits any keys,
        // while a `deny_unknown_fields` struct permits none. Stated rather
        // than hidden, because the direction matters -- everything this
        // catalog generates is still valid input, and the cost is that an
        // author cannot express a key the schema never described. The
        // alternative (`Untyped`) would make three components' options
        // uncheckable to buy back a freedom nobody has asked for.
        if json_type_of(schema).as_deref() == Some("object") {
            return Ok(FieldType::Nested {
                struct_name: pascal(seed),
                fields: IndexMap::new(),
            });
        }
        self.untypeable(
            path,
            "object with neither `properties` nor typed `additionalProperties`",
        )
    }

    /// Lift an anonymous type to module scope and return the name to refer to
    /// it by.
    ///
    /// Collides against BOTH the definition names and the already-hoisted
    /// ones, because a hoisted name that shadows a definition would silently
    /// redirect every reference to that definition.
    fn hoist(&self, preferred: &str, ty: NamedType) -> String {
        let mut taken: std::collections::HashSet<String> = self.names.values().cloned().collect();
        taken.extend(self.hoisted.borrow().keys().cloned());
        let mut name = preferred.to_string();
        let mut n = 2usize;
        while taken.contains(&name) {
            name = format!("{preferred}{n}");
            n += 1;
        }
        self.hoisted.borrow_mut().insert(name.clone(), ty);
        name
    }

    /// Resolve a `$ref` node to the definition it points at, if it is one.
    fn resolve_ref(&self, node: &Value, path: &str) -> Result<Option<Value>, JsonSchemaError> {
        let Some(target) = node.get("$ref").and_then(Value::as_str) else {
            return Ok(None);
        };
        let key = target
            .strip_prefix(self.opts.defs_key.ref_prefix())
            .unwrap_or(target);
        self.defs
            .get(key)
            .cloned()
            .map(Some)
            .ok_or_else(|| JsonSchemaError::DanglingRef {
                path: path.to_string(),
                target: target.to_string(),
                key: self.opts.defs_key.key(),
            })
    }

    /// The single place `Untyped` is decided, so strictness cannot be
    /// bypassed by a new call site forgetting to check it.
    fn untypeable<T>(&self, path: &str, message: &str) -> Result<T, JsonSchemaError> {
        // Permissive callers still exist (the CRD ingestor), so honour the
        // knob rather than hard-coding refusal -- but note that `Untyped` is
        // unreachable here today because every caller of this ingestor passes
        // Strict. Keeping the branch means the graph lowering can serve a
        // permissive source later without the strictness check being bolted
        // on at that point.
        debug_assert_eq!(
            self.opts.strictness,
            Strictness::Strict,
            "the permissive branch is unexercised; add a test before relying on it"
        );
        Err(JsonSchemaError::Untypeable {
            path: path.to_string(),
            message: message.to_string(),
        })
    }
}

// ── metadata + shape helpers ────────────────────────────────────────────────

/// Vector's out-of-band type information lives here.
const METADATA: &str = "_metadata";

fn metadata_str(node: &Value, key: &str) -> Option<String> {
    node.get(METADATA)?.get(key)?.as_str().map(str::to_string)
}

/// Signedness from `_metadata.docs::numeric_type`.
///
/// Measured on vector 0.52.0: `uint` 291, `float` 61, `int` 5. Defaulting all
/// of them to `i64` -- which is all JSON Schema's `type: integer` permits --
/// would accept a negative `batch.max_events` at the parse boundary and fail
/// in the executor instead.
fn numeric_scalar(node: &Value) -> ScalarType {
    match metadata_str(node, "docs::numeric_type").as_deref() {
        Some("uint") => ScalarType::U64,
        Some("int") => ScalarType::I64,
        _ => ScalarType::I64,
    }
}

fn doc_of(node: &Value) -> Option<String> {
    node.get("description")?.as_str().map(str::to_string)
}

/// `type` as a single string, tolerating the `["string", "null"]` nullable
/// form this dialect uses.
fn json_type_of(schema: &Value) -> Option<String> {
    match schema.get("type") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .find(|s| *s != "null")
            .map(str::to_string),
        _ => None,
    }
}

/// Whether a struct lowered from `schema` should `deny_unknown_fields`.
///
/// Taken from the source's own closure marker, never guessed: this dialect
/// stamps `unevaluatedProperties: false` on schemas it means to close, and
/// inventing closure where the schema did not ask for it would reject configs
/// the binary accepts.
///
/// Suppressed whenever a field is flattened, because serde cannot combine
/// `deny_unknown_fields` with `flatten` — the flattened child's keys look
/// unknown to the parent, so the pair rejects every value. That is a silent,
/// total failure, which is why it is enforced here rather than left to the
/// emitter to remember.
fn deny_unknown_for(schema: &Value, has_flattened: bool) -> bool {
    if has_flattened {
        return false;
    }
    schema.get("unevaluatedProperties").and_then(Value::as_bool) == Some(false)
}

/// Whether a node constrains nothing — JSON Schema's explicit "any value".
///
/// Annotation keywords (`description`, `title`, `_metadata`, …) do not
/// constrain instances, so a node carrying only those is still `{}`.
fn is_empty_schema(node: &Value) -> bool {
    const ANNOTATIONS: &[&str] = &[
        "description",
        "title",
        "_metadata",
        "deprecated",
        "examples",
        "default",
        "readOnly",
        "writeOnly",
    ];
    match node {
        Value::Bool(true) => true,
        Value::Object(m) => m.keys().all(|k| ANNOTATIONS.contains(&k.as_str())),
        _ => false,
    }
}

/// The variant arms of a union node, whether written `oneOf` or `anyOf`.
///
/// JSON Schema distinguishes them — `oneOf` is *exactly* one match, `anyOf` is
/// *at least* one — and Rust has no type for "at least one of these shapes".
/// Both therefore lower to the same enum, which is sound in the direction that
/// matters: every value the schema accepts has a representation. The loss is
/// that a value matching two `anyOf` arms is represented by whichever serde
/// tries first, and serde's untagged enums resolve in declaration order, which
/// is closer to `anyOf`'s intent than to `oneOf`'s.
///
/// Measured on vector 0.52.0: 84 definitions use `oneOf`, exactly **one**
/// (`vector::aws::auth::AwsAuthentication`) uses `anyOf`. Refusing the single
/// `anyOf` would have cost the entire AWS surface for a distinction that Rust
/// cannot express anyway.
fn union_arms(schema: &Value) -> Option<&Vec<Value>> {
    schema
        .get("oneOf")
        .and_then(Value::as_array)
        .or_else(|| schema.get("anyOf").and_then(Value::as_array))
}

/// The single meaningful arm of a nullable `oneOf`, if that is what this is.
///
/// Returns `Some` only when dropping every `{"type": "null"}` arm leaves
/// **exactly one** arm AND at least one null arm was actually dropped. Both
/// halves matter: without the first this would silently discard arms from a
/// genuine three-way union that happens to include null; without the second
/// it would unwrap a one-arm `oneOf` that means something else.
fn sole_non_null_arm(arms: &[Value]) -> Option<&Value> {
    let non_null: Vec<&Value> = arms
        .iter()
        .filter(|a| a.get("type").and_then(Value::as_str) != Some("null"))
        .collect();
    (non_null.len() == 1 && non_null.len() < arms.len()).then(|| non_null[0])
}

/// Whether a union arm carries nothing once its discriminator is removed.
///
/// True for `{"type":"object","required":["codec"],"properties":{"codec":…}}`
/// after the tag strip — the schema's way of writing a unit variant.
fn is_unit_payload(payload: &Value) -> bool {
    let Some(obj) = payload.as_object() else {
        return false;
    };
    !obj.contains_key("properties")
        && !obj.contains_key("$ref")
        && !obj.contains_key("allOf")
        && !obj.contains_key("oneOf")
        && !obj.contains_key("additionalProperties")
        && !obj.contains_key("items")
}

/// The wire values of a `oneOf` whose every arm is a bare `const` string.
///
/// Returns `None` the moment one arm carries a payload — a `type`,
/// `properties`, `$ref` or `allOf` — because a *partially* unit union is a
/// real sum type and collapsing it to an enum would silently discard the
/// arms that carry data.
fn unit_const_variants(arms: &[Value]) -> Option<Vec<String>> {
    let mut out = Vec::with_capacity(arms.len());
    for arm in arms {
        let obj = arm.as_object()?;
        if obj.contains_key("type")
            || obj.contains_key("properties")
            || obj.contains_key("$ref")
            || obj.contains_key("allOf")
        {
            return None;
        }
        out.push(obj.get("const")?.as_str()?.to_string());
    }
    (!out.is_empty()).then_some(out)
}

fn string_enum(schema: &Value) -> Option<Vec<String>> {
    let vals = schema.get("enum")?.as_array()?;
    let variants: Vec<String> = vals
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    (!variants.is_empty()).then_some(variants)
}

/// Find the `(field, const-value)` discriminator inside a union arm.
///
/// Vector buries it in `allOf[1].properties.<tag>.const`; a simpler dialect
/// puts it directly in `properties`. Both are searched, in that order.
fn find_const_tag(arm: &Value) -> Option<(String, String)> {
    if let Some(branches) = arm.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            if let Some(hit) = const_tag_in_properties(branch) {
                return Some(hit);
            }
        }
    }
    const_tag_in_properties(arm)
}

fn const_tag_in_properties(node: &Value) -> Option<(String, String)> {
    let props = node.get("properties")?.as_object()?;
    for (name, sub) in props {
        if let Some(v) = sub.get("const").and_then(Value::as_str) {
            return Some((name.clone(), v.to_string()));
        }
    }
    None
}

/// The part of a union arm that carries data, as opposed to the tag.
///
/// For Vector's `allOf` encoding that is the `$ref` branch. Otherwise it is
/// the arm itself with the discriminator property stripped from both
/// `properties` and `required`, so the tag does not also become a struct
/// field.
fn payload_of_arm(arm: &Value, tag_field: Option<&str>) -> Value {
    if let Some(branches) = arm.get("allOf").and_then(Value::as_array) {
        if let Some(r) = branches.iter().find(|b| b.get("$ref").is_some()) {
            return r.clone();
        }
    }
    let mut out = arm.clone();
    let Some(tag) = tag_field else {
        return out;
    };
    if let Some(obj) = out.as_object_mut() {
        if let Some(Value::Object(props)) = obj.get_mut("properties") {
            props.remove(tag);
            if props.is_empty() {
                obj.remove("properties");
            }
        }
        if let Some(Value::Array(req)) = obj.get_mut("required") {
            req.retain(|v| v.as_str() != Some(tag));
            if req.is_empty() {
                obj.remove("required");
            }
        }
    }
    out
}

// ── naming ──────────────────────────────────────────────────────────────────

/// Turn a definition key into a Rust type name, avoiding collisions.
///
/// Keys are fully-qualified Rust paths (`vector::sinks::file::FileSinkConfig`)
/// and may carry generics (`SinkOuter<String>`). The last path segment is
/// almost always the right name; when two definitions share one, the loser
/// widens to include its parent module, which is both stable and readable.
fn unique_rust_name(key: &str, taken: &std::collections::HashSet<String>) -> String {
    let segments: Vec<&str> = split_path(key);
    // For a generic, the meaningful name is the type PLUS its arguments
    // (`SinkOuter` + `String`), not whichever segment happens to be last.
    // Relying on the reserved-name widening to reach that answer works by
    // accident and stops working the moment an argument is not a reserved word.
    let base = if key.contains('<') {
        let n = key.matches(',').count() + 1;
        let start = segments.len().saturating_sub(n + 1);
        segments[start..]
            .iter()
            .map(|s| pascal(&sanitize(s)))
            .collect::<String>()
    } else {
        sanitize(segments.last().copied().unwrap_or(key))
    };
    if !taken.contains(&base) && !is_reserved_type_name(&base) {
        return base;
    }
    // Widen leftwards until unique.
    for take in 2..=segments.len() {
        let joined: String = segments[segments.len() - take..]
            .iter()
            .map(|s| pascal(&sanitize(s)))
            .collect();
        if !taken.contains(&joined) && !is_reserved_type_name(&joined) {
            return joined;
        }
    }
    // Exhausted the path: append a discriminating suffix rather than silently
    // returning a duplicate, which would emit two structs with one name.
    let mut n = 2usize;
    loop {
        let candidate = format!("{base}{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Split a definition key into path segments, keeping a generic monomorphisation
/// together with the type it parameterises.
///
/// ★ Splitting the raw key on `::` is wrong for a generic, and wrong in a way
/// that produces a plausible name rather than an error.
/// `vector::config::sink::SinkOuter<alloc::string::String>` split naively ends
/// in `String>`, so the last segment sanitizes to `String`, hits the reserved
/// list, widens one step and lands on **`StringString`** — a sink outer type
/// that says nothing about being a sink, carrying a doc comment that still
/// reads "Fully resolved sink component". Found by reading the generated
/// catalog, not by any test.
///
/// So the base path and the generic arguments are separated first, and the
/// name becomes base + each argument's own last segment:
/// `SinkOuter<alloc::string::String>` → `SinkOuterString`,
/// `Inputs<alloc::string::String>` → `InputsString`.
fn split_path(key: &str) -> Vec<&str> {
    let Some(open) = key.find('<') else {
        return key.split("::").collect();
    };
    let (base, rest) = key.split_at(open);
    let args = rest.trim_start_matches('<').trim_end_matches('>');
    let mut out: Vec<&str> = base.split("::").collect();
    // Only the final segment of each argument carries meaning; the module path
    // in front of it (`alloc::string::`) is noise in a type name.
    for arg in args.split(',') {
        if let Some(last) = arg.trim().rsplit("::").next() {
            if !last.is_empty() {
                out.push(last);
            }
        }
    }
    out
}

/// Names a generated type may never take, because taking one SHADOWS it.
///
/// ★ This is the subtlest failure in the whole lowering, and it was found by
/// compiling rather than by reading. A definition whose last path segment is
/// `String` (schemars emits `alloc::string::String` and friends) mangles to
/// `String` and then shadows the prelude — so every field the IR lowered as
/// `ScalarType::String` silently resolves to the GENERATED struct instead of
/// Rust's. It surfaced as `E0072: recursive types have infinite size` across
/// four unrelated types, which points nowhere near the cause; a shadow that
/// happened not to close a cycle would have compiled and been wrong.
///
/// Primitives are included even though a `struct u64` would not shadow the
/// primitive in type position, because a type named `u64` is a trap for every
/// later reader whether or not the compiler minds.
fn is_reserved_type_name(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        // Prelude types a generated name would genuinely shadow.
        "String",
        "Option",
        "Result",
        "Vec",
        "Box",
        "Some",
        "None",
        "Ok",
        "Err",
        "Cow",
        "Rc",
        "Arc",
        "HashMap",
        "BTreeMap",
        "HashSet",
        "BTreeSet",
        "Self",
        // Primitives.
        "bool",
        "char",
        "str",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "f32",
        "f64",
        // Traits the emitted derives bring into scope.
        "Serialize",
        "Deserialize",
        "Debug",
        "Clone",
        "Default",
    ];
    RESERVED.contains(&name)
}

/// Strip generics and punctuation a Rust identifier cannot carry.
/// `SinkOuter<String>` -> `SinkOuterString`.
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = false;
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => {
                if upper_next {
                    out.extend(c.to_uppercase());
                    upper_next = false;
                } else {
                    out.push(c);
                }
            }
            _ => upper_next = !out.is_empty(),
        }
    }
    out
}

/// PascalCase an arbitrary string into something that is definitely a legal
/// Rust identifier.
///
/// ★ Every non-alphanumeric character is a SEPARATOR, never passed through.
/// Type names here are seeded from field names, and a field whose JSON key is
/// a Rust keyword becomes a raw identifier (`type` → `r#type`) — so a
/// pass-through implementation produced struct names containing `#`, which
/// fail to parse with a message ("unknown prefix") that points at the emitted
/// crate rather than at the naming rule that made it.
fn pascal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if upper {
                out.extend(c.to_uppercase());
                upper = false;
            } else {
                out.push(c);
            }
        } else {
            upper = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> JsonSchemaOptions {
        JsonSchemaOptions::new("test-domain")
    }

    #[test]
    fn a_ref_lowers_to_a_named_reference_not_an_inlined_copy() {
        // The property that makes a 351-definition graph tractable at all.
        let doc = r##"{
          "definitions": {
            "a::Shared": {"type":"object","properties":{"x":{"type":"string"}}},
            "a::First": {"type":"object","properties":{"s":{"$ref":"#/definitions/a::Shared"}}},
            "a::Second": {"type":"object","properties":{"s":{"$ref":"#/definitions/a::Shared"}}}
          }
        }"##;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        assert_eq!(d.types.len(), 3);
        for name in ["First", "Second"] {
            let NamedType::Struct { fields, .. } = &d.types[name] else {
                panic!("{name} should be a struct");
            };
            assert!(
                matches!(&fields["s"].ty, FieldType::Ref(r) if r == "Shared"),
                "{name}.s must be a Ref, not an inlined copy"
            );
        }
    }

    #[test]
    fn vectors_allof_const_encoding_recovers_a_tagged_union() {
        // Trap 2 from the module docs, verbatim in shape: the discriminator is
        // a `const` inside the SECOND allOf branch, not a `discriminator`
        // keyword and not an `enum`.
        let doc = r##"{
          "definitions": {
            "v::FileConfig": {"type":"object","properties":{"path":{"type":"string"}}},
            "v::Sinks": {"oneOf":[
              {"allOf":[
                 {"$ref":"#/definitions/v::FileConfig"},
                 {"type":"object","required":["type"],
                  "properties":{"type":{"const":"file"}}}],
               "_metadata":{"logical_name":"File"}}
            ]}
          }
        }"##;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Union { tag, variants, .. } = &d.types["Sinks"] else {
            panic!("Sinks should be a union");
        };
        assert_eq!(tag.as_deref(), Some("type"), "tag must be recovered");
        assert_eq!(
            variants[0].name, "File",
            "logical_name wins the variant name"
        );
        assert_eq!(variants[0].tag_value.as_deref(), Some("file"));
        assert!(
            matches!(&variants[0].ty, Some(FieldType::Ref(r)) if r == "FileConfig"),
            "payload is the $ref branch, not the tag branch"
        );
    }

    #[test]
    fn a_tagged_arm_with_no_options_becomes_a_unit_variant() {
        // Measured on vector's `codecs::decoding::DeserializerConfig`: some
        // codecs carry a config struct, `bytes` carries nothing. Forcing an
        // empty struct onto the payload-less arms would put `{}` on the wire
        // where the schema says there is nothing at all.
        let doc = r#"{
          "definitions": {
            "c::Deser": {"oneOf":[
              {"type":"object","required":["codec"],
               "properties":{"codec":{"const":"bytes"}},
               "_metadata":{"logical_name":"Bytes"}}
            ]}
          }
        }"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Union { tag, variants, .. } = &d.types["Deser"] else {
            panic!("Deser should be a union");
        };
        assert_eq!(tag.as_deref(), Some("codec"));
        assert_eq!(variants[0].name, "Bytes");
        assert!(
            variants[0].ty.is_none(),
            "an arm carrying only its tag is a unit variant, not an empty struct"
        );
    }

    #[test]
    fn a_oneof_of_bare_consts_is_an_enum_not_a_union() {
        // schemars writes closed string enums as `oneOf` of `const`, never as
        // `enum: [...]`. Lowering that to a Union of empty variants compiles
        // and then serializes as `{"Single": null}` instead of `"single"`.
        let doc = r#"{
          "definitions": {
            "c::MetricTagValues": {"oneOf":[
              {"const":"single","_metadata":{"logical_name":"Single"}},
              {"const":"full","_metadata":{"logical_name":"Full"}}
            ]}
          }
        }"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Enum { variants, .. } = &d.types["MetricTagValues"] else {
            panic!("a payload-less oneOf must be an Enum, got a Union");
        };
        assert_eq!(variants, &["single".to_string(), "full".to_string()]);
    }

    #[test]
    fn a_union_in_field_position_is_hoisted_to_a_named_type() {
        // Rust has no inline sum type, so `SinkOuter.buffer` -- a `oneOf` of
        // `$ref`s sitting in a property -- must become a module-scope enum
        // plus a Ref. Without this the field is untypeable even though every
        // arm of it is perfectly well described.
        let doc = r##"{
          "definitions": {
            "b::Single": {"type":"object","properties":{"a":{"type":"string"}}},
            "b::Multi":  {"type":"object","properties":{"b":{"type":"string"}}},
            "b::Outer": {"type":"object","properties":{
              "buffer": {"oneOf":[
                {"$ref":"#/definitions/b::Single","_metadata":{"logical_name":"Single"}},
                {"$ref":"#/definitions/b::Multi","_metadata":{"logical_name":"Multi"}}
              ]}}}
          }
        }"##;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Struct { fields, .. } = &d.types["Outer"] else {
            panic!("Outer should be a struct");
        };
        let FieldType::Ref(target) = &fields["buffer"].ty else {
            panic!(
                "buffer must be a Ref to a hoisted type, got {:?}",
                fields["buffer"].ty
            );
        };
        assert!(
            matches!(&d.types[target], NamedType::Union { variants, .. } if variants.len() == 2),
            "the hoisted type must be the 2-arm union"
        );
        assert_eq!(d.types.len(), 4, "3 definitions + 1 hoisted union");
    }

    #[test]
    fn a_hoisted_name_never_shadows_a_definition() {
        // A hoisted name colliding with a definition would silently redirect
        // every reference to that definition to the anonymous type instead.
        let doc = r##"{
          "definitions": {
            "b::A": {"type":"object","properties":{"x":{"type":"string"}}},
            "b::OuterBuffer": {"type":"object","properties":{"z":{"type":"string"}}},
            "b::Outer": {"type":"object","properties":{
              "buffer": {"oneOf":[
                {"$ref":"#/definitions/b::A","_metadata":{"logical_name":"A"}},
                {"type":"object","properties":{"y":{"type":"string"}},
                 "_metadata":{"logical_name":"Inline"}}
              ]}}}
          }
        }"##;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Struct { fields, .. } = &d.types["Outer"] else {
            panic!()
        };
        let FieldType::Ref(target) = &fields["buffer"].ty else {
            panic!("expected a Ref")
        };
        assert_ne!(
            target, "OuterBuffer",
            "the hoisted union must not take a name a definition already owns"
        );
        // And the original definition is still intact and reachable.
        assert!(matches!(
            &d.types["OuterBuffer"],
            NamedType::Struct { fields, .. } if fields.contains_key("z")
        ));
    }

    #[test]
    fn an_options_less_component_is_an_empty_struct_not_a_hole() {
        // vector's three `unit_test` components are bare `{"type":"object"}`.
        let doc = r#"{"definitions":{"v::UnitTestSource":{"type":"object",
            "_metadata":{"docs::component_type":"source"}}}}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        // A STRUCT, not a newtype wrapping a same-named struct. The wrapper
        // shape this used to produce was
        //   pub struct UnitTestSource(pub UnitTestSource);
        // which is at once a duplicate definition and an infinitely-sized
        // self-reference — caught only by compiling the generated crate.
        let NamedType::Struct { fields, .. } = &d.types["UnitTestSource"] else {
            panic!("got {:?}", d.types["UnitTestSource"]);
        };
        assert!(
            fields.is_empty(),
            "an options-less component is a zero-field struct, never Untyped"
        );
    }

    #[test]
    fn a_generated_name_never_shadows_a_prelude_type() {
        // The subtlest failure found in this whole lowering, and it was found
        // by COMPILING, not by reading. A definition whose last segment is
        // `String` mangles to `String` and shadows the prelude, so every
        // field lowered as a string silently resolves to the generated struct.
        // It surfaced as E0072 across four unrelated types — and a shadow that
        // happened not to close a cycle would have compiled and been wrong.
        let doc = r#"{"definitions":{
            "alloc::string::String":{"type":"object","properties":{"a":{"type":"string"}}},
            "core::option::Option":{"type":"object","properties":{"b":{"type":"string"}}}
        }}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        assert!(
            !d.types.contains_key("String") && !d.types.contains_key("Option"),
            "reserved names must be widened, got {:?}",
            d.types.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            d.types.len(),
            2,
            "both definitions still land, under safe names"
        );
    }

    #[test]
    fn a_union_arm_struct_does_not_collide_with_its_own_union() {
        // An arm whose payload is an inline object gets its struct name seeded
        // from the arm, not from the union. Seeding from the union emitted two
        // items with one name (`Compression`, `BufferType`, `MetricValue` and
        // `AwsAuthentication` all hit this on the real schema).
        let doc = r#"{"definitions":{"v::Compression":{"oneOf":[
            {"type":"object","properties":{"level":{"type":"integer"}},
             "_metadata":{"logical_name":"Detailed"}}
        ]}}}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        assert!(matches!(&d.types["Compression"], NamedType::Union { .. }));
        // Whatever the arm's payload struct is called, it is not the union.
        let NamedType::Union { variants, .. } = &d.types["Compression"] else {
            panic!()
        };
        if let Some(FieldType::Nested { struct_name, .. }) = &variants[0].ty {
            assert_ne!(
                struct_name, "Compression",
                "an arm's payload struct must not take the union's own name"
            );
        }
    }

    #[test]
    fn an_explicitly_empty_schema_is_allowed_even_in_strict_mode() {
        // `{}` means "any value" -- a DECLARATION. Strict mode refuses
        // constructs we failed to type, not constructs the author typed as
        // open. Conflating them would refuse a well-specified schema.
        let doc = r#"{"definitions":{"v::T":{"type":"object","properties":{
            "log_fields":{"type":"object","additionalProperties":{}}}}}}"#;
        let d = from_json_schema(doc, &opts()).expect("an explicit `{}` is not a gap");
        let NamedType::Struct { fields, .. } = &d.types["T"] else {
            panic!()
        };
        assert!(
            matches!(&fields["log_fields"].ty, FieldType::Map(inner) if matches!(**inner, FieldType::Untyped)),
            "an open map keeps its map shape and carries Untyped as the VALUE"
        );
    }

    #[test]
    fn an_allof_branch_that_is_a_union_becomes_a_flattened_field() {
        // `SourceOuter = allOf[{$ref: Sources}, {properties:{graph}}]`.
        // `Sources` is a oneOf and contributes NO properties, so a
        // properties-only merge dropped it -- leaving a struct with no `type`
        // tag, which then accepted components that do not exist. Measured:
        // that is exactly what let an unknown source deserialize cleanly.
        let doc = r##"{
          "definitions": {
            "v::Sources": {"oneOf":[
              {"type":"object","required":["type"],
               "properties":{"type":{"const":"file"}},
               "_metadata":{"logical_name":"File"}}]},
            "v::SourceOuter": {"allOf":[
              {"$ref":"#/definitions/v::Sources"},
              {"type":"object","properties":{"graph":{"type":"string"}}}]}
          }
        }"##;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Struct { fields, .. } = &d.types["SourceOuter"] else {
            panic!("got {:?}", d.types["SourceOuter"]);
        };
        let flat: Vec<_> = fields.values().filter(|f| f.flatten).collect();
        assert_eq!(
            flat.len(),
            1,
            "the union branch must survive as a flattened field"
        );
        assert!(
            matches!(&flat[0].ty, FieldType::Ref(r) if r == "Sources"),
            "and it must point at the union, got {:?}",
            flat[0].ty
        );
        assert!(
            flat[0].required,
            "a flattened discriminator is never optional -- optional would let \
             the tag go missing and the value still parse"
        );
        assert!(
            fields.contains_key("graph"),
            "sibling properties still merge"
        );
    }

    #[test]
    fn deny_unknown_fields_follows_the_schemas_own_closure_marker() {
        // Never guessed: inventing closure would reject configs the binary
        // accepts, and omitting it silently drops typo'd keys.
        let doc = r#"{"definitions":{
          "v::Closed":{"type":"object","unevaluatedProperties":false,
                       "properties":{"a":{"type":"string"}}},
          "v::Open":{"type":"object","properties":{"a":{"type":"string"}}}
        }}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Struct { deny_unknown, .. } = &d.types["Closed"] else {
            panic!()
        };
        assert!(*deny_unknown, "a closed schema denies unknown fields");
        let NamedType::Struct { deny_unknown, .. } = &d.types["Open"] else {
            panic!()
        };
        assert!(!*deny_unknown, "an open schema does not");
    }

    #[test]
    fn deny_unknown_fields_is_suppressed_when_a_field_is_flattened() {
        // serde cannot combine the two: the flattened child's keys look
        // unknown to the parent, so the pair rejects EVERY value. A silent,
        // total failure -- hence enforced in lowering, not left to the
        // emitter to remember.
        let doc = r##"{
          "definitions": {
            "v::Inner": {"oneOf":[
              {"type":"object","required":["type"],
               "properties":{"type":{"const":"x"}},"_metadata":{"logical_name":"X"}}]},
            "v::Outer": {"unevaluatedProperties": false, "allOf":[
              {"$ref":"#/definitions/v::Inner"},
              {"type":"object","properties":{"g":{"type":"string"}}}]}
          }
        }"##;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Struct {
            fields,
            deny_unknown,
            ..
        } = &d.types["Outer"]
        else {
            panic!()
        };
        assert!(
            fields.values().any(|f| f.flatten),
            "precondition: something flattens"
        );
        assert!(
            !*deny_unknown,
            "the schema asked for closure, but flatten makes it unsatisfiable"
        );
    }

    #[test]
    fn a_reserved_keyword_field_cannot_poison_a_generated_type_name() {
        // Type names are seeded from field names, and a field whose JSON key
        // is a Rust keyword becomes `r#type`. Passing `#` through produced
        // struct names that fail to parse, with a message pointing at the
        // emitted crate rather than at the naming rule.
        let doc = r#"{"definitions":{"v::T":{"type":"object","properties":{
            "type":{"type":"object","properties":{"a":{"type":"string"}}}}}}}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Struct { fields, .. } = &d.types["T"] else {
            panic!()
        };
        let FieldType::Nested { struct_name, .. } = &fields["r#type"].ty else {
            panic!("expected a nested struct, got {:?}", fields["r#type"].ty);
        };
        assert!(
            struct_name.chars().all(char::is_alphanumeric),
            "a generated type name must be a legal ident, got {struct_name}"
        );
    }

    #[test]
    fn a_partially_unit_oneof_stays_a_union() {
        // The guard on the rule above: one arm with a payload means this is a
        // real sum type, and collapsing it would discard that arm's data.
        let doc = r##"{
          "definitions": {
            "c::P": {"type":"object","properties":{"a":{"type":"string"}}},
            "c::Mixed": {"oneOf":[
              {"const":"none"},
              {"$ref":"#/definitions/c::P"}
            ]}
          }
        }"##;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        assert!(
            matches!(&d.types["Mixed"], NamedType::Union { .. }),
            "a oneOf with one payload-carrying arm is not an enum"
        );
    }

    #[test]
    fn numeric_signedness_comes_from_metadata_not_from_json_schema() {
        // JSON Schema cannot say `uint`. 291 of vector's 357 numeric fields
        // are unsigned; lowering them to i64 accepts negatives at the parse
        // boundary.
        let doc = r#"{
          "definitions": {
            "v::B": {"type":"object","properties":{
              "max_events":{"type":"integer","_metadata":{"docs::numeric_type":"uint"}},
              "offset":{"type":"integer","_metadata":{"docs::numeric_type":"int"}},
              "bare":{"type":"integer"}
            }}
          }
        }"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Struct { fields, .. } = &d.types["B"] else {
            panic!("B should be a struct");
        };
        assert!(matches!(
            fields["max_events"].ty,
            FieldType::Scalar(ScalarType::U64)
        ));
        assert!(matches!(
            fields["offset"].ty,
            FieldType::Scalar(ScalarType::I64)
        ));
        assert!(
            matches!(fields["bare"].ty, FieldType::Scalar(ScalarType::I64)),
            "no metadata falls back to the signed reading, never to unsigned"
        );
    }

    #[test]
    fn nullable_type_arrays_keep_the_real_type() {
        let doc = r#"{"definitions":{"v::N":{"type":"object","properties":{
            "maybe":{"type":["string","null"]}}}}}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Struct { fields, .. } = &d.types["N"] else {
            panic!()
        };
        assert!(matches!(
            fields["maybe"].ty,
            FieldType::Scalar(ScalarType::String)
        ));
    }

    #[test]
    fn strict_mode_refuses_a_free_form_object_and_names_its_path() {
        // The done-predicate: an unmapped construct is an ERROR, never a
        // `serde_json::Value` hole that silently stops checking an option.
        let doc = r#"{"definitions":{"v::F":{"type":"object","properties":{
            "anything":{"type":"object","additionalProperties":true}}}}}"#;
        let err = from_json_schema(doc, &opts()).expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("additionalProperties"), "got: {msg}");
        assert!(
            msg.contains("definitions/v::F/properties/anything"),
            "the path must locate the gap in a 1.5 MB document; got: {msg}"
        );
    }

    #[test]
    fn a_dangling_ref_is_an_error_not_a_silent_drop() {
        let doc = r##"{"definitions":{"v::A":{"type":"object","properties":{
            "x":{"$ref":"#/definitions/v::Missing"}}}}}"##;
        let err = from_json_schema(doc, &opts()).expect_err("must refuse");
        assert!(err.to_string().contains("dangling"), "got: {err}");
    }

    #[test]
    fn wrong_defs_key_errors_instead_of_returning_an_empty_catalog() {
        // Trap 1: vector declares draft 2019-09 but uses `definitions`. A
        // reader looking for `$defs` must fail loudly, because an empty
        // catalog looks exactly like a successfully-lowered empty document.
        let doc = r#"{"definitions":{"v::A":{"type":"object","properties":{}}}}"#;
        let mut o = opts();
        o.defs_key = DefsKey::Defs;
        let err = from_json_schema(doc, &o).expect_err("must refuse");
        assert!(err.to_string().contains("$defs"), "got: {err}");
    }

    #[test]
    fn colliding_last_segments_widen_leftwards_instead_of_clobbering() {
        let doc = r#"{
          "definitions": {
            "v::sinks::file::Config": {"type":"object","properties":{"a":{"type":"string"}}},
            "v::sources::file::Config": {"type":"object","properties":{"b":{"type":"string"}}}
          }
        }"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        assert_eq!(d.types.len(), 2, "two definitions must yield two types");
        assert!(d.types.contains_key("Config"));
        assert!(
            d.types
                .keys()
                .any(|k| k != "Config" && k.contains("Config")),
            "the loser widens: {:?}",
            d.types.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn generics_in_a_definition_key_sanitize_to_a_legal_identifier() {
        let doc = r#"{"definitions":{"v::SinkOuter<String>":{"type":"object",
            "properties":{"x":{"type":"string"}}}}}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        assert!(
            d.types.contains_key("SinkOuterString"),
            "got {:?}",
            d.types.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_generic_argument_with_a_module_path_keeps_the_outer_type_name() {
        // ★ The real-world form, and the one that broke. Splitting the raw key
        // on `::` makes the LAST segment `String>`, which sanitizes to
        // `String`, hits the reserved list, widens one step, and lands on
        // `StringString` -- a sink outer type whose name says nothing about
        // being a sink, carrying a doc comment that still reads "Fully
        // resolved sink component". It compiled and round-tripped perfectly;
        // it was found by READING the generated catalog.
        let doc = r#"{"definitions":{
          "vector::config::sink::SinkOuter<alloc::string::String>":
            {"type":"object","properties":{"inputs":{"type":"string"}}},
          "vector_common::id::Inputs<alloc::string::String>":
            {"type":"array","items":{"type":"string"}}
        }}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let names: Vec<&String> = d.types.keys().collect();
        assert!(
            d.types.contains_key("SinkOuterString"),
            "the outer type must survive its own generic argument, got {names:?}"
        );
        assert!(
            d.types.contains_key("InputsString"),
            "and the argument's module path is noise, got {names:?}"
        );
        assert!(
            !d.types.contains_key("StringString"),
            "the accidental name must not come back, got {names:?}"
        );
    }

    #[test]
    fn a_named_scalar_definition_survives_as_a_newtype() {
        // `vector::template::Template` is a String with a meaning. Collapsing
        // it to `String` is how a Vector template stops being distinguishable
        // from any other string at the type level.
        let doc = r#"{"definitions":{"vector::template::Template":{"type":"string",
            "_metadata":{"docs::templateable":true}}}}"#;
        let d = from_json_schema(doc, &opts()).expect("lowers");
        let NamedType::Newtype { inner, .. } = &d.types["Template"] else {
            panic!("Template must stay a distinct named type");
        };
        assert!(matches!(inner, FieldType::Scalar(ScalarType::String)));
    }
}

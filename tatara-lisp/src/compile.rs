//! Generic Lisp-to-type compiler — drives `#[derive(TataraDomain)]` types.
//!
//! This module used to contain a 1200-line hand-rolled compiler for a single
//! domain (ProcessSpec). The derive macro now handles every typed domain
//! uniformly, so this file shrinks to a thin pipeline: read → macroexpand →
//! dispatch to derive-generated `compile_from_args`.
//!
//! Two entry points:
//!   - `compile_typed::<T>(src)` — every `(T::KEYWORD :k v …)` form becomes
//!     one `T`. Returns `Vec<T>`.
//!   - `compile_named::<T>(src)` — every `(T::KEYWORD NAME :k v …)` form
//!     (positional name after keyword) becomes one `NamedDefinition<T>`.
//!     This is the shape used by ProcessSpec / `(defpoint name …)`.

use crate::ast::Sexp;
use crate::domain::TataraDomain;
use crate::error::{LispError, Result};
use crate::macro_expand::Expander;
use crate::reader::read;

/// Typed-keyword dispatchers on the `Expander` surface — the
/// `T: TataraDomain`-shaped sibling family of
/// [`Expander::expand_and_collect_calls_to`] (from-forms posture) and
/// [`Expander::expand_source_and_collect_calls_to`] (from-source posture).
///
/// The family is closed across TWO axes: input posture (from-forms +
/// from-source) × form shape (typed bare-kwargs + named NAME-then-kwargs).
/// Each cell is ONE typed method on `Expander`, binding `(T::KEYWORD,
/// projection-for-T)` at the type level through `T`:
///
/// |              | typed bare-kwargs            | named NAME-then-kwargs        |
/// |--------------|------------------------------|-------------------------------|
/// | from-forms   | [`expand_to_typed`](Self::expand_to_typed)   | [`expand_to_named`](Self::expand_to_named)   |
/// | from-source  | [`expand_source_to_typed`](Self::expand_source_to_typed) | [`expand_source_to_named`](Self::expand_source_to_named) |
///
/// The from-source row composes `crate::reader::read` with its from-forms
/// row sibling — `read(src)? + <expander>.expand_to_typed::<T>(forms)` —
/// so the typed-pair `(T::KEYWORD, projection-for-T)` is bound in ONE
/// place per form shape (the from-forms row), and the from-source row
/// inherits the binding through delegation. A regression that mis-pairs
/// `T::KEYWORD` with `U::compile_from_args` (where `T != U`) is
/// structurally impossible at any site: the type parameter binds both
/// substitutions together inside ONE method body per form shape.
impl Expander {
    /// Macroexpand + project every `(T::KEYWORD :k v …)` form in `forms`
    /// into a typed `T` — the from-forms posture of the typed bare-kwargs
    /// dispatcher family, sibling of [`Self::expand_to_named`].
    ///
    /// Composes [`Self::expand_and_collect_calls_to`] with `T::KEYWORD`
    /// as the keyword filter and `T::compile_from_args` as the per-form
    /// projection — the two-arg `(keyword, projection)` discipline bound
    /// at the type level through `T` inside ONE method body.
    ///
    /// Sibling of [`Self::expand_source_to_typed`] — that method stacks
    /// a `crate::reader::read` step on top of this one, projecting source
    /// text through the SAME typed-pair primitive. Consumers that have
    /// already parsed their forms (macro-expanded subforms, `Sexp`
    /// loaded from disk, a REPL's already-read top-level buffer) bind
    /// to this method; consumers that consume source text directly bind
    /// to the from-source sibling.
    ///
    /// Theory anchor: THEORY.md §VI.1 — generation over composition;
    /// the typed-pair `(T::KEYWORD, T::compile_from_args)` is bound in
    /// ONE place per form shape (this method) — the from-source sibling
    /// inherits the binding through delegation rather than re-deriving
    /// it at its own call site. THEORY.md §II.1 invariant 1 — typed
    /// entry; the typed-keyword filter paired with `T::compile_from_args`
    /// IS the from-forms typed-entry-batch gate, named on the `Expander`
    /// surface. THEORY.md §II.1 invariant 2 — free middle; the from-forms
    /// posture and the from-source posture route through the SAME typed
    /// primitive, so a regression that drifts ONE posture's pairing
    /// from the other becomes structurally impossible.
    ///
    /// Frontier inspiration: MLIR's `Region::walk<Op>(callback)` —
    /// every typed rewriter binds to a region walker that composes the
    /// typed kind filter with the per-op visitor; the substrate's
    /// `expand_to_typed::<T>` is the typed-pair peer on the `&[Sexp]`
    /// algebra, with `T: TataraDomain` standing in for MLIR's `Op` type
    /// witness.
    pub fn expand_to_typed<T: TataraDomain>(&mut self, forms: Vec<Sexp>) -> Result<Vec<T>> {
        self.expand_and_collect_calls_to(forms, T::KEYWORD, T::compile_from_args)
    }

    /// Macroexpand + project every `(T::KEYWORD NAME :k v …)` form in
    /// `forms` into a typed [`NamedDefinition<T>`] — the from-forms posture
    /// of the named NAME-then-kwargs dispatcher family, sibling of
    /// [`Self::expand_to_typed`].
    ///
    /// Routes through the named constant-keyword primitive
    /// [`Self::expand_and_collect_named_calls_to`] (which itself routes
    /// through the named typed-decoded classifier primitive
    /// [`Self::expand_and_collect_named_calls_to_any`] via a constant-
    /// classifier decoder) with `T::KEYWORD` as the keyword filter and
    /// a per-form `(name, spec_args) -> Result<NamedDefinition<T>>`
    /// projection that composes `T::compile_from_args` with the
    /// `NamedDefinition { name: name.to_string(), spec }` packaging.
    /// Post-lift the typed (this method, `T::KEYWORD`-baked) and
    /// untyped (the runtime-keyword sibling
    /// [`Self::expand_and_collect_named_calls_to`]) constant-keyword
    /// named cells route through the SAME composition point on the
    /// `Expander` surface, mirroring how `expand_and_collect_calls_to`
    /// (bare × constant-keyword × untyped) routes through
    /// `expand_and_collect_calls_to_any` (bare × classifier) — the
    /// `split_name_slot` composition lives at ONE site
    /// (`expand_and_collect_named_calls_to_any` body) for the entire
    /// named cell, with `crate::compile::named_form_projection`
    /// remaining a slice-side primitive for callers that have a
    /// single rest tail.
    ///
    /// The named-form structural rejection chain (`NamedFormMissingName`
    /// for the missing NAME slot, `NamedFormNonSymbolName` for the
    /// non-symbol NAME slot, `T::compile_from_args`'s typed-entry kwargs
    /// gate) fires identically across all consumers of the named
    /// dispatcher family — fresh / preloaded × from-forms / from-source
    /// × constant / classifier — because every consumer routes through
    /// the SAME `expand_and_collect_named_calls_to_any` composition that
    /// composes `split_name_slot` with the per-form projection.
    ///
    /// Sibling of [`Self::expand_to_typed`] — both methods route
    /// through their constant-keyword Expander primitive sibling
    /// ([`Self::expand_and_collect_calls_to`] for the bare-kwargs row,
    /// [`Self::expand_and_collect_named_calls_to`] for the named row),
    /// each binding the per-form projection that fits its typed entry
    /// shape. Together with their from-source siblings they close the
    /// typed-from-`Expander` family.
    ///
    /// Theory anchor: see [`Self::expand_to_typed`] — the named sibling
    /// shares the same lift posture, threading the NAME-then-kwargs
    /// projection through `T` AND routing through the named
    /// constant-keyword primitive (rather than the bare-kwargs one
    /// with `named_form_projection::<T>` doing the NAME extraction
    /// inside the projection). THEORY.md §VI.1 — generation over
    /// composition; the named-form `split_name_slot` composition lives
    /// at ONE site post-lift rather than at TWO sites (the bare-kwargs
    /// path through `named_form_projection<T>` AND the classifier path
    /// through the `_any` primitive).
    pub fn expand_to_named<T: TataraDomain>(
        &mut self,
        forms: Vec<Sexp>,
    ) -> Result<Vec<NamedDefinition<T>>> {
        self.expand_and_collect_named_calls_to(forms, T::KEYWORD, |name, spec_args| {
            let spec = T::compile_from_args(spec_args)?;
            Ok(NamedDefinition {
                name: name.to_string(),
                spec,
            })
        })
    }

    /// Read + macroexpand + project every `(T::KEYWORD :k v …)` form in
    /// `src` into a typed `T` — the from-source posture of the typed
    /// bare-kwargs dispatcher family, sibling of
    /// [`Self::expand_source_to_named`].
    ///
    /// Composes [`crate::reader::read`] with [`Self::expand_to_typed`] —
    /// the typed-pair `(T::KEYWORD, T::compile_from_args)` is bound in
    /// ONE place (the from-forms row), and this from-source sibling
    /// inherits the binding through delegation. The expander posture
    /// (fresh [`Expander::new()`](crate::macro_expand::Expander::new)
    /// for one-shot typed compilation, preloaded
    /// [`self.preloaded.clone()`](crate::compiler_spec::RealizedCompiler)
    /// for compilation inside a CompilerSpec's macro library) is the
    /// caller's choice — this method binds the read step and dispatches
    /// on whichever `Expander` value the caller materialized.
    ///
    /// `?`-routing through `read` preserves the structural ordering of
    /// the rejection chain end-to-end: a reader error (lexer / parser /
    /// unbalanced-paren / unterminated-string) short-circuits BEFORE
    /// `expand_to_typed` runs; the from-forms posture's pipeline
    /// (`expand_program → iter_calls_to → map → collect`) fires
    /// afterwards exactly as it does for direct from-forms callers.
    pub fn expand_source_to_typed<T: TataraDomain>(&mut self, src: &str) -> Result<Vec<T>> {
        let forms = crate::reader::read(src)?;
        self.expand_to_typed::<T>(forms)
    }

    /// Read + macroexpand + project every `(T::KEYWORD NAME :k v …)` form
    /// in `src` into a typed [`NamedDefinition<T>`] — the from-source
    /// posture of the named NAME-then-kwargs dispatcher family, sibling
    /// of [`Self::expand_source_to_typed`].
    ///
    /// Composes [`crate::reader::read`] with [`Self::expand_to_named`] —
    /// the typed-pair `(T::KEYWORD, named_form_projection::<T>)` is bound
    /// in ONE place (the from-forms row), and this from-source sibling
    /// inherits the binding through delegation. Together with the three
    /// other cells of the family ([`Self::expand_to_typed`],
    /// [`Self::expand_to_named`], [`Self::expand_source_to_typed`]) it
    /// closes the typed-from-`Expander` surface across both input
    /// postures and both form shapes.
    pub fn expand_source_to_named<T: TataraDomain>(
        &mut self,
        src: &str,
    ) -> Result<Vec<NamedDefinition<T>>> {
        let forms = crate::reader::read(src)?;
        self.expand_to_named::<T>(forms)
    }
}

/// A typed definition with a positional name — e.g., `(defpoint NAME …)` →
/// `NamedDefinition<ProcessSpec> { name, spec }`.
#[derive(Debug, Clone)]
pub struct NamedDefinition<T> {
    pub name: String,
    pub spec: T,
}

/// Back-compat alias — the old `Definition` type was `NamedDefinition<ProcessSpec>`.
pub type Definition<T> = NamedDefinition<T>;

/// Read + macroexpand + compile every `(T::KEYWORD :k v …)` form into `T`.
pub fn compile_typed<T: TataraDomain>(src: &str) -> Result<Vec<T>> {
    let forms = read(src)?;
    let mut exp = Expander::new();
    let expanded = exp.expand_program(forms)?;
    let mut out = Vec::new();
    for form in &expanded {
        if let Some(list) = form.as_list() {
            if list.first().and_then(|s| s.as_symbol()) == Some(T::KEYWORD) {
                out.push(T::compile_from_args(&list[1..])?);
            }
        }
    }
    Ok(out)
}

/// Read + macroexpand + compile every `(T::KEYWORD NAME :k v …)` form into
/// `NamedDefinition<T>`. The positional `NAME` is captured separately from
/// the `:kw v` arguments that feed `compile_from_args`.
pub fn compile_named<T: TataraDomain>(src: &str) -> Result<Vec<NamedDefinition<T>>> {
    compile_named_from_forms::<T>(read(src)?)
}

/// Same as `compile_named` but operates on already-parsed forms. Useful when
/// the caller has done its own reading (e.g., from a string, a Sexp loaded
/// from disk, a macro-expanded subform).
pub fn compile_named_from_forms<T: TataraDomain>(
    forms: Vec<Sexp>,
) -> Result<Vec<NamedDefinition<T>>> {
    let mut exp = Expander::new();
    let expanded = exp.expand_program(forms)?;
    let mut out = Vec::new();
    for form in &expanded {
        let Some(list) = form.as_list() else { continue };
        if list.first().and_then(|s| s.as_symbol()) != Some(T::KEYWORD) {
            continue;
        }
        if list.len() < 2 {
            return Err(LispError::Compile {
                form: T::KEYWORD.to_string(),
                message: format!("expected ({} NAME …)", T::KEYWORD),
            });
        }
        let name = list[1]
            .as_symbol_or_string()
            .ok_or_else(|| LispError::Compile {
                form: T::KEYWORD.to_string(),
                message: "positional NAME must be a symbol or string".into(),
            })?
            .to_string();
        let spec = T::compile_from_args(&list[2..])?;
        out.push(NamedDefinition { name, spec });
    }
    Ok(out)
}

/// Split a `(<keyword> NAME …)` form's argument tail into the NAME slot
/// projection and the remaining argument tail — the named-form arity +
/// NAME-shape gate lifted out of `named_form_projection`'s inline body
/// into ONE public primitive on the substrate's `&[Sexp]` algebra,
/// independent of any `T: TataraDomain` typed-entry follow-up.
///
/// Composes the two-step structural rejection chain — `rest.split_first()`
/// arity gate → `as_symbol_or_string()` NAME-shape gate — yielding the
/// borrowed `(&'a str, &'a [Sexp])` pair on success: the NAME slot's
/// canonical symbol-or-string projection (sourced from
/// [`Sexp::as_symbol_or_string`], which accepts BOTH `(defcompiler
/// my-compiler …)` symbol-author and `(defcompiler "quoted-compiler"
/// …)` string-author surfaces) alongside the spec args tail (`&rest[1..]`,
/// the empty slice for a singleton like `(defcompiler my-compiler)`).
/// Both projections borrow from `rest` verbatim — no copy, no
/// allocation, same lifetime as [`Sexp::as_symbol_or_string`]'s tail —
/// so a consumer that wants to use the NAME slot as a lookup key (a
/// REPL completion that resolves a partial NAME against a registry, an
/// LSP that surfaces a tooltip for the NAME at hover, a
/// `tatara-check` diagnostic that quotes the NAME in its rendered
/// message) reaches the borrowed projection directly. Consumers that
/// need owned ownership (`NamedDefinition.name: String`,
/// JSON-serialized payloads, channel-bounded message bodies)
/// `.to_string()` themselves — pushing the clone to the consumer
/// boundary means the substrate primitive does NOT force a clone the
/// consumer doesn't need.
///
/// Before this lift the same two-step gate was welded INSIDE
/// `named_form_projection`'s body, immediately followed by the typed-
/// entry `T::compile_from_args` call. The pre-lift body had ONE
/// consumer (every named-form dispatcher in the matrix routed through
/// `named_form_projection::<T>` directly, which welded the gate with
/// the typed-domain compose). After this lift the gate is composable:
/// `named_form_projection` is now a 2-line composition of this
/// primitive with `T::compile_from_args`, and ANY consumer that wants
/// the named-form NAME extraction WITHOUT the typed-domain compose
/// binds to ONE primitive rather than re-deriving the
/// `split_first()` arity gate + `as_symbol_or_string()` shape gate +
/// `LispError::NamedFormMissingName` / `LispError::NamedFormNonSymbolName`
/// emission triple inline at its own call site.
///
/// `keyword: &'static str` is the canonical operator-position label
/// the named-form structural rejection variants
/// ([`LispError::NamedFormMissingName.keyword`],
/// [`LispError::NamedFormNonSymbolName.keyword`]) carry as `&'static
/// str` slots. Threading the `&'static` constraint through this
/// helper's parameter pins the same compile-time guarantee at the
/// boundary — a typo in the keyword can never drift into the
/// diagnostic at runtime, same posture as `MissingHeadSymbol.keyword`,
/// `HeadMismatch.keyword`, `TypeMismatch.expected`, and the
/// `Defmacro*.head` family. The pre-lift call sites bound the keyword
/// via `T::KEYWORD` (the typed-domain witness's canonical label); the
/// post-lift signature admits ANY `&'static str`, so a classifier
/// consumer that decodes the head to a typed kind whose label is
/// `&'static` (e.g. a `ClosedSet` implementor's `T::label()` or a
/// hand-rolled `&'static str` lookup) binds to ONE primitive without
/// requiring a `T: TataraDomain` witness.
///
/// Sibling of [`crate::ast::iter_calls_to`] /
/// [`crate::ast::iter_calls_to_any`] on the slice-side `&[Sexp]`
/// algebra — those primitives filter forms by keyword / classifier,
/// this primitive splits an already-filtered form's argument tail
/// into NAME + spec args. Together with [`Sexp::as_call`] /
/// [`Sexp::as_call_to`] / [`Sexp::as_call_to_any`] on the per-form
/// algebra, the substrate's named-form authoring surface decomposes
/// into ONE chain of named primitives the consumer composes per
/// call-site posture, instead of a four-step inline pipeline.
///
/// The future change that benefits: a `compile_named_any` family —
/// the (named NAME-then-kwargs × typed-decoded classifier) cell the
/// substrate's typed-dispatcher matrix leaves open today. A
/// classifier-NAME consumer composes
/// `expand_and_collect_calls_to_any(forms, decode_kind, |kind, args|
/// { let (name, spec_args) = split_name_slot(args, kind.label())?;
/// project(kind, name, spec_args) })` — the named-form gate is
/// COMPOSED in, not re-derived inline. A future named-classifier
/// primitive on `Expander` (a hypothetical
/// `expand_and_collect_named_calls_to_any`) would land as 3 lines on
/// top of `expand_and_collect_calls_to_any` + this primitive, without
/// re-deriving the gate.
///
/// Theory anchor: THEORY.md §VI.1 — generation over composition; the
/// named-form arity + NAME-shape gate is a NAMED primitive on the
/// `&[Sexp]` algebra, NOT a re-derived inline pipeline at every
/// named-form consumer site. The typed-domain compose (the
/// `T::compile_from_args` step inside `named_form_projection`)
/// follows AS A COMPOSITION of THIS primitive + the typed-entry gate,
/// not as a re-derivation of either. THEORY.md §II.1 invariant 2 —
/// free middle; both the typed-domain consumer
/// (`named_form_projection<T>`) AND any future classifier-NAME
/// consumer route through ONE gate body, so a regression in the gate
/// (a future debug-mode logger, span-aware borrow walker,
/// instrumentation that records every NAME-slot rejection for
/// telemetry) lands at ONE site the entire named-form authoring
/// surface inherits. THEORY.md §V.1 — knowable platform; the
/// named-form gate becomes a discoverable primitive on the
/// substrate's `&[Sexp]` algebra rather than an implementation
/// detail buried inside the typed-domain composition.
///
/// Frontier inspiration: Tree-sitter's `query` matched-set + capture
/// binding — a typed pattern exposes named CAPTURES that the
/// consumer references by binding; the NAME slot of a
/// `(<keyword> NAME …)` form is the substrate's typed peer of the
/// capture, exposed as a borrowed `&str` slot the caller composes
/// into its typed projection. Racket's `syntax-parse`
/// `(~datum keyword) name:id arg ...` matches the NAME slot through
/// the `name:id` capture binder and the consumer references it
/// downstream; `split_name_slot` is the unstructured-Rust peer with
/// the typed structural rejection chain (`NamedFormMissingName`,
/// `NamedFormNonSymbolName`) preserved across the boundary.
pub fn split_name_slot<'a>(
    rest: &'a [Sexp],
    keyword: &'static str,
) -> Result<(&'a str, &'a [Sexp])> {
    let (name_form, spec_args) = rest
        .split_first()
        .ok_or(LispError::NamedFormMissingName { keyword })?;
    let name =
        name_form
            .as_symbol_or_string()
            .ok_or_else(|| LispError::NamedFormNonSymbolName {
                keyword,
                got: name_form.shape(),
            })?;
    Ok((name, spec_args))
}

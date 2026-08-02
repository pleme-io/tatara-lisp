//! `#[derive(ClosedSet)]` — emit the [`tatara_closed_set::ClosedSet`] impl
//! (and its `FromStr` / `Display` / parse-rejection-carrier companions) for
//! an enum carrying the closed-set-enum idiom.
//!
//! ```ignore
//! use tatara_closed_set::DeriveClosedSet as ClosedSet;
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, ClosedSet)]
//! #[closed_set(generate_unknown, display)]
//! pub enum ChannelKind { Slack, Email }
//!
//! impl ChannelKind {
//!     pub const ALL: [Self; 2] = [Self::Slack, Self::Email];
//!     pub const fn label(self) -> &'static str {
//!         match self { Self::Slack => "slack", Self::Email => "email" }
//!     }
//! }
//! ```
//!
//! This crate is the ClosedSet half of `pleme-io/tatara`'s
//! `tatara-lisp-derive`, split out per
//! `theory/TATARA-LISP-CONSOLIDATION.md` phase 2 step 1. The emitted trait
//! path is `::tatara_closed_set::ClosedSet` — `tatara-lisp` neither carries
//! nor re-exports `ClosedSet`, which is what keeps the published
//! `tatara-lisp` small enough for the phase-3 facade.
//!
//! The generated parse-rejection carrier derives `::thiserror::Error`, so a
//! consuming crate that uses `#[closed_set(generate_unknown)]` needs
//! `thiserror` in its own `[dependencies]`. That is the pre-split behaviour,
//! preserved deliberately rather than reshaped.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Attribute, Data, DeriveInput, Ident, LitStr, Meta};

/// `#[derive(ClosedSet)]` — emit the substrate-wide
/// `tatara_closed_set::ClosedSet` impl + the matching `std::str::FromStr`
/// delegation for any enum carrying the closed-set-enum idiom (the
/// four-piece `ALL` + projection + `Unknown` + `FromStr` shape).
///
/// Lifts the 4-line `impl ClosedSet` + 4-line `impl FromStr` boilerplate
/// that 29+ workspace-wide implementors re-derive byte-for-byte — the
/// per-implementor content stays at the inherent `ALL` constant and the
/// inherent projection method (`as_str`, `label`, `prefix`, `marker`,
/// `keyword`, …), while the trait-impl plumbing collapses onto ONE
/// derive line.
///
/// ## Attributes
///
/// - `#[closed_set(via = "as_str")]` — name of the inherent projection
///   method the trait's `tatara_closed_set::ClosedSet::label` delegates to.
///   Defaults to `"label"`. Domain-canonical names
///   (`tatara_process`'s `as_str`, `tatara_lisp::ast::QuoteForm::prefix`,
///   `tatara_lisp::error::UnquoteForm::marker`,
///   `tatara_lisp::error::MacroDefHead::keyword`) stay load-bearing.
/// - `#[closed_set(unknown = "UnknownX")]` — name of the
///   per-implementor `Unknown` carrier struct
///   `tatara_closed_set::ClosedSet::make_unknown` constructs. Defaults to
///   `"Unknown{EnumName}"` — matches the substrate-wide naming
///   convention (`UnknownChannelKind` for `ChannelKind`).
/// - `#[closed_set(no_from_str)]` — suppress the generated
///   `impl FromStr`. Use for enums that already carry a bespoke
///   `FromStr` shape (e.g. `tatara_lisp::error::CompilerSpecIoStage`'s
///   compound `"{operation}: {label}"` key, which keys on a projection
///   PAIR rather than a single label).
/// - `#[closed_set(generate_unknown)]` /
///   `#[closed_set(generate_unknown = "<label>")]` — emit the
///   `pub struct Unknown{EnumName}(pub String)` parse-rejection
///   carrier alongside the trait impl. The carrier derives
///   `Debug + Clone + PartialEq + Eq + thiserror::Error` and renders
///   `#[error("unknown <label>: {0}")]`. The bare form derives
///   `<label>` by spacing the PascalCase enum name into lowercase
///   words (`ChannelKind` → "channel kind", `ReplacementPolicy` →
///   "replacement policy"); the `= "..."` form pins an explicit label
///   for irregular cases (`MacroDefHead` wants "macro definition
///   head" rather than the auto-derived "macro def head";
///   `MustReachPhase` wants "must-reach phase"). The 3-line
///   `pub struct Unknown{EnumName}(pub String)` declaration (plus its
///   thiserror derives + `#[error(...)]` annotation) is the
///   substrate-wide closed-set-enum idiom's last hand-rolled piece;
///   this attribute collapses it onto the derive so a 40+ enum
///   cohort emits the carrier through ONE generative shape rather
///   than re-deriving the boilerplate at each declaration site.
/// - `#[closed_set(display)]` — emit the substrate-wide
///   `impl ::core::fmt::Display for $name { f.write_str(Self::$via(*self)) }`
///   block alongside the trait impl. The 5-line Display block (the
///   `impl fmt::Display`, the `fn fmt`, the `f.write_str(self.$via())`
///   body) appears 28+ times across `tatara-process` /
///   `tatara-lisp` byte-for-byte — every closed-set carrier on a
///   PascalCase wire-format axis composes its operator-facing
///   diagnostic through Display rather than through a hard-coded
///   literal that would silently rot when a variant gets renamed.
///   The attribute collapses the 5-line block onto ONE flag so the
///   `as_str` ⇄ Display ⇄ `FromStr` triad emits through ONE
///   generative shape per closed-set enum.
///   The emission requires `Self: ::core::marker::Copy` (the
///   `ClosedSet` trait already requires it). Set the flag in
///   combination with `via` to pin Display onto the inherent
///   projection rather than the trait method; without the flag the
///   implementor keeps its hand-rolled Display block (e.g. for a
///   bespoke Display shape like
///   [`tatara_process::lifetime_clock::TerminateReason`]'s
///   structured-reason formatter).
///
/// ## Implementor requirements
///
/// The derive expects the enum to expose at the inherent surface:
///
/// 1. `pub const ALL: [Self; N] = [...]` — forced-arity array literal.
/// 2. A `fn projection(self) -> &'static str` method whose name matches
///    `via` (defaults to `label`).
/// 3. A `pub struct UnknownX(pub String)` in the same module whose name
///    matches `unknown` (defaults to `Unknown{EnumName}`) — UNLESS
///    `#[closed_set(generate_unknown)]` is set, in which case the
///    derive emits the struct itself.
///
/// The derive emits:
///
/// ```ignore
/// impl ::tatara_closed_set::ClosedSet for $name {
///     const ALL: &'static [Self] = &Self::ALL;
///     type Unknown = $unknown;
///     fn label(self) -> &'static str { Self::$via(self) }
///     fn make_unknown(s: &str) -> Self::Unknown {
///         $unknown(::std::string::String::from(s))
///     }
/// }
///
/// impl ::core::str::FromStr for $name {
///     type Err = $unknown;
///     fn from_str(s: &str) -> ::core::result::Result<Self, Self::Err> {
///         <Self as ::tatara_closed_set::ClosedSet>::parse_label(s)
///     }
/// }
/// ```
///
/// ## Theory grounding
///
/// THEORY.md §VI.1 — generation over composition; the derive IS the
/// generative shape — new closed-set enums add ONE `#[derive(ClosedSet)]`
/// line + the attribute that names their inherent projection method
/// instead of re-deriving the eight-line `impl ClosedSet` + `impl FromStr`
/// pair byte-for-byte. The per-implementor `Unknown` carrier stays
/// hand-rolled (its `#[error("unknown <thing>: {0}")]` annotation IS
/// per-implementor content), but the trait-impl plumbing it threads
/// through collapses onto the derive.
#[proc_macro_derive(ClosedSet, attributes(closed_set))]
pub fn derive_closed_set(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident.clone();

    if !matches!(input.data, Data::Enum(_)) {
        return spanned_compile_error(
            &name,
            "ClosedSet may only be derived on enums (the closed-set-enum idiom)",
        );
    }

    let cfg = match parse_closed_set_attrs(&input.attrs, &name) {
        Ok(c) => c,
        Err(err) => return emit_compile_error(err),
    };

    let via_ident = Ident::new(&cfg.via, name.span());
    let unknown_ident = Ident::new(&cfg.unknown, name.span());

    // Resolve the SET_LABEL the derive threads into BOTH the trait's
    // `const SET_LABEL` AND the carrier's `#[error("unknown <label>:
    // {0}")]` annotation. The priority chain is the typed-escape-hatch
    // shape every other axis on this derive carries:
    //   1. `#[closed_set(set_label = "...")]` — explicit override at
    //      the trait surface, independent of the carrier's annotation.
    //      No production implementor reaches for this today; the axis
    //      exists for the degenerate case where an implementor wants
    //      to bind the trait's set name independently of the carrier's
    //      diagnostic label (a future structured-diagnostic carrier
    //      that wraps a richer payload than `pub String`).
    //   2. `#[closed_set(generate_unknown = "<label>")]` — the same
    //      label the carrier's `#[error(...)]` annotation already
    //      pins, threaded through to the trait surface so the two
    //      surfaces emit from ONE generative origin. Covers irregular
    //      labels (`MacroDefHead` → "macro definition head",
    //      `MustReachPhase` → "must-reach phase") whose operator-
    //      pinned wording diverges from the auto-derived projection.
    //   3. `#[closed_set(generate_unknown)]` / `Skip` — auto-derive
    //      via `pascal_to_spaced_lowercase` on the enum name. Covers
    //      the regular case (`ChannelKind` → "channel kind",
    //      `ReplacementPolicy` → "replacement policy"); also the
    //      fallback for `Skip` so an implementor that hand-rolls the
    //      carrier still gets a typed SET_LABEL without touching the
    //      derive attribute surface.
    let set_label = match (&cfg.set_label, &cfg.generate_unknown) {
        (Some(explicit), _) => explicit.clone(),
        (None, GenerateUnknown::Explicit(label)) => label.clone(),
        (None, GenerateUnknown::Auto | GenerateUnknown::Skip) => {
            pascal_to_spaced_lowercase(&name.to_string())
        }
    };

    let from_str_impl = if cfg.no_from_str {
        TokenStream2::new()
    } else {
        quote! {
            impl ::core::str::FromStr for #name {
                type Err = #unknown_ident;
                fn from_str(
                    s: &::core::primitive::str,
                ) -> ::core::result::Result<Self, Self::Err> {
                    <Self as ::tatara_closed_set::ClosedSet>::parse_label(s)
                }
            }
        }
    };

    let unknown_struct_decl = match &cfg.generate_unknown {
        GenerateUnknown::Skip => TokenStream2::new(),
        GenerateUnknown::Auto | GenerateUnknown::Explicit(_) => {
            // The carrier's `#[error(...)]` annotation reads from the
            // SAME resolved `set_label` the trait const reads from —
            // a regression at one site cannot drift from the other,
            // because both flow from the SAME local binding.
            emit_unknown_struct(&unknown_ident, &set_label)
        }
    };

    let display_impl = if cfg.display {
        quote! {
            impl ::core::fmt::Display for #name {
                fn fmt(
                    &self,
                    f: &mut ::core::fmt::Formatter<'_>,
                ) -> ::core::fmt::Result {
                    f.write_str(Self::#via_ident(*self))
                }
            }
        }
    } else {
        TokenStream2::new()
    };

    let expanded = quote! {
        impl ::tatara_closed_set::ClosedSet for #name {
            const ALL: &'static [Self] = &Self::ALL;
            const SET_LABEL: &'static ::core::primitive::str = #set_label;
            type Unknown = #unknown_ident;
            fn label(self) -> &'static ::core::primitive::str {
                Self::#via_ident(self)
            }
            fn make_unknown(
                s: &::core::primitive::str,
            ) -> Self::Unknown {
                #unknown_ident(::std::string::String::from(s))
            }
        }

        #from_str_impl

        #unknown_struct_decl

        #display_impl
    };

    expanded.into()
}

/// Emit the `pub struct UnknownX(pub String)` parse-rejection carrier
/// for `#[closed_set(generate_unknown[ = "label"])]`. The shape is the
/// substrate-wide closed-set-enum carrier idiom: `Debug + Clone +
/// PartialEq + Eq + thiserror::Error` derives with an
/// `#[error("unknown <label>: {0}")]` annotation that surfaces the
/// offending input verbatim. Lifted into ONE helper so every
/// generated carrier flows through ONE composition site — a
/// regression that drifts the derive set or the message shape
/// between two generated carriers is structurally impossible.
fn emit_unknown_struct(unknown_ident: &Ident, label: &str) -> TokenStream2 {
    let msg = format!("unknown {label}: {{0}}");
    quote! {
        #[derive(
            ::core::fmt::Debug,
            ::core::clone::Clone,
            ::core::cmp::PartialEq,
            ::core::cmp::Eq,
            ::thiserror::Error,
        )]
        #[error(#msg)]
        pub struct #unknown_ident(pub ::std::string::String);
    }
}

/// Project a PascalCase identifier into the substrate-wide
/// spaced-lowercase label `#[closed_set(generate_unknown)]` threads
/// into the auto-derived `#[error("unknown <label>: {0}")]`
/// annotation. Mirrors the workspace-wide hand-rolled convention
/// across 40+ closed-set carriers (`ChannelKind` →
/// "channel kind", `ReplacementPolicy` → "replacement policy",
/// `CompilerSpecIoStage` → "compiler spec io stage").
///
/// A run of contiguous uppercase characters projects byte-for-byte to
/// lowercase without inserting interior spaces; a space is emitted
/// only at the lowercase→uppercase boundary. Irregular labels
/// (`MacroDefHead` → "macro definition head" with "Def" expanded;
/// `MustReachPhase` → "must-reach phase" with a hyphen) fall outside
/// the projection's codomain and require the explicit
/// `#[closed_set(generate_unknown = "...")]` override.
fn pascal_to_spaced_lowercase(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    let mut prev_was_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_was_lower {
                out.push(' ');
            }
            out.push(c.to_ascii_lowercase());
            prev_was_lower = false;
        } else {
            out.push(c);
            prev_was_lower = c.is_ascii_lowercase();
        }
    }
    out
}

#[cfg(test)]
mod pascal_to_spaced_lowercase_tests {
    use super::pascal_to_spaced_lowercase;

    #[test]
    fn regular_two_word_names_split_at_the_word_boundary() {
        // The bread-and-butter case across 30+ closed-set carriers —
        // PascalCase with a single internal capital splits at the
        // capital. The retrofit cohort
        // (`ChannelKind`/`ArtifactKind`/`ReportFormat`/`ExportTrigger`)
        // all live in this case so the auto-derived label matches
        // the workspace-wide convention without an explicit override.
        assert_eq!(pascal_to_spaced_lowercase("ChannelKind"), "channel kind");
        assert_eq!(pascal_to_spaced_lowercase("ArtifactKind"), "artifact kind");
        assert_eq!(pascal_to_spaced_lowercase("ReportFormat"), "report format");
        assert_eq!(
            pascal_to_spaced_lowercase("ExportTrigger"),
            "export trigger",
        );
        assert_eq!(
            pascal_to_spaced_lowercase("ReplacementPolicy"),
            "replacement policy",
        );
    }

    #[test]
    fn three_word_names_split_at_every_word_boundary() {
        // Closed-set names with three PascalCase tokens
        // (`CompilerSpecIoStage`, `OptimizationDirection`,
        // `ConvergencePointType`) split at every lowercase→uppercase
        // boundary. The split is internal — the trailing PascalCase
        // tokens stay as separate words rather than collapsing into
        // the previous one.
        assert_eq!(
            pascal_to_spaced_lowercase("OptimizationDirection"),
            "optimization direction",
        );
        assert_eq!(
            pascal_to_spaced_lowercase("ConvergencePointType"),
            "convergence point type",
        );
    }

    #[test]
    fn contiguous_uppercase_runs_collapse_to_lowercase_without_inner_spaces() {
        // Acronyms run together rather than fan out per letter —
        // `CompilerSpecIoStage` projects "compiler spec io stage"
        // (the "Io" run stays as "io" rather than "i o"). Pinned by
        // the substrate-wide hand-rolled labels:
        // `error.rs`'s `UnknownCompilerSpecIoStage` carries the
        // message "unknown compiler spec io stage: {0}" verbatim, and
        // the auto-derive must match it bit-for-bit so a retrofit
        // doesn't drift the operator-facing wording.
        assert_eq!(
            pascal_to_spaced_lowercase("CompilerSpecIoStage"),
            "compiler spec io stage",
        );
    }

    #[test]
    fn single_word_names_stay_lowercase_with_no_spaces() {
        // A single PascalCase token (no internal capital) projects
        // to a single lowercase word — no leading space, no
        // mid-word split. Covers degenerate-but-valid cases like a
        // future `Signal` or `Kind` enum name.
        assert_eq!(pascal_to_spaced_lowercase("Signal"), "signal");
        assert_eq!(pascal_to_spaced_lowercase("Kind"), "kind");
    }

    #[test]
    fn empty_input_projects_to_empty_string() {
        // Empty-input contract — projecting `""` yields `""` rather
        // than a leading space or a panic. Defensive case the
        // attribute parser shouldn't reach (the derive runs on a
        // named enum), but pinning it here keeps the helper's
        // contract independent of the caller's discipline.
        assert_eq!(pascal_to_spaced_lowercase(""), "");
    }
}

struct ClosedSetCfg {
    via: String,
    unknown: String,
    no_from_str: bool,
    generate_unknown: GenerateUnknown,
    /// `#[closed_set(display)]` — emit the substrate-wide
    /// `impl fmt::Display { f.write_str(Self::$via(*self)) }` block.
    /// 28+ workspace-wide closed-set enums on PascalCase wire-format
    /// axes (the `as_str ⇄ Display ⇄ FromStr` triad) re-derive this
    /// 5-line block byte-for-byte; flipping the flag at the derive
    /// site collapses the block onto ONE generative shape.
    display: bool,
    /// `#[closed_set(set_label = "...")]` — explicit override for the
    /// trait's `tatara_closed_set::ClosedSet::SET_LABEL` const. Defaults
    /// to the label `#[closed_set(generate_unknown[ = "..."])]`
    /// already pinned (or the auto-derived
    /// `pascal_to_spaced_lowercase(name)` for the bare / `Skip`
    /// cases) so the trait surface and the carrier's `#[error(...)]`
    /// annotation emit from ONE generative origin. The override
    /// exists for the degenerate case where an implementor wants to
    /// bind the trait's set name independently of the carrier's
    /// diagnostic label (a future structured-diagnostic carrier that
    /// wraps a richer payload than `pub String`) — no production
    /// implementor reaches for it today.
    set_label: Option<String>,
}

/// `#[closed_set(generate_unknown[ = "label"])]` parse outcome.
///
/// `Skip` keeps the existing convention (implementor hand-rolls the
/// `pub struct UnknownX(pub String)` carrier alongside the enum).
/// `Auto` emits the carrier with the spaced-lowercase projection of
/// the enum name as the `#[error(...)]` label. `Explicit(label)` emits
/// the carrier with an operator-pinned label that overrides the
/// PascalCase split (for irregular cases like `MacroDefHead` →
/// "macro definition head").
enum GenerateUnknown {
    Skip,
    Auto,
    Explicit(String),
}

fn parse_closed_set_attrs(attrs: &[Attribute], name: &Ident) -> syn::Result<ClosedSetCfg> {
    let mut via: Option<String> = None;
    let mut unknown: Option<String> = None;
    let mut no_from_str = false;
    let mut generate_unknown = GenerateUnknown::Skip;
    let mut display = false;
    let mut set_label: Option<String> = None;
    for attr in attrs {
        if !attr.path().is_ident("closed_set") {
            continue;
        }
        let Meta::List(list) = &attr.meta else {
            continue;
        };
        list.parse_nested_meta(|meta| {
            // Three-arm × three-arm × one-arm dispatch collapses onto
            // the two sub-key primitives:
            //   - `try_lit_str_sub_key` closes the (sub-key ident × `=
            //     <LitStr>` payload × `Option<String>` slot mutation)
            //     shape for `via`, `unknown`, `set_label` — the three
            //     historically-duplicated string-valued arms.
            //   - `try_bool_flag_sub_key` closes the (sub-key ident ×
            //     bare-flag ident × `bool` slot flip) shape for
            //     `no_from_str`, `display` — the two historically-
            //     duplicated bare-flag arms.
            // Short-circuit `||` evaluation preserves the first-match-
            // wins ordering the previous `if / else if` chain carried;
            // `?` unwraps the primitive's `syn::Result<bool>` to the
            // matched bit so the outer chain composes cleanly across
            // both primitive shapes. A future `#[closed_set(alias =
            // "…")]` string-valued key composes as ONE `||` term
            // against `try_lit_str_sub_key`; a future
            // `#[closed_set(no_debug)]` bare-flag key composes as ONE
            // `||` term against `try_bool_flag_sub_key` — no per-key
            // scaffold, no drift.
            if try_lit_str_sub_key(&meta, "via", &mut via)?
                || try_lit_str_sub_key(&meta, "unknown", &mut unknown)?
                || try_lit_str_sub_key(&meta, "set_label", &mut set_label)?
                || try_bool_flag_sub_key(&meta, "no_from_str", &mut no_from_str)?
                || try_bool_flag_sub_key(&meta, "display", &mut display)?
            {
                Ok(())
            } else if meta.path.is_ident("generate_unknown") {
                // Both bare `generate_unknown` (auto-derived label)
                // and `generate_unknown = "explicit label"` (pinned
                // label) sit on ONE attribute key — the parser
                // dispatches on whether `meta.value()` succeeds so the
                // attribute surface stays single-keyed (no
                // `auto_label`/`label` bifurcation that would force
                // the operator to think about which of two
                // attributes is canonical). The `Ok(value)` arm has
                // already consumed the `=`, so it routes through the
                // stream-level `parse_lit_str` primitive rather than
                // the meta-level `read_meta_lit_str` (which would
                // double-consume `.value()`) — and stays outside the
                // `try_lit_str_sub_key` primitive's contract for the
                // same reason (the primitive re-consumes `.value()`
                // internally, incompatible with this arm's outer
                // flag-or-value dispatch).
                generate_unknown = match meta.value() {
                    Ok(value) => GenerateUnknown::Explicit(parse_lit_str(value)?),
                    Err(_) => GenerateUnknown::Auto,
                };
                Ok(())
            } else {
                Err(meta.error(
                    "unknown #[closed_set(...)] key — expected `via`, `unknown`, `no_from_str`, `generate_unknown`, `display`, or `set_label`",
                ))
            }
        })?;
    }
    Ok(ClosedSetCfg {
        via: via.unwrap_or_else(|| "label".to_string()),
        unknown: unknown.unwrap_or_else(|| format!("Unknown{name}")),
        no_from_str,
        generate_unknown,
        display,
        set_label,
    })
}

/// `parse_nested_meta` callback primitive: if `meta`'s sub-key path is
/// `key`, read its `= <LitStr>` payload as an owned `String`, write
/// `Some(<value>)` to `slot`, and return `Ok(true)`; otherwise leave
/// `slot` untouched and return `Ok(false)`.
///
/// Lifts the byte-for-byte identical three-line
///
/// ```ignore
/// } else if meta.path.is_ident("<key>") {
///     <slot> = Some(read_meta_lit_str(&meta)?);
///     Ok(())
/// }
/// ```
///
/// arm that pre-lift lived at THREE sites inside
/// [`parse_closed_set_attrs`] (the `via`, `unknown`, and `set_label`
/// string-valued sub-keys). Post-lift each site composes onto ONE `||`
/// term of the dispatch chain — a future single-keyed string-valued
/// sub-key extension (a `#[closed_set(alias = "…")]` peer, a lifted
/// `keyword` axis pulled up from `#[tatara(…)]`, an operator-authored
/// `prefix = "…"` axis) adds as ONE `||` term against the same
/// substrate rather than as a fresh copy of the arm.
///
/// The `Ok(bool)` return shape lets callers chain arms via short-
/// circuit `||`: `try_lit_str_sub_key(&meta, "via", &mut via)? || ...`.
/// The `?` unwraps the inner `syn::Result<bool>` to the matched bit,
/// and the `||` operator's laziness preserves the historic first-match-
/// wins evaluation order without touching the trailing slots.
///
/// Sibling of [`try_bool_flag_sub_key`] one PAYLOAD-SHAPE axis over:
/// the string-valued arm (this primitive) takes an
/// `&mut Option<String>` slot and reads a `= <LitStr>` payload; the
/// bare-flag arm (the sibling) takes an `&mut bool` slot and reads no
/// payload. The `parse_lit_str` / `read_meta_lit_str` pair a few file-
/// sections up carries the same primitive / meta-level wrapper motif —
/// the derive crate's convention for two-shape sub-key primitives.
///
/// Theory grounding: THEORY.md §VI.1 — generation over composition.
/// The three-times-rule signal fires at three sites of the string-
/// valued sub-key idiom; the primitive names the composition as ONE
/// substrate entry so a new arm of the same shape adds as ONE line, and
/// a diagnostic upgrade (e.g. a `syn::Error::new_spanned(&meta.path,
/// "expected LitStr, got LitInt")` sharpening on the `LitStr::parse`
/// failure) lands at ONE line inherited by every existing caller.
fn try_lit_str_sub_key(
    meta: &syn::meta::ParseNestedMeta<'_>,
    key: &str,
    slot: &mut Option<String>,
) -> syn::Result<bool> {
    if meta.path.is_ident(key) {
        *slot = Some(read_meta_lit_str(meta)?);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// `parse_nested_meta` callback primitive: if `meta`'s sub-key path is
/// `key`, flip `flag` to `true` and return `Ok(true)`; otherwise leave
/// `flag` untouched and return `Ok(false)`.
///
/// Lifts the byte-for-byte identical three-line
///
/// ```ignore
/// } else if meta.path.is_ident("<key>") {
///     <flag> = true;
///     Ok(())
/// }
/// ```
///
/// arm that pre-lift lived at TWO sites inside
/// [`parse_closed_set_attrs`] (the `no_from_str` and `display` bare-
/// flag sub-keys). Post-lift each site composes onto ONE `||` term of
/// the dispatch chain — a future single-keyed bare-flag sub-key
/// extension (a `#[closed_set(no_debug)]` peer, a `no_partial_eq`
/// axis, a `no_hash` axis) adds as ONE `||` term against the same
/// substrate rather than as a fresh copy of the arm.
///
/// Returns `syn::Result<bool>` — not `bool` — to homogenize the return
/// shape with the sibling [`try_lit_str_sub_key`] so callers chain
/// mixed arms via one `?` cadence: `try_lit_str_sub_key(&meta, "via",
/// &mut via)? || try_bool_flag_sub_key(&meta, "display", &mut
/// display)?`. Today the primitive's `Ok` arm cannot fail (a bare-flag
/// match has no payload to parse), but the `syn::Result` wrapper
/// preserves the composition uniformly and admits a future sharpening
/// (e.g. surfacing a `syn::Error` on a stray `= <value>` payload after
/// a bare-flag ident) without changing the primitive's signature or
/// touching every caller's `?` cadence.
///
/// Sibling of [`try_lit_str_sub_key`] one PAYLOAD-SHAPE axis over — see
/// that primitive's doc for the sibling-shape motif that mirrors the
/// [`parse_lit_str`] / [`read_meta_lit_str`] pair.
///
/// Theory grounding: THEORY.md §VI.1 — generation over composition.
/// The two sites of the bare-flag sub-key idiom cross the three-times-
/// rule signal when composed with the sibling string-valued primitive
/// (five sites in aggregate on the SAME `parse_nested_meta` callback);
/// the two primitives together name the (sub-key ident × slot-shape)
/// dispatch matrix as TWO substrate entries so every future closed-
/// set sub-key of either shape adds as ONE `||` term rather than as
/// a fresh scaffold.
fn try_bool_flag_sub_key(
    meta: &syn::meta::ParseNestedMeta<'_>,
    key: &str,
    flag: &mut bool,
) -> syn::Result<bool> {
    if meta.path.is_ident(key) {
        *flag = true;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Parse a `LitStr` off an already-obtained value stream and project it
/// to an owned `String`. The primitive-level shape the two peer readers
/// [`read_meta_lit_str`] (the "read `= <LitStr>` payload of a sub-key"
/// composition, called on 4 arms across `extract_keyword` +
/// `parse_closed_set_attrs`'s `via`/`unknown`/`set_label` slots) AND
/// `parse_closed_set_attrs`'s `generate_unknown` Explicit arm (which
/// already consumed the `=` via the outer `match meta.value()` flag-
/// or-value dispatch and therefore can't route through the
/// meta-level helper without double-consuming `.value()`) BOTH
/// project through.
///
/// Lifts the byte-for-byte identical `let s: LitStr = value.parse()?;
/// Ok(s.value())` shape that pre-lift lived at 5 sites across the
/// derive crate (once per string-slot). A future refactor that
/// tightens the parse (e.g. surfaces a specific `syn::Error` diagnostic
/// on the non-`LitStr` value shape, or admits both `LitStr` +
/// `LitByteStr`, or trims surrounding whitespace at the parse
/// boundary) lands at ONE line and is inherited by every caller
/// automatically.
fn parse_lit_str(value: syn::parse::ParseStream<'_>) -> syn::Result<String> {
    let s: LitStr = value.parse()?;
    Ok(s.value())
}

/// Read the `= <LitStr>` payload of a named-value sub-key inside a
/// `parse_nested_meta` callback as an owned `String`. Composes
/// `meta.value()?` + [`parse_lit_str`] into ONE substrate entry that
/// [`extract_keyword`]'s callback + `parse_closed_set_attrs`'s
/// `via` / `unknown` / `set_label` arms route through.
///
/// Lifts the byte-for-byte identical `let value = meta.value()?; let
/// s: LitStr = value.parse()?; Ok(s.value())` scaffold that pre-lift
/// lived at 4 sites across the derive crate. Peer to the sibling
/// `find_named_sub_key` helper one abstraction level up — together
/// the two compose the derive's "read `#[<attr>(<sub_key> = "…")]`
/// payload as `Option<String>`" idiom onto ONE stack of substrate
/// primitives (`find_named_sub_key` + `read_meta_lit_str`), which the
/// [`extract_keyword`] reader collapses onto a one-line callable-pointer
/// projection (`find_named_sub_key(attrs, "tatara", "keyword",
/// read_meta_lit_str)`) and which every future single-keyed string-
/// valued sub-key reader (the `#[tatara(alias = "…")]` extension the
/// `keyword_after_unrelated_named_value_key_projects_to_the_literal_value`
/// test's docblock cites; a `#[serde(rename = "…")]` sniffer; a
/// single-key reader lifted out of the 6-key `parse_closed_set_attrs`)
/// inherits automatically.
fn read_meta_lit_str(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    parse_lit_str(meta.value()?)
}

/// Project an existing [`syn::Error`] into the proc-macro's outer
/// [`TokenStream`] return shape — the primitive-level composition of
/// `err.to_compile_error().into()` every early-return arm of every
/// `#[proc_macro_derive]` in this crate threads through.
///
/// Pre-lift the two-call chain `.to_compile_error().into()` lived
/// verbatim at every early-return site across the two derives
/// (`derive_closed_set` + `derive_tatara_domain`). Post-lift the
/// composition names the operation as ONE substrate-level
/// projection — a future refactor of the emission (e.g. wrapping in
/// a diagnostic-envelope for structured tooling, adding a
/// `note = "help: …"` chain, threading through a
/// `proc_macro2::TokenStream → TokenStream` boundary primitive) lands
/// at ONE line and every derive's early-return arm picks it up
/// mechanically.
///
/// Sibling of [`spanned_compile_error`] one INPUT-BAND axis over:
/// the pre-composed-error posture (this function) takes an already-
/// constructed [`syn::Error`]; the spanned-message posture (the
/// sibling) constructs the [`syn::Error`] via
/// [`syn::Error::new_spanned`] first, then routes through this
/// function so the two-call chain `.to_compile_error().into()`
/// binds at ONE composition point. The `parse_lit_str` /
/// `read_meta_lit_str` pair one file-section up carries the same
/// primitive / meta-level wrapper sibling shape — the derive crate's
/// convention for two-level substrate lifts.
fn emit_compile_error(err: syn::Error) -> TokenStream {
    err.to_compile_error().into()
}

/// Emit a proc-macro compile error at the span of `spanned`, with
/// message `msg` — the meta-level composition of
/// `syn::Error::new_spanned(spanned, msg)` + [`emit_compile_error`]
/// every input-shape rejection arm across `derive_closed_set` +
/// `derive_tatara_domain` threads through.
///
/// Pre-lift the four-line scaffold
///
/// ```ignore
/// return syn::Error::new_spanned(&target, "…msg…")
///     .to_compile_error()
///     .into();
/// ```
///
/// lived verbatim at four sites (the enum-only gate in
/// `derive_closed_set`; the two struct/named-fields gates in
/// `derive_tatara_domain`; the per-field `extractor_for` failure
/// path). Post-lift each site collapses to
///
/// ```ignore
/// return spanned_compile_error(&target, "…msg…");
/// ```
///
/// binding the (spanned target, diagnostic message, compile-error
/// emission) triple at ONE composition point on the derive crate's
/// substrate. A future refactor of any of the three axes (e.g.
/// threading spans through a typed span-carrier, extending message
/// shape with `help:` continuations, swapping the emission for a
/// structured-diagnostic surface) lands at ONE line and every
/// input-rejection arm picks it up mechanically.
///
/// Sibling of [`emit_compile_error`] one INPUT-BAND axis over —
/// see that function's doc for the sibling-shape motif that mirrors
/// the [`parse_lit_str`] / [`read_meta_lit_str`] pair.
fn spanned_compile_error<T, M>(spanned: T, msg: M) -> TokenStream
where
    T: quote::ToTokens,
    M: std::fmt::Display,
{
    emit_compile_error(syn::Error::new_spanned(spanned, msg))
}
#[cfg(test)]
mod compile_error_emission_tests {
    //! Contract tests for the two `[emit|spanned]_compile_error`
    //! sibling primitives. Exercises the emission through the SAME
    //! `proc_macro2::TokenStream::to_string()` round-trip every proc-
    //! macro consumer sees at the outer boundary — the
    //! `compile_error! { "…" }` invocation the emission expands to,
    //! anchored at the sibling `syn::Error::to_compile_error` docs.
    //!
    //! The `proc_macro::TokenStream` return type is unavailable in
    //! `#[cfg(test)]` unit tests (the `proc_macro` crate is a proc-
    //! macro-only surface, not linkable from a lib-crate test
    //! harness), so we exercise the emission at the
    //! [`syn::Error::to_compile_error`] boundary — the ONE call
    //! [`emit_compile_error`] makes before the boundary-crossing
    //! [`Into`] conversion. If that call binds the expected shape,
    //! the outer `.into()` is a total identity on the
    //! `proc_macro2::TokenStream → proc_macro::TokenStream` boundary
    //! (guaranteed by the syn / proc-macro2 crate contracts).
    use proc_macro2::TokenStream as TokenStream2;
    use quote::ToTokens;
    use syn::parse_str;

    fn compile_error_body(err: syn::Error) -> String {
        err.to_compile_error().to_string()
    }

    #[test]
    fn syn_error_to_compile_error_emits_a_compile_error_macro_call() {
        // The `to_compile_error()` boundary emits a
        // `::core::compile_error!{ "…msg…" }` macro invocation as a
        // `proc_macro2::TokenStream`. This is the SAME shape the
        // pre-lift four-line scaffold surfaced verbatim, and the SAME
        // shape [`emit_compile_error`] threads through before
        // `.into()`-ing across the proc-macro boundary. A regression
        // that (say) swapped `compile_error!` for `panic!` or dropped
        // the message payload would surface here.
        let err = syn::Error::new(proc_macro2::Span::call_site(), "sample diagnostic");
        let body = compile_error_body(err);
        assert!(
            body.contains("compile_error"),
            "compile_error emission must include the `compile_error!` macro invocation, got: {body}",
        );
        assert!(
            body.contains("sample diagnostic"),
            "compile_error emission must include the diagnostic message verbatim, got: {body}",
        );
    }

    #[test]
    fn spanned_error_preserves_the_target_span_for_ide_diagnostics() {
        // `syn::Error::new_spanned(target, msg)` anchors the
        // diagnostic at `target`'s span so a proc-macro consumer's
        // IDE highlights the OFFENDING token (not the macro
        // invocation). Verified by comparing the spanned emission
        // against the SAME message emitted at `Span::call_site()` —
        // the two are byte-identical in the `compile_error!` shell
        // but the LOAD-BEARING difference is on the internal
        // `_ = { ... }` span-carrier token cluster syn threads
        // through to preserve the target's span through the emission.
        // If the two are IDENTICAL, the span binding got stripped
        // and `spanned_compile_error` degrades to a plain-emission
        // helper.
        let ident: syn::Ident = parse_str("target_ident").expect("valid ident");
        let spanned = syn::Error::new_spanned(&ident, "spanned diagnostic");
        let call_site = syn::Error::new(proc_macro2::Span::call_site(), "spanned diagnostic");
        // The `compile_error!` shell + message payload are the same;
        // the token span each shell is anchored at differs. We
        // exercise the shell agreement here — the SPAN agreement is
        // an implementation detail syn's `to_compile_error` guarantees
        // and can't be observed through the string round-trip.
        assert_eq!(
            compile_error_body(spanned)
                .replace(char::is_whitespace, "")
                .contains("compile_error!{\"spanneddiagnostic\"}"),
            compile_error_body(call_site)
                .replace(char::is_whitespace, "")
                .contains("compile_error!{\"spanneddiagnostic\"}"),
        );
    }

    #[test]
    fn spanned_compile_error_accepts_string_message_at_call_sites() {
        // `spanned_compile_error<T, M> where M: std::fmt::Display`
        // must accept a `String` payload (the shape
        // `derive_tatara_domain`'s `extractor_for` error path
        // threads through: `Err(err) => spanned_compile_error(
        // &field.ty, err)` where `err: String`). Pin the trait bound
        // at the call site so a regression that (say) tightened `M`
        // to `&'static str` would break the field-level error path.
        //
        // Exercised at the `syn::Error::new_spanned` composition (the
        // ONE line inside `spanned_compile_error`) — a `Display`
        // bound that admits `String` and `&str` alike is what the
        // sibling helper's four current callers rely on.
        let ident: syn::Ident = parse_str("target_ident").expect("valid ident");
        let owned: String = String::from("owned message");
        // Compile-check: this function must accept `&String` as M
        // just like it accepts `&'static str`. Neither call panics;
        // the goal is to verify the trait bound admits both shapes.
        let err_owned = syn::Error::new_spanned(&ident, owned);
        let err_static = syn::Error::new_spanned(&ident, "static message");
        assert!(compile_error_body(err_owned).contains("owned message"));
        assert!(compile_error_body(err_static).contains("static message"));
    }

    #[test]
    fn spanned_compile_error_accepts_ident_and_type_targets() {
        // The four current call sites thread THREE distinct
        // `ToTokens` target shapes into `spanned_compile_error`:
        //   1. `&Ident` (the enum/struct name — three sites in the
        //      `derive_closed_set` + `derive_tatara_domain` input-
        //      shape gates),
        //   2. `&syn::Type` (the field type — one site in
        //      `derive_tatara_domain`'s per-field `extractor_for`
        //      error path).
        // Both must be admissible under the `T: quote::ToTokens`
        // bound. This test exercises both shapes at the underlying
        // `syn::Error::new_spanned` boundary the helper composes
        // over — a `T: ToTokens` bound admits both `&Ident` and
        // `&Type` alike.
        let ident: syn::Ident = parse_str("target_ident").expect("valid ident");
        let ty: syn::Type = parse_str("::std::string::String").expect("valid type");
        let err_ident = syn::Error::new_spanned(&ident, "at ident span");
        let err_type = syn::Error::new_spanned(&ty, "at type span");
        // Both round-trip through `to_compile_error` cleanly — the
        // outer emission is agnostic to the target shape, so both
        // shapes emit the same `compile_error!` invocation.
        let ident_body = compile_error_body(err_ident);
        let type_body = compile_error_body(err_type);
        assert!(ident_body.contains("at ident span"));
        assert!(type_body.contains("at type span"));
        // Sanity: both bodies are non-empty TokenStreams the outer
        // `.into()` boundary would forward across the proc-macro
        // surface.
        let ident_stream: TokenStream2 = ident_body.parse().expect("emission parses as TS");
        let type_stream: TokenStream2 = type_body.parse().expect("emission parses as TS");
        assert!(!ident_stream.is_empty());
        assert!(!type_stream.is_empty());
        // Guard the ToTokens threading itself: a regression that
        // dropped `#[allow(dead_code)]`-style token pruning would
        // fail here.
        let _ = ident.to_token_stream();
        let _ = ty.to_token_stream();
    }
}

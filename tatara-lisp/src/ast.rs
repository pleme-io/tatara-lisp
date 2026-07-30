//! S-expression AST.

use crate::error::{SexpShape, UnquoteForm};
use std::fmt;
use std::hash::{Hash, Hasher};

// `Sexp` is `PartialEq` but not `Eq` (Float contains NaN). We implement Hash
// manually so cache keys can hash a borrowed `&[Sexp]` directly — avoids the
// serde_json serialization that would otherwise dominate cache overhead on
// cheap macro calls.
impl Hash for Sexp {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match self {
            Self::Nil => 0u8.hash(h),
            Self::Atom(a) => {
                1u8.hash(h);
                a.hash(h);
            }
            Self::List(items) => {
                2u8.hash(h);
                items.len().hash(h);
                for i in items {
                    i.hash(h);
                }
            }
            Self::Quote(inner) => {
                3u8.hash(h);
                inner.hash(h);
            }
            Self::Quasiquote(inner) => {
                4u8.hash(h);
                inner.hash(h);
            }
            Self::Unquote(inner) => {
                5u8.hash(h);
                inner.hash(h);
            }
            Self::UnquoteSplice(inner) => {
                6u8.hash(h);
                inner.hash(h);
            }
        }
    }
}

impl Hash for Atom {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match self {
            Self::Symbol(s) => {
                0u8.hash(h);
                s.hash(h);
            }
            Self::Keyword(s) => {
                1u8.hash(h);
                s.hash(h);
            }
            Self::Str(s) => {
                2u8.hash(h);
                s.hash(h);
            }
            Self::Int(n) => {
                3u8.hash(h);
                n.hash(h);
            }
            // Float: hash the bit pattern. NaN != NaN so PartialEq is broken,
            // but cache lookups use PartialEq-by-hash which this satisfies
            // modulo a NaN collision risk we accept for template args.
            Self::Float(f) => {
                4u8.hash(h);
                f.to_bits().hash(h);
            }
            Self::Bool(b) => {
                5u8.hash(h);
                b.hash(h);
            }
        }
    }
}

/// An S-expression — the homoiconic value + program representation.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Sexp {
    Nil,
    Atom(Atom),
    List(Vec<Sexp>),
    /// `'x` — literal; does not participate in macro substitution.
    Quote(Box<Sexp>),
    /// `` `x `` — quasi-quotation; substitution happens inside.
    Quasiquote(Box<Sexp>),
    /// `,x` — substitute the binding named `x`. Only valid inside a quasi-quote.
    Unquote(Box<Sexp>),
    /// `,@x` — splice the list `x` into the containing list.
    UnquoteSplice(Box<Sexp>),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Atom {
    /// Plain symbol (`foo`, `defpoint`, `seph.1`).
    Symbol(String),
    /// Keyword (`:parent`, `:attr`) — a symbol bound to itself.
    Keyword(String),
    /// String literal.
    Str(String),
    /// Integer literal.
    Int(i64),
    /// Floating literal.
    Float(f64),
    /// Boolean literal (`#t`, `#f`).
    Bool(bool),
}

impl Atom {
    /// Canonical Scheme spelling of [`true`] at the reader boundary.
    ///
    /// Ported from the retired fork alongside the closed-set carriers: the
    /// two literals were spelled inline at the `Display` arms below AND at
    /// the reader's classifier, and the `assert_str_array_*` witnesses over
    /// [`Self::BOOL_LITERALS`] need ONE array to prove distinctness /
    /// non-emptiness / ASCII-ness against. Two sites for one pairing is the
    /// ≥2 PRIME-DIRECTIVE trigger; this is the single site.
    pub const TRUE_LITERAL: &'static str = "#t";
    /// Canonical Scheme spelling of [`false`] at the reader boundary —
    /// column-dual peer of [`Self::TRUE_LITERAL`].
    pub const FALSE_LITERAL: &'static str = "#f";
    /// The closed two-element bool-literal vocabulary, in `[true, false]`
    /// order. The const-eval witnesses below prove it pairwise-distinct,
    /// all-non-empty and all-ASCII at `cargo check` time.
    pub const BOOL_LITERALS: [&'static str; 2] = [Self::TRUE_LITERAL, Self::FALSE_LITERAL];

    /// Project the atomic payload onto its closed-set [`AtomKind`] marker.
    ///
    /// The single site the (Atom variant, AtomKind) pairing lives at, so
    /// every consumer surface — diagnostic label, `SexpShape`, cache-key
    /// discriminator — reads one table instead of re-deriving six arms.
    #[must_use]
    pub fn kind(&self) -> AtomKind {
        match self {
            Self::Symbol(_) => AtomKind::Symbol,
            Self::Keyword(_) => AtomKind::Keyword,
            Self::Str(_) => AtomKind::Str,
            Self::Int(_) => AtomKind::Int,
            Self::Float(_) => AtomKind::Float,
            Self::Bool(_) => AtomKind::Bool,
        }
    }
}

impl Sexp {
    pub fn symbol(s: impl Into<String>) -> Self {
        Self::Atom(Atom::Symbol(s.into()))
    }
    pub fn keyword(s: impl Into<String>) -> Self {
        Self::Atom(Atom::Keyword(s.into()))
    }
    pub fn string(s: impl Into<String>) -> Self {
        Self::Atom(Atom::Str(s.into()))
    }
    pub fn int(n: i64) -> Self {
        Self::Atom(Atom::Int(n))
    }
    pub fn float(n: f64) -> Self {
        Self::Atom(Atom::Float(n))
    }
    pub fn boolean(b: bool) -> Self {
        Self::Atom(Atom::Bool(b))
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Self::List(_))
    }
    pub fn as_list(&self) -> Option<&[Sexp]> {
        match self {
            Self::List(xs) => Some(xs),
            _ => None,
        }
    }
    pub fn as_symbol(&self) -> Option<&str> {
        match self {
            Self::Atom(Atom::Symbol(s)) => Some(s),
            _ => None,
        }
    }
    pub fn as_keyword(&self) -> Option<&str> {
        match self {
            Self::Atom(Atom::Keyword(s)) => Some(s),
            _ => None,
        }
    }
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Self::Atom(Atom::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Atom(Atom::Int(n)) => Some(*n),
            _ => None,
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Atom(Atom::Float(n)) => Some(*n),
            Self::Atom(Atom::Int(n)) => Some(*n as f64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Atom(Atom::Bool(b)) => Some(*b),
            _ => None,
        }
    }
    /// `foo` or `"foo"` — useful for names that may be authored either way.
    pub fn as_symbol_or_string(&self) -> Option<&str> {
        self.as_symbol().or_else(|| self.as_string())
    }

    /// Soft projection onto the closed-set [`QuoteForm`] carving marker —
    /// the four homoiconic prefix-wrappers — paired with the wrapped inner
    /// form. `None` for every non-quote-family outer shape.
    #[must_use]
    pub fn as_quote_form(&self) -> Option<(QuoteForm, &Sexp)> {
        match self {
            Self::Quote(inner) => Some((QuoteForm::Quote, inner)),
            Self::Quasiquote(inner) => Some((QuoteForm::Quasiquote, inner)),
            Self::Unquote(inner) => Some((QuoteForm::Unquote, inner)),
            Self::UnquoteSplice(inner) => Some((QuoteForm::UnquoteSplice, inner)),
            _ => None,
        }
    }

    /// Soft projection onto the 2-of-4 [`UnquoteForm`] template-substitution
    /// subset of the quote family, paired with the wrapped inner form.
    /// `Some` iff this is `,x` or `,@x`.
    #[must_use]
    pub fn as_unquote(&self) -> Option<(UnquoteForm, &Sexp)> {
        let (qf, inner) = self.as_quote_form()?;
        qf.as_unquote_form().map(|uf| (uf, inner))
    }

    /// Project this form to its outer [`SexpShape`].
    ///
    /// Every arm routes through a carving marker's own `sexp_shape`
    /// projection — atomic via [`AtomKind::sexp_shape`], structural via
    /// [`StructuralKind::sexp_shape`], quote-family via
    /// [`QuoteForm::sexp_shape`] (6 + 2 + 4 = the twelve shapes). No arm
    /// reaches a bare `SexpShape::*` literal, so a thirteenth `Sexp`
    /// variant lands as one arm here plus one arm on its carving marker,
    /// in lockstep, instead of drifting between them.
    #[must_use]
    pub fn shape(&self) -> SexpShape {
        match self {
            Self::Nil => crate::error::StructuralKind::Nil.sexp_shape(),
            Self::Atom(a) => a.kind().sexp_shape(),
            Self::List(_) => crate::error::StructuralKind::List.sexp_shape(),
            Self::Quote(_) | Self::Quasiquote(_) | Self::Unquote(_) | Self::UnquoteSplice(_) => {
                // Unreachable-by-construction: the four arms above are
                // exactly `as_quote_form`'s `Some` domain.
                let (qf, _) = self
                    .as_quote_form()
                    .expect("quote-family arm must project to a QuoteForm");
                qf.sexp_shape()
            }
        }
    }
}

impl fmt::Display for Sexp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => f.write_str("()"),
            Self::Atom(a) => match a {
                Atom::Symbol(s) => f.write_str(s),
                Atom::Keyword(s) => write!(f, ":{s}"),
                Atom::Str(s) => write!(f, "{s:?}"),
                Atom::Int(n) => write!(f, "{n}"),
                Atom::Float(n) => write!(f, "{n}"),
                Atom::Bool(true) => f.write_str("#t"),
                Atom::Bool(false) => f.write_str("#f"),
            },
            Self::List(xs) => {
                f.write_str("(")?;
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{x}")?;
                }
                f.write_str(")")
            }
            Self::Quote(inner) => write!(f, "'{inner}"),
            Self::Quasiquote(inner) => write!(f, "`{inner}"),
            Self::Unquote(inner) => write!(f, ",{inner}"),
            Self::UnquoteSplice(inner) => write!(f, ",@{inner}"),
        }
    }
}

// ── Ported from the retired fork (pleme-io/tatara @ tatara-lisp/src/ast.rs)
//    as part of phase 2 step 2: the two ClosedSet carriers LispError's payload
//    alphabet projects into. See theory/TATARA-LISP-CONSOLIDATION.md R1 M3.

/// Closed-set typed discriminator for the six [`Atom`] payload variants —
/// `Symbol(String)`, `Keyword(String)`, `Str(String)`, `Int(i64)`,
/// `Float(f64)`, `Bool(bool)` — paired with the projections every
/// per-atom-kind consumer keys on ([`Self::hash_discriminator`] for
/// [`Hash for Atom`]'s cache-key bytes, [`Self::sexp_shape`] for
/// [`crate::domain::sexp_shape`]'s atom-arm collapse, [`Self::label`]
/// for the operator-facing diagnostic vocabulary, [`Self::FromStr`]
/// for the typed-inverse decode that lets LSP / REPL / metric-aggregator
/// consumers round-trip a rendered diagnostic label back into the typed
/// discriminator).
///
/// Atomic-payload peer of [`QuoteForm`] (the four homoiconic prefix
/// wrappers — `Sexp::{Quote, Quasiquote, Unquote, UnquoteSplice}`):
/// where `QuoteForm` carves the closed set on `Sexp`'s wrapper-variant
/// axis, `AtomKind` carves the closed set on `Sexp`'s atomic-payload
/// axis. Together the two closed-set discriminators cover every reachable
/// `Sexp` outermost shape except `Nil` and `List` (the structural
/// constructors `()` and `(…)`) — every other shape is either an
/// `Atom(_)` projecting through this enum's [`Self::sexp_shape`] arm or a
/// quote-family wrapper projecting through [`QuoteForm::sexp_shape`].
/// After this lift the two enums' [`Self::sexp_shape`] arms own ALL TEN
/// of [`SexpShape`]'s twelve canonical labels through ONE typed
/// composition each rather than through per-callsite arm-pairing in
/// [`crate::domain::sexp_shape`].
///
/// Mirror at the atomic-payload boundary of the prior-run [`QuoteForm`]
/// (homoiconic-prefix-wrapper closed set, 4 variants), the cross-crate
/// `tatara-process` closed-set family
/// (`ConditionKind::ALL`, `ProcessPhase::ALL`, `ProcessSignal::ALL`,
/// `ChannelKind::ALL`, `IntentKind::ALL`, `LifetimeKind::ALL`,
/// `RequestorKind::ALL`, `ReceiptKind::ALL`, …) and this crate's own
/// [`SexpShape`] (the twelve reachable Sexp outermost shapes — the
/// SUPERSET this enum projects into via [`Self::sexp_shape`]) and
/// [`UnquoteForm`] (the two template-substitution markers) closed-set
/// lifts: those enums key their respective rejection or projection
/// variants on a typed identity carried inside the variant's data shape;
/// this enum keys the SIX [`Atom`] payload variants on a typed
/// discriminator identity threaded through ALL THREE per-atom-kind
/// dispatch sites ([`Hash for Atom`]'s six byte literals,
/// [`crate::domain::sexp_shape`]'s six atom arms, AND the
/// diagnostic-label vocabulary [`SexpShape::label`] publishes for the
/// atom subset). Adding a hypothetical seventh atomic kind (e.g. a
/// `Char` literal for `#\x` reader syntax, a `Bigint` for arbitrary-
/// precision integers, a `Symbol2` for namespaced symbols) requires
/// extending this enum, which rustc-enforces matching at every
/// projection site ([`Self::label`], [`Self::hash_discriminator`],
/// [`Self::sexp_shape`], [`Atom::kind`], the [`Hash for Atom`] inner
/// match, and the [`Self::FromStr`] sweep keyed on [`Self::ALL`]) — the
/// closed set becomes a TYPE rather than six `&'static str` / `u8`
/// / `SexpShape` literals that could drift independently across the
/// substrate's three per-atom-kind consumer surfaces.
///
/// Theory anchor: THEORY.md §II.1 invariant 1 — typed entry; the
/// atomic-payload discriminator at a typed-entry rejection IS part of
/// the proof of WHAT the gate observed, and naming its closed-set
/// identity lifts the discriminator from per-site literal-pair
/// discipline (a byte at the Hash site, a SexpShape variant at the
/// `sexp_shape` site, a `&'static str` at any future LSP completion
/// site) to ONE typed enum the substrate's diagnostic + cache-key
/// surfaces both bind against. THEORY.md §II.1 invariant 2 — free
/// middle; THREE consumers ([`Hash for Atom`],
/// [`crate::domain::sexp_shape`], and the future diagnostic /
/// completion surface) route through ONE typed closed-set match
/// family, so a regression that drifts ONE consumer's pairing from the
/// others cannot reach the substrate's runtime. THEORY.md §V.1 —
/// knowable platform; the closed set of atomic payload kinds becomes a
/// TYPE rather than six byte literals (Hash) + six SexpShape literals
/// (`sexp_shape`) scattered across distinct files — a typo in any one
/// site is no longer a runtime drift but a compile error against the
/// typed projection. THEORY.md §VI.1 — generation over composition;
/// the (Atom variant, label, discriminator-byte, SexpShape variant)
/// quadruple appeared inline at THREE sites (`Hash for Atom`'s six
/// byte arms, `domain::sexp_shape`'s six atom arms, plus implicit
/// pairing across `SexpShape::label`'s six atom-subset arms) — well
/// past the ≥2 PRIME-DIRECTIVE trigger once the structural shape is
/// named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, tatara_closed_set::DeriveClosedSet)]
#[closed_set(via = "label", display, generate_unknown = "atom kind")]
pub enum AtomKind {
    /// `Atom::Symbol(_)` — `"symbol"` diagnostic label, byte `0u8`
    /// hash discriminator, projects to [`SexpShape::Symbol`].
    Symbol,
    /// `Atom::Keyword(_)` — `"keyword"` diagnostic label, byte `1u8`
    /// hash discriminator, projects to [`SexpShape::Keyword`].
    Keyword,
    /// `Atom::Str(_)` — `"string"` diagnostic label, byte `2u8` hash
    /// discriminator, projects to [`SexpShape::String`].
    Str,
    /// `Atom::Int(_)` — `"int"` diagnostic label, byte `3u8` hash
    /// discriminator, projects to [`SexpShape::Int`].
    Int,
    /// `Atom::Float(_)` — `"float"` diagnostic label, byte `4u8` hash
    /// discriminator, projects to [`SexpShape::Float`].
    Float,
    /// `Atom::Bool(_)` — `"bool"` diagnostic label, byte `5u8` hash
    /// discriminator, projects to [`SexpShape::Bool`].
    Bool,
}

impl AtomKind {
    /// The closed set of six atomic [`Atom`] payload kinds — single
    /// source of truth that drives every per-kind projection
    /// ([`Self::label`] / [`fmt::Display`], [`Self::hash_discriminator`],
    /// [`Self::sexp_shape`], and the [`Self::FromStr`] decode sweep
    /// keyed on [`Self::label`]).
    ///
    /// Adding a hypothetical seventh atomic kind (e.g. `Char` for
    /// `#\x` reader syntax, `Bigint` for arbitrary-precision
    /// integers) lands at one [`Self::ALL`] entry plus one arm per
    /// projection — exhaustively checked by the compiler (the
    /// `[Self; 6]` array literal forces the arity) AND by the
    /// per-variant truth-table tests below.
    ///
    /// Sibling closed-set lift to every other typed-shape enum the
    /// substrate carries: this crate's own [`SexpShape::ALL`] (the
    /// twelve reachable outer shapes — superset of this kind's six),
    /// [`QuoteForm`] (the four homoiconic prefix wrappers — peer
    /// projection on the SAME `Sexp` algebra), [`UnquoteForm`] (the
    /// two template-substitution markers — proper subset of
    /// `QuoteForm`), and the cross-crate `tatara-process` family
    /// (`ConditionKind::ALL`, `ProcessPhase::ALL`,
    /// `ProcessSignal::ALL`, `ChannelKind::ALL`, `IntentKind::ALL`,
    /// …) every one of which paired its typed projection with `ALL`
    /// before this lift.
    ///
    /// Future consumers that compose against `ALL`: LSP / REPL
    /// completion for the operator-facing rendered atom-kind label
    /// (every `expected X, got Y` substring in `LispError`'s rendered
    /// diagnostics for an atomic witness keys on this set's projection
    /// through [`Self::label`]); `tatara-check` coverage assertions
    /// over which atomic kinds reach a `TypeMismatch.got` arm at all
    /// — the typed sweep replaces a per-callsite vocabulary of six
    /// `&'static str` literals; any future audit-trail metric jointly
    /// labeled by [`Self::label`] (e.g.
    /// `tatara_lisp_atom_type_mismatch_total{got="symbol"}`) — the
    /// metric label set IS [`Self::ALL`] mapped through
    /// [`Self::label`]; any future structural rewriter (typed
    /// analogue of MLIR's `op.walk<AtomKind::Symbol>()`) that wants
    /// to sweep over every atomic kind in a typed sequence.
    pub const ALL: [Self; 6] = [
        Self::Symbol,
        Self::Keyword,
        Self::Str,
        Self::Int,
        Self::Float,
        Self::Bool,
    ];

    /// Canonical `&'static str` bytes for the [`Self::Symbol`] atomic-
    /// payload marker — aliases [`SexpShape::SYMBOL_LABEL`] on the
    /// AtomKind ⊂ SexpShape carving so the marker-level per-role bytes
    /// bind at ONE `pub const` on the parent superset's atomic arm
    /// rather than at TWO sites (the per-role `pub const` AND a
    /// parallel inline literal). Per-role peer of `Self::Symbol` on the
    /// closed-set atomic algebra; consumers reach for
    /// `AtomKind::SYMBOL_LABEL` when the caller has a variant in hand
    /// at compile time and wants the canonical diagnostic bytes without
    /// runtime dispatch through [`Self::label`].
    pub const SYMBOL_LABEL: &'static str = SexpShape::SYMBOL_LABEL;

    /// Canonical `&'static str` bytes for the [`Self::Keyword`] atomic-
    /// payload marker — aliases [`SexpShape::KEYWORD_LABEL`] on the
    /// AtomKind ⊂ SexpShape carving. Per-role peer of `Self::Keyword`.
    pub const KEYWORD_LABEL: &'static str = SexpShape::KEYWORD_LABEL;

    /// Canonical `&'static str` bytes for the [`Self::Str`] atomic-
    /// payload marker — aliases [`SexpShape::STRING_LABEL`] on the
    /// AtomKind ⊂ SexpShape carving. Per-role peer of `Self::Str`; the
    /// `Str → "string"` wire-shape rename matches
    /// [`SexpShape::String`]'s label projection so the AtomKind marker
    /// and its SexpShape peer emit byte-identical diagnostic bytes.
    pub const STRING_LABEL: &'static str = SexpShape::STRING_LABEL;

    /// Canonical `&'static str` bytes for the [`Self::Int`] atomic-
    /// payload marker — aliases [`SexpShape::INT_LABEL`] on the
    /// AtomKind ⊂ SexpShape carving. Per-role peer of `Self::Int`.
    pub const INT_LABEL: &'static str = SexpShape::INT_LABEL;

    /// Canonical `&'static str` bytes for the [`Self::Float`] atomic-
    /// payload marker — aliases [`SexpShape::FLOAT_LABEL`] on the
    /// AtomKind ⊂ SexpShape carving. Per-role peer of `Self::Float`.
    pub const FLOAT_LABEL: &'static str = SexpShape::FLOAT_LABEL;

    /// Canonical `&'static str` bytes for the [`Self::Bool`] atomic-
    /// payload marker — aliases [`SexpShape::BOOL_LABEL`] on the
    /// AtomKind ⊂ SexpShape carving. Per-role peer of `Self::Bool`.
    pub const BOOL_LABEL: &'static str = SexpShape::BOOL_LABEL;

    /// Closed-set forced-arity ALL array over the canonical atomic-
    /// payload marker `&'static str` bytes, in declaration order
    /// matching [`Self::ALL`] element-wise (pinned by
    /// `atom_kind_labels_align_with_all_by_index`). Sibling posture to
    /// [`SexpShape::LABELS`] (`[&'static str; 12]` — the superset
    /// carving this AtomKind subset embeds into),
    /// [`crate::error::ExpectedKwargShape::LABELS`] (`[&'static str; 7]`),
    /// [`crate::error::KwargPathKind::LABELS`] (`[&'static str; 3]`),
    /// [`crate::error::MacroDefHead::KEYWORDS`] (`[&'static str; 3]`),
    /// [`Atom::BOOL_LITERALS`] (`[&'static str; 2]`), and
    /// [`QuoteForm::PREFIXES`] (`[&'static str; 4]`) — every closed-set
    /// outer projection on the substrate that carries an `&'static str`-
    /// per-variant label now pins its per-role canonical bytes at ONE
    /// `pub const` per role PLUS an ALL array for family-wide consumers.
    ///
    /// Pre-lift the six atomic-payload marker bytes had NO per-role
    /// primitive on this closed-set algebra — a consumer with an
    /// `AtomKind` variant in hand at compile time reaching for the
    /// canonical diagnostic bytes had to spell
    /// `AtomKind::Symbol.label()` (runtime dispatch through the
    /// composition [`Self::sexp_shape`] + [`SexpShape::label`]) OR
    /// reach across the algebra boundary into
    /// [`SexpShape::SYMBOL_LABEL`] and re-derive the AtomKind ⊂
    /// SexpShape variant pairing at the call site. Post-lift the SIX
    /// canonical bytes bind at ONE `pub const` per role on the typed
    /// [`AtomKind`] algebra AND at [`Self::LABELS`] as a family-wide
    /// forced-arity array — a future LSP / REPL completion bar keyed on
    /// `AtomKind::LABELS`, a `tatara-check` coverage sweep over the
    /// atomic-payload arms of a `TypeMismatch.got` corpus, or a Sekiban
    /// audit-trail metric jointly labeled by the atomic marker
    /// (`tatara_lisp_atom_type_mismatch_total{kind="symbol"}`) reads
    /// through the typed constants on this subset algebra without
    /// re-deriving the 6-of-12 carving inline.
    ///
    /// Each entry is byte-for-byte identical to the corresponding
    /// [`SexpShape`] atomic arm — an intentional cross-axis overlap
    /// pinned by
    /// `atom_kind_per_role_labels_alias_sexp_shape_per_role_labels_byte_for_byte`
    /// so a future label rename on EITHER side (a SexpShape `"string"`
    /// → `"str"` drift, or an AtomKind rename that skips the alias)
    /// fails-loudly at the alias test rather than as a silent
    /// operator-facing vocabulary fracture. Adding a hypothetical
    /// seventh atomic kind (e.g. `Char` for `#\x` reader syntax,
    /// `Bigint` for arbitrary-precision integers) extends [`Self::ALL`]
    /// AND [`Self::LABELS`] AND adds ONE per-role `pub const` alias in
    /// lockstep — rustc's forced-arity check on the two `[_; N]` arrays
    /// fails compilation if EITHER ALL array grows without the other.
    ///
    /// Theory anchor: THEORY.md §III — the typescape; the six canonical
    /// atomic-payload marker bytes bind at ONE typed
    /// `[&'static str; 6]` array on the closed-set AtomKind algebra
    /// rather than at zero-primitive-on-this-subset-plus-six-inline-
    /// lookups scattered across the substrate. THEORY.md §V.1 —
    /// knowable platform; the family's cardinality becomes a TYPE-level
    /// constant on the substrate algebra rather than a per-consumer
    /// runtime dispatch through the composition. THEORY.md §VI.1 —
    /// generation over composition; the family-wide contract sweeps
    /// (alignment with `ALL`, pairwise disjointness, membership through
    /// [`Self::label`]) emerge from the composition of TWO substrate
    /// primitives (this `pub const` array + the six per-role
    /// `pub const *_LABEL` aliases) rather than as per-variant inline
    /// assertions duplicated at each call site.
    pub const LABELS: [&'static str; 6] = [
        Self::SYMBOL_LABEL,
        Self::KEYWORD_LABEL,
        Self::STRING_LABEL,
        Self::INT_LABEL,
        Self::FLOAT_LABEL,
        Self::BOOL_LABEL,
    ];

    /// Canonical `u8` cache-key byte for [`Self::Symbol`]'s
    /// [`Self::hash_discriminator`] arm — `0`. Per-role peer of
    /// [`Self::Symbol`] on the closed-set atomic-payload cache-key-byte
    /// axis; consumers reach for `AtomKind::SYMBOL_HASH_DISCRIMINATOR`
    /// when the caller has a variant in hand at compile time and wants
    /// the canonical byte without runtime dispatch through
    /// [`Self::hash_discriminator`]. The byte is load-bearing because
    /// the macro-expansion cache ([`crate::macro_expand::Expander`]'s
    /// cache) keys on [`Hash for Atom`], and any renumbering silently
    /// invalidates every cached expansion — post-lift the six canonical
    /// bytes bind at ONE `pub(crate) const` per role rather than at
    /// six inline `u8` literals scattered across
    /// [`Self::hash_discriminator`]'s match arms.
    ///
    /// Sibling posture to [`crate::error::QuoteForm::QUOTE_HASH_DISCRIMINATOR`]
    /// on the quote-family sub-carving — both close their respective
    /// closed-set cache-key algebras at ONE per-role constant per
    /// variant PLUS a family-wide [`Self::HASH_DISCRIMINATORS`] array.
    /// The two families partition their respective cache-key spaces
    /// independently: `AtomKind` at `{0..=5}` NESTED inside
    /// [`crate::ast::Sexp::Atom`]'s outer `1u8` byte (`Hash for Atom`
    /// runs on the [`Atom`] type, not [`Sexp`]), `QuoteForm` at
    /// `{3..=6}` at the outer [`Sexp`] cache-key space itself.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 5 — composition
    /// preserves proofs; the alias-chain composition law
    /// `AtomKind::HASH_DISCRIMINATORS[i] ==
    /// AtomKind::ALL[i].hash_discriminator()` binds the family-wide
    /// array to the projection method at rustc time, pinned by byte
    /// equality. THEORY.md §III — the typescape; the six canonical
    /// cache-key bytes bind at ONE `pub(crate) const` per role on the
    /// typed algebra rather than as inline `u8` literals in the
    /// `hash_discriminator` match arms.
    pub(crate) const SYMBOL_HASH_DISCRIMINATOR: u8 = 0;

    /// Canonical `u8` cache-key byte for [`Self::Keyword`]'s
    /// [`Self::hash_discriminator`] arm — `1`. Sibling of
    /// [`Self::SYMBOL_HASH_DISCRIMINATOR`] on the closed-set per-role
    /// atomic-payload cache-key-byte axis; see
    /// [`Self::SYMBOL_HASH_DISCRIMINATOR`] for the algebra-level
    /// round-trip + disjointness contracts every sibling shares.
    pub(crate) const KEYWORD_HASH_DISCRIMINATOR: u8 = 1;

    /// Canonical `u8` cache-key byte for [`Self::Str`]'s
    /// [`Self::hash_discriminator`] arm — `2`. Sibling of
    /// [`Self::SYMBOL_HASH_DISCRIMINATOR`] on the closed-set per-role
    /// atomic-payload cache-key-byte axis.
    pub(crate) const STR_HASH_DISCRIMINATOR: u8 = 2;

    /// Canonical `u8` cache-key byte for [`Self::Int`]'s
    /// [`Self::hash_discriminator`] arm — `3`. Sibling of
    /// [`Self::SYMBOL_HASH_DISCRIMINATOR`] on the closed-set per-role
    /// atomic-payload cache-key-byte axis.
    pub(crate) const INT_HASH_DISCRIMINATOR: u8 = 3;

    /// Canonical `u8` cache-key byte for [`Self::Float`]'s
    /// [`Self::hash_discriminator`] arm — `4`. Sibling of
    /// [`Self::SYMBOL_HASH_DISCRIMINATOR`] on the closed-set per-role
    /// atomic-payload cache-key-byte axis.
    pub(crate) const FLOAT_HASH_DISCRIMINATOR: u8 = 4;

    /// Canonical `u8` cache-key byte for [`Self::Bool`]'s
    /// [`Self::hash_discriminator`] arm — `5`. Sibling of
    /// [`Self::SYMBOL_HASH_DISCRIMINATOR`] on the closed-set per-role
    /// atomic-payload cache-key-byte axis. The HIGHEST byte on the
    /// closed set — a future seventh atomic kind (e.g. `Char` for
    /// `#\x` reader syntax, `Bigint` for arbitrary-precision integers)
    /// would extend the partition to `{0..=6}` and land the new
    /// discriminator at `6u8`.
    pub(crate) const BOOL_HASH_DISCRIMINATOR: u8 = 5;

    /// Closed-set forced-arity ALL array over the canonical atomic-
    /// payload cache-key `u8` bytes, in declaration order matching
    /// [`Self::ALL`] element-wise (pinned by
    /// `atom_kind_hash_discriminators_align_with_all_by_index`).
    /// Sibling posture to [`Self::LABELS`] (`[&'static str; 6]` — the
    /// diagnostic-label `&'static str` axis on the SAME closed set) and
    /// to [`crate::error::QuoteForm::HASH_DISCRIMINATORS`] (`[u8; 4]` —
    /// the quote-family sub-carving's cache-key-byte peer). Every
    /// closed-set outer projection on the substrate's [`AtomKind`]
    /// algebra that carries a `u8` per-variant discriminator now pins
    /// its per-role canonical bytes at ONE `pub(crate) const` per role
    /// PLUS an ALL array for family-wide consumers.
    ///
    /// Pre-lift the six cache-key bytes had NO per-role primitive on
    /// this closed-set algebra — a consumer with an [`AtomKind`]
    /// variant in hand at compile time reaching for the canonical byte
    /// had to spell `AtomKind::Str.hash_discriminator()` (runtime
    /// dispatch through the match arm) OR reach across into the inline
    /// `2u8` at the pre-lift match arm's [`Self::Str`] branch and
    /// re-derive the (variant, byte) pairing at the call site.
    /// Post-lift the SIX canonical bytes bind at ONE `pub(crate) const`
    /// per role on the typed [`AtomKind`] algebra AND at
    /// [`Self::HASH_DISCRIMINATORS`] as a family-wide forced-arity
    /// array — a future substrate-facing cache-key introspection tool
    /// (a `tatara-check` predicate that asserts every atomic arm's
    /// discriminator injective on the nested [`Atom`] axis, a Sekiban
    /// audit-trail metric jointly labeled by the atomic cache-key
    /// partition, a future `TypedRewriter<AtomKindOp>` sweep zipping
    /// ALL / LABELS / HASH_DISCRIMINATORS in lockstep for a family-wide
    /// (variant, label, byte) triple render) reads through the typed
    /// constants without re-deriving the six-arm carving inline.
    ///
    /// Each entry is byte-for-byte identical to the pre-lift inline
    /// `u8` literal at the corresponding [`Self::hash_discriminator`]
    /// arm — pinned by
    /// `atom_kind_hash_discriminators_pin_legacy_cache_key_bytes` so
    /// a regression that drifts ONE `pub(crate) const` from its pre-
    /// lift byte silently invalidates every cached expansion of an
    /// [`Atom`] participating in [`crate::macro_expand::Expander::cache`],
    /// fails-loudly at the alias test rather than at a silent cache
    /// mis-hash. Adding a hypothetical seventh atomic kind (e.g.
    /// `Char` for `#\x` reader syntax, `Bigint` for arbitrary-
    /// precision integers) extends [`Self::ALL`] AND
    /// [`Self::HASH_DISCRIMINATORS`] AND adds ONE per-role
    /// `pub(crate) const` in lockstep — rustc's forced-arity check on
    /// the two `[_; N]` arrays fails compilation if EITHER array grows
    /// without the other, closing the extensibility gap that pre-lift
    /// silently allowed a discriminator collision on `6u8` (the next
    /// free byte on the nested [`Atom`] cache-key space).
    ///
    /// Theory anchor: THEORY.md §III — the typescape; the six
    /// canonical cache-key bytes bind at ONE typed `[u8; 6]` array on
    /// the closed-set [`AtomKind`] algebra rather than at zero-
    /// primitive-plus-six-inline-`u8`-literals scattered across the
    /// [`Self::hash_discriminator`] match arms. THEORY.md §V.1 —
    /// knowable platform; the family's cardinality becomes a TYPE-
    /// level constant on the substrate algebra rather than a per-
    /// consumer runtime dispatch through the match table. THEORY.md
    /// §V.3 — three-pillar attestation; the cache-key partition is
    /// the substrate's nested [`Atom`] `intent_hash` composition axis
    /// for every atomic arm — binding the six bytes on the typed
    /// algebra makes attestation-key drift a compile error rather
    /// than a silent BLAKE3 mis-hash. THEORY.md §VI.1 — generation
    /// over composition; the family-wide contract sweeps (alignment
    /// with `ALL`, pairwise disjointness, membership through
    /// [`Self::hash_discriminator`]) emerge from the composition of
    /// TWO substrate primitives (this `pub(crate) const` array + the
    /// six per-role `pub(crate) const *_HASH_DISCRIMINATOR` aliases)
    /// rather than as per-variant inline assertions duplicated at each
    /// call site.
    ///
    /// The `#[allow(dead_code)]` posture matches
    /// [`crate::error::QuoteForm::HASH_DISCRIMINATORS`]: the substrate's
    /// current [`Hash for Atom`] body composes through the per-variant
    /// [`Self::hash_discriminator`] projection arm-by-arm rather than
    /// sweeping the family-wide array, so no non-test caller currently
    /// reaches this ALL array directly. The lift lands the substrate
    /// primitive so future consumers keyed on the whole family (a
    /// future [`crate::macro_expand::Expander`] cache-warmup pass that
    /// hashes the atomic byte-set upfront, a future `tatara-check`
    /// predicate `(check-atom-cache-key-partition-injective …)` that
    /// verifies the `{0..=5}` partition structurally, a future
    /// `TypedRewriter<AtomKindOp>` sweep zipping ALL / LABELS /
    /// HASH_DISCRIMINATORS in lockstep for a family-wide (variant,
    /// label, byte) triple render) bind to ONE `[u8; 6]` primitive
    /// rather than re-deriving the array inline per callsite.
    #[allow(dead_code)]
    pub(crate) const HASH_DISCRIMINATORS: [u8; 6] = [
        Self::SYMBOL_HASH_DISCRIMINATOR,
        Self::KEYWORD_HASH_DISCRIMINATOR,
        Self::STR_HASH_DISCRIMINATOR,
        Self::INT_HASH_DISCRIMINATOR,
        Self::FLOAT_HASH_DISCRIMINATOR,
        Self::BOOL_HASH_DISCRIMINATOR,
    ];

    /// Canonical `u8` OUTER-`Sexp` cache-key byte at which ALL SIX
    /// atomic-payload shapes collapse when hashed at the outer
    /// [`Hash for Sexp`](crate::ast::Sexp) level — `1`. The
    /// outer-carve peer of [`Self::HASH_DISCRIMINATORS`] (the six
    /// nested INNER cache-key bytes `{0..=5}` that specialise INSIDE
    /// [`Hash for Atom`] after the outer marker byte is emitted).
    /// The lift moves the byte the outer-Sexp cache-key algebra uses
    /// to distinguish [`crate::ast::Sexp::Atom(_)`] from every other
    /// outer-Sexp variant off inline `1u8` literals scattered across
    /// [`crate::error::SexpShape::hash_discriminator`]'s six-arm
    /// atomic collapse + the two structural-carve joint-partition
    /// disjointness pins and onto ONE `pub(crate) const` on the
    /// [`AtomKind`] algebra it names.
    ///
    /// Where the byte appears at the outer-Sexp cache-key algebra:
    /// [`crate::error::SexpShape::hash_discriminator`]'s atomic-arm
    /// collapse `Self::Symbol | Self::Keyword | Self::String |
    /// Self::Int | Self::Float | Self::Bool => 1` binds directly to
    /// this constant; every one of the six atomic shapes routes
    /// through the shape-level projection into the outer-Sexp cache
    /// key at THIS byte. The nested inner
    /// [`Self::HASH_DISCRIMINATORS`] `{0..=5}` bytes then specialise
    /// the atomic payload INSIDE [`Hash for Atom`] via a second
    /// discriminator emission (`self.hash_discriminator().hash(h)`
    /// on the [`Atom`] value carrier), so the two byte spaces live
    /// at different hash-sequence positions and do not collide.
    ///
    /// Sibling posture to [`crate::error::StructuralKind::HASH_DISCRIMINATORS`]
    /// (`[u8; 2]` at `{0, 2}` on the outer-Sexp cache-key space) and
    /// to [`crate::error::QuoteForm::HASH_DISCRIMINATORS`] (`[u8; 4]`
    /// at `{3, 4, 5, 6}` on the same space) — together with THIS
    /// scalar the three sibling carvings' byte spaces jointly
    /// partition the outer-Sexp discriminator space `{0..=6}`
    /// injectively. Post-lift the outer-Sexp cache-key algebra
    /// closes over FOUR typed byte primitives:
    ///   * [`Self::OUTER_HASH_DISCRIMINATOR`] (this constant) —
    ///     scalar `1u8` for the atomic-payload outer-carve;
    ///   * [`crate::error::StructuralKind::HASH_DISCRIMINATORS`] —
    ///     `{0, 2}` for the structural-residual carve;
    ///   * [`crate::error::QuoteForm::HASH_DISCRIMINATORS`] —
    ///     `{3..=6}` for the quote-family carve;
    ///   * [`Self::HASH_DISCRIMINATORS`] — the nested inner
    ///     `{0..=5}` byte-set INSIDE [`Hash for Atom`], NOT on the
    ///     outer-Sexp space.
    ///
    /// The scalar shape (single `u8`, NOT an array) is intrinsic to
    /// the carve: all six atomic-payload arms of
    /// [`crate::error::SexpShape`] collapse to the SAME outer byte
    /// (the outer-Sexp distinguisher is variant-level: `Sexp::Atom(_)`
    /// vs the six sibling `Sexp` variants); per-atom-kind
    /// specialisation lives at the nested inner
    /// [`Self::HASH_DISCRIMINATORS`] carve inside [`Hash for Atom`].
    /// The other two carvings' `HASH_DISCRIMINATORS` are arrays
    /// because their shape-level arms each carry a DISTINCT outer
    /// byte; the atomic carve is a scalar because its arms carry the
    /// SAME outer byte.
    ///
    /// `pub(crate)` because the byte is an implementation detail of
    /// the substrate's `Hash for Sexp` cache-key contract; exposing
    /// it publicly would leak the cache-key shape through the API
    /// without enabling any external consumer the public projections
    /// ([`Self::label`], [`Self::sexp_shape`]) don't already serve.
    /// Same posture as [`Self::HASH_DISCRIMINATORS`] +
    /// [`Self::SYMBOL_HASH_DISCRIMINATOR`] and the sibling carvings'
    /// per-role `pub(crate) const` peers.
    ///
    /// Pre-lift the outer-Atom marker byte lived at THREE sites: the
    /// inline `1u8` literal at
    /// [`crate::error::SexpShape::hash_discriminator`]'s six-arm
    /// atomic collapse; the inline `1u8` literal at
    /// `sexp_shape_hash_discriminator_atomic_arms_collapse_to_outer_atom_marker`'s
    /// assertion body; the inline `1u8` literal at
    /// `sexp_shape_hash_discriminator_partitions_by_three_way_carving_disjointly`'s
    /// `expected_atomic` fixture; PLUS a duplicated local `const
    /// ATOM_OUTER_CARVE_BYTE: u8 = 1` inside
    /// `structural_kind_hash_discriminator_disjoint_from_atom_outer_carve_byte_and_quote_form_hash_discriminator_partition`.
    /// The (byte, algebra) pairing had no typed home — a consumer
    /// with a typed [`AtomKind`] identity in hand reaching for the
    /// outer-Sexp cache-key byte the atomic arm collapses to had to
    /// re-derive the byte from the shape-level projection method's
    /// atomic collapse arm inline, OR re-derive the pre-lift local
    /// `ATOM_OUTER_CARVE_BYTE` fixture at every joint-partition-check
    /// site. Post-lift the byte binds at ONE `pub(crate) const` on
    /// the closed-set [`AtomKind`] algebra it names; every downstream
    /// consumer (the shape-level projection, the joint-partition
    /// disjointness pins, the three-way carving image pin, a future
    /// `tatara-check` predicate that verifies the outer-Sexp cache-
    /// key partition structurally, a future
    /// [`crate::macro_expand::Expander`] cache-warmup pass that
    /// hashes the outer-Sexp byte-set upfront) picks up the same
    /// canonical byte from ONE source of truth.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 5 — composition
    /// preserves proofs; the (AtomKind ⊂ SexpShape carve, outer-Sexp
    /// cache-key byte) pairing binds at rustc time by byte equality
    /// against the shape-level projection's atomic collapse arm.
    /// THEORY.md §III — the typescape; the outer-Atom cache-key byte
    /// binds at ONE `pub(crate) const` on the typed algebra rather
    /// than as inline `1u8` literals at every joint-partition + shape-
    /// level-collapse site. THEORY.md §V.1 — knowable platform; the
    /// outer-Sexp cache-key space's four-way partition (this scalar
    /// PLUS the three sibling carvings' arrays) becomes a TYPE-level
    /// constant on the substrate algebra rather than a per-callsite
    /// hand-rolled `{0, 1, 2, 3, 4, 5, 6}` re-enumeration. THEORY.md
    /// §V.3 — three-pillar attestation; the outer-Sexp cache-key
    /// partition is the substrate's outer [`Sexp`] `intent_hash`
    /// composition axis — binding the four-way partition's atomic-
    /// carve byte on the typed algebra makes attestation-key drift a
    /// compile error rather than a silent BLAKE3 mis-hash. A future
    /// eighth [`Sexp`] variant (e.g. `Vector` for `#(...)` reader
    /// syntax, `Map` for `{...}`, `Char` for `#\x`) picks a fresh
    /// cache-key byte outside `{0..=6}` (e.g. `7u8`), extends the
    /// closed-set [`crate::error::SexpShape`] enum + its shape-level
    /// `hash_discriminator` and either an existing carving OR a fresh
    /// sub-algebra — the outer-Atom scalar itself stays untouched
    /// unless the new variant is also an atomic-payload arm.
    pub(crate) const OUTER_HASH_DISCRIMINATOR: u8 = 1;

    /// Project the typed marker to the canonical `&'static str`
    /// diagnostic label — `"symbol"` for [`Self::Symbol`],
    /// `"keyword"` for [`Self::Keyword`], `"string"` for [`Self::Str`]
    /// (the wire-shape rename `Str → "string"` matches the
    /// [`SexpShape::String`] label projection), `"int"` for
    /// [`Self::Int`], `"float"` for [`Self::Float`], `"bool"` for
    /// [`Self::Bool`]. Each label is byte-for-byte identical to the
    /// corresponding [`SexpShape`] variant's label — and post-lift this
    /// agreement is STRUCTURAL rather than two literal-discipline sites
    /// pinned by a cross-projection test.
    ///
    /// Composition law: `AtomKind::label(k) ==
    /// AtomKind::sexp_shape(k).label()` for every `k: AtomKind`. The
    /// body composes [`Self::sexp_shape`] (the typed projection lifting
    /// each AtomKind variant into its peer [`SexpShape`] variant) with
    /// [`SexpShape::label`] (the canonical `&'static str` projection on
    /// the supeset's twelve-variant closed set), so the six atomic-arm
    /// labels live at ONE canonical site ([`SexpShape::label`]) rather
    /// than at TWO ([`SexpShape::label`] AND a parallel six-arm match
    /// here, pre-lift). Pre-lift the substrate-wide AtomKind ⊂ SexpShape
    /// label-vocabulary agreement was enforced by literal discipline at
    /// the two sites + a cross-projection test
    /// (`atom_kind_label_agrees_with_sexp_shape_label_for_every_atom_arm`);
    /// post-lift the agreement is a TYPED CONSEQUENCE of the composition
    /// — a typo in `SexpShape::label`'s atomic arms is a typo in BOTH
    /// projections, and the cross-projection test is true by
    /// construction. Same lift posture as the prior-run
    /// `Atom::as_X → Atom::as_X` algebra-lift commit (6935416), the
    /// `from_lexeme` reader-atom lift commit (9b95e64), and the
    /// `to_iac_forge_sexpr` Atom-arm lift commit (418be51): the typed
    /// projection sits on the value, and the consumer composes through
    /// the existing structural pairing rather than re-deriving the
    /// per-variant literal.
    ///
    /// The `&'static str` lifetime is load-bearing: it lets the
    /// variant project through this method without an allocation,
    /// parallel to how [`SexpShape::label`], [`QuoteForm::prefix`],
    /// [`QuoteForm::iac_forge_tag`], [`UnquoteForm::marker`], and
    /// [`crate::error::ExpectedKwargShape::label`] project their
    /// respective closed-set surfaces. The composition preserves the
    /// no-allocation contract: [`Self::sexp_shape`] returns a `Copy`
    /// value and [`SexpShape::label`] yields `&'static str`, so the
    /// `&'static str` projection through the composition allocates
    /// nothing at runtime.
    ///
    /// The bidirectional contract is anchored by tests:
    /// `atom_kind_label_renders_canonical_string_for_every_variant`
    /// pins each variant's canonical literal so a typo in
    /// [`SexpShape::label`]'s atomic arms fails-loudly through this
    /// projection too, `atom_kind_display_matches_label_for_every_variant`
    /// pins Display-equals-label so any future
    /// `#[error("... got {got}")]` annotation that threads through
    /// this projection projects byte-for-byte, and
    /// `atom_kind_label_round_trips_through_from_str` pins the
    /// `label` ↔ [`Self::FromStr`] round-trip for every variant in
    /// [`Self::ALL`] so the typed surface and the rendered diagnostic
    /// literal cannot drift. The post-lift composition contract is
    /// pinned by
    /// `atom_kind_label_routes_through_sexp_shape_label_via_sexp_shape_projection`
    /// — a regression that re-inlines the six atomic-arm literals here
    /// and silently drifts ONE arm from the [`SexpShape::label`] axis
    /// fails the routing pin loudly without needing a per-variant
    /// cross-axis literal sweep.
    ///
    /// Theory anchor: THEORY.md §V.1 — knowable platform; the
    /// AtomKind ⊂ SexpShape label-vocabulary containment becomes a
    /// TYPED CONSEQUENCE of the [`Self::sexp_shape`] + [`SexpShape::label`]
    /// composition rather than literal discipline at two sites. THEORY.md
    /// §VI.1 — generation over composition; the six atomic-arm labels
    /// live at ONE canonical site ([`SexpShape::label`]) and this method
    /// generates its identity through the typed-projection composition.
    /// THEORY.md §II.1 invariant 2 — free middle; FOUR consumers of the
    /// [`AtomKind`] algebra ([`Hash for Atom`] via
    /// [`Self::hash_discriminator`], [`crate::domain::sexp_shape`] via
    /// [`Self::sexp_shape`], the diagnostic-rendering surface via this
    /// method, and the `ClosedSet`-trait FromStr/Display surface via
    /// `#[closed_set(via = "label")]`) now route through ONE typed
    /// closed-set projection family with no per-consumer literal
    /// duplication.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.sexp_shape().label()
    }

    /// Stable, per-variant byte discriminator that paired with the
    /// recursive payload hash builds the substrate's [`Hash for Atom`]
    /// projection — `0u8` for [`Self::Symbol`], `1u8` for
    /// [`Self::Keyword`], `2u8` for [`Self::Str`], `3u8` for
    /// [`Self::Int`], `4u8` for [`Self::Float`], `5u8` for
    /// [`Self::Bool`]. The byte values are load-bearing because the
    /// macro-expansion cache ([`crate::macro_expand::Expander`]'s
    /// cache) keys on the hash of `(macro_name, args)`, and any
    /// `Atom` participates in that hash — changing a discriminator
    /// silently invalidates every cached expansion across the
    /// substrate.
    ///
    /// The closed set ensures the six arms partition `{0, 1, 2, 3,
    /// 4, 5}` injectively. Disjointness from [`QuoteForm`]'s
    /// `{3, 4, 5, 6}` is structural rather than overlap-induced
    /// hash collision: [`Hash for Atom`] and the quote-family arms of
    /// [`Hash for Sexp`] hash DISTINCT types (`Atom` vs `Sexp`), and
    /// `Atom`'s discriminator lives nested INSIDE `Sexp::Atom`'s outer
    /// `1u8` discriminator — the prefix-uniqueness contract that the
    /// `Hash for Sexp` outer match maintains independently. A future
    /// quote-family or atomic-kind extension must extend BOTH bodies'
    /// arms in lockstep, with rustc binding the consistency through
    /// exhaustiveness over BOTH closed enums.
    ///
    /// `pub(crate)` because the byte-discriminator surface is an
    /// implementation detail of the substrate's [`Hash for Atom`]
    /// cache-key contract; exposing it publicly would leak the
    /// cache-key shape through the API without enabling any external
    /// consumer the public projections ([`Atom::kind`], [`Self::label`],
    /// [`Self::sexp_shape`]) don't already serve. Same posture as
    /// [`QuoteForm::hash_discriminator`] and its outer-value peer
    /// [`Atom::hash_discriminator`] (the outer-`Atom` projection that
    /// composes through this method via `self.kind().hash_discriminator()`
    /// so the [`Hash for Atom`] callsite binds at ONE site on the
    /// outer-`Atom` algebra rather than at the two-hop
    /// `.kind().hash_discriminator()` chain).
    // `allow(dead_code)`: in the fork this is the single site the `Hash for
    // Atom` body routes its discriminator byte through. B's `Hash` impls
    // still spell the six bytes inline (they are byte-identical, 0..=5), so
    // the method has no caller until phase 2 step 5 unifies the two readers
    // and their `Hash` bodies. Deleting it instead would re-open the
    // two-sites-for-one-pairing defect the projection exists to close.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn hash_discriminator(self) -> u8 {
        match self {
            Self::Symbol => Self::SYMBOL_HASH_DISCRIMINATOR,
            Self::Keyword => Self::KEYWORD_HASH_DISCRIMINATOR,
            Self::Str => Self::STR_HASH_DISCRIMINATOR,
            Self::Int => Self::INT_HASH_DISCRIMINATOR,
            Self::Float => Self::FLOAT_HASH_DISCRIMINATOR,
            Self::Bool => Self::BOOL_HASH_DISCRIMINATOR,
        }
    }

    /// Canonical [`SexpShape`] embed target for the [`Self::Symbol`]
    /// atomic-payload arm on the AtomKind ⊂ SexpShape 6-of-12 carving —
    /// [`SexpShape::Symbol`]. Per-role peer of `Self::Symbol` on the
    /// closed-set atomic-payload → outer-shape embed axis; consumers
    /// reach for `AtomKind::SYMBOL_SHAPE` when the caller has a variant
    /// in hand at compile time and wants the canonical outer-shape
    /// identity without runtime dispatch through [`Self::sexp_shape`].
    ///
    /// Sibling posture to the six pre-existing per-role LABEL /
    /// HASH_DISCRIMINATOR aliases on this same closed-set algebra
    /// ([`Self::SYMBOL_LABEL`], [`Self::SYMBOL_HASH_DISCRIMINATOR`]) —
    /// each closes a distinct per-role sub-vocabulary axis on the
    /// AtomKind carving. This constant closes the THIRD per-role
    /// axis on [`AtomKind`] (the `SexpShape`-embed axis, paired with
    /// the pre-existing `&'static str` diagnostic-label axis + the
    /// `u8` cache-key axis) at ONE typed alias through the peer
    /// superset variant on the [`SexpShape`] closed set.
    pub const SYMBOL_SHAPE: SexpShape = SexpShape::Symbol;

    /// Canonical [`SexpShape`] embed target for the [`Self::Keyword`]
    /// atomic-payload arm on the AtomKind ⊂ SexpShape carving —
    /// [`SexpShape::Keyword`]. Per-role peer of `Self::Keyword`.
    pub const KEYWORD_SHAPE: SexpShape = SexpShape::Keyword;

    /// Canonical [`SexpShape`] embed target for the [`Self::Str`]
    /// atomic-payload arm on the AtomKind ⊂ SexpShape carving —
    /// [`SexpShape::String`]. Per-role peer of `Self::Str`; the
    /// `Str → String` wire-shape rename matches the peer
    /// [`Self::STRING_LABEL`] alias (both bind the AtomKind subset's
    /// `Str` variant to the SexpShape superset's `String` variant on
    /// their respective per-role sub-vocabulary axes).
    pub const STR_SHAPE: SexpShape = SexpShape::String;

    /// Canonical [`SexpShape`] embed target for the [`Self::Int`]
    /// atomic-payload arm on the AtomKind ⊂ SexpShape carving —
    /// [`SexpShape::Int`]. Per-role peer of `Self::Int`.
    pub const INT_SHAPE: SexpShape = SexpShape::Int;

    /// Canonical [`SexpShape`] embed target for the [`Self::Float`]
    /// atomic-payload arm on the AtomKind ⊂ SexpShape carving —
    /// [`SexpShape::Float`]. Per-role peer of `Self::Float`.
    pub const FLOAT_SHAPE: SexpShape = SexpShape::Float;

    /// Canonical [`SexpShape`] embed target for the [`Self::Bool`]
    /// atomic-payload arm on the AtomKind ⊂ SexpShape carving —
    /// [`SexpShape::Bool`]. Per-role peer of `Self::Bool`.
    pub const BOOL_SHAPE: SexpShape = SexpShape::Bool;

    /// Closed-set forced-arity ALL array over the canonical
    /// [`SexpShape`] embed targets on the AtomKind ⊂ SexpShape
    /// 6-of-12 carving, in declaration order matching [`Self::ALL`]
    /// element-wise (pinned by
    /// `atom_kind_shapes_align_with_all_by_index`). Sibling posture
    /// to [`Self::LABELS`] (`[&'static str; 6]` — per-role diagnostic
    /// bytes) and [`Self::HASH_DISCRIMINATORS`] (`[u8; 6]` — per-role
    /// nested-Atom cache-key bytes) on the SAME closed-set AtomKind
    /// algebra; where those two arrays lift the per-role
    /// `&'static str` and `u8` sub-vocabularies onto the substrate,
    /// this array lifts the per-role [`SexpShape`] embed-target
    /// sub-vocabulary at the same `[_; 6]` forced arity.
    ///
    /// Pre-lift the six [`SexpShape`] embed targets had NO per-role
    /// primitive on this closed-set algebra — a consumer with an
    /// `AtomKind` variant in hand at compile time reaching for the
    /// canonical embed target had to spell
    /// `AtomKind::Symbol.sexp_shape()` (runtime dispatch through the
    /// six-arm match body) OR re-derive the AtomKind ⊂ SexpShape
    /// variant pairing at the call site by importing both enums and
    /// spelling `SexpShape::Symbol` inline. Post-lift the SIX
    /// canonical embed targets bind at ONE `pub const` per role on
    /// the typed [`AtomKind`] algebra AND at [`Self::SHAPES`] as a
    /// family-wide forced-arity array — a future LSP / REPL
    /// completion bar keyed on `AtomKind::SHAPES` for the "which
    /// SexpShape does this AtomKind embed into?" outer-shape column,
    /// a `tatara-check` coverage sweep zipping `AtomKind::ALL` /
    /// `LABELS` / `HASH_DISCRIMINATORS` / `SHAPES` in lockstep for a
    /// family-wide (variant, label, byte, embed-target) quadruple
    /// render, or a Sekiban audit-trail metric jointly labeled by
    /// the embed-target's SexpShape identity reads through the typed
    /// constants on this subset algebra without re-deriving the
    /// 6-of-12 carving inline.
    ///
    /// Round-trip identity with the inverse projection
    /// [`crate::error::SexpShape::as_atom_kind`]: for every index `i`,
    /// `Self::SHAPES[i].as_atom_kind() == Some(Self::ALL[i])`
    /// (pinned by
    /// `atom_kind_shapes_align_with_all_by_index_through_as_atom_kind`) —
    /// the embed / project section closes as a family-wide array-
    /// indexed law rather than as a per-variant assertion sweep.
    /// Adding a hypothetical seventh atomic kind (e.g. `Char` for
    /// `#\x` reader syntax, `Bigint` for arbitrary-precision
    /// integers) extends [`Self::ALL`] AND [`Self::SHAPES`] AND
    /// [`SexpShape::ALL`] AND adds ONE per-role `pub const *_SHAPE`
    /// in lockstep — rustc's forced-arity check on the two `[_; N]`
    /// arrays fails compilation if EITHER ALL array grows without
    /// the other, AND the peer [`SexpShape::as_atom_kind`] arm must
    /// grow in lockstep to preserve the round-trip identity.
    ///
    /// Theory anchor: THEORY.md §III — the typescape; the six
    /// canonical [`SexpShape`] embed targets bind at ONE typed
    /// `[SexpShape; 6]` array on the closed-set AtomKind algebra
    /// rather than at zero-primitive-on-this-subset-plus-six-inline-
    /// lookups scattered across the substrate. THEORY.md §V.1 —
    /// knowable platform; the family's cardinality becomes a TYPE-
    /// level constant on the substrate algebra rather than a per-
    /// consumer runtime dispatch through the composition. THEORY.md
    /// §II.1 invariant 2 — free middle; the (embed, project) pair
    /// binds at THREE typed sites now — the projection method
    /// [`Self::sexp_shape`], this family-wide array, AND the peer
    /// inverse [`crate::error::SexpShape::as_atom_kind`] — with
    /// rustc-enforced consistency across all three. THEORY.md §VI.1
    /// — generation over composition; the family-wide contract
    /// sweeps (alignment with `ALL`, round-trip through
    /// `as_atom_kind`, membership through `sexp_shape`) emerge from
    /// the composition of TWO substrate primitives (this `pub const`
    /// array + the six per-role `pub const *_SHAPE` aliases) rather
    /// than as per-variant inline assertions duplicated at each call
    /// site.
    pub const SHAPES: [SexpShape; 6] = [
        Self::SYMBOL_SHAPE,
        Self::KEYWORD_SHAPE,
        Self::STR_SHAPE,
        Self::INT_SHAPE,
        Self::FLOAT_SHAPE,
        Self::BOOL_SHAPE,
    ];

    /// Project the typed marker into its matching [`SexpShape`]
    /// variant — `Symbol → SexpShape::Symbol`, `Keyword →
    /// SexpShape::Keyword`, `Str → SexpShape::String`, `Int →
    /// SexpShape::Int`, `Float → SexpShape::Float`, `Bool →
    /// SexpShape::Bool`. ONE projection on the closed-set atomic-
    /// payload algebra that [`crate::domain::sexp_shape`]'s outer-shape
    /// projection routes through for the six atom arms — so the
    /// (Atom variant, SexpShape variant) pairing binds at ONE site on
    /// the typed algebra rather than at six byte-identical inline arms
    /// in [`crate::domain::sexp_shape`]. Direct sibling to
    /// [`QuoteForm::sexp_shape`] — that closed enum carves the
    /// quote-family arms of [`SexpShape`]'s twelve-variant closed set,
    /// while this enum carves the atomic-payload arms.
    ///
    /// Each arm routes through the per-role `pub const` on `impl Self`
    /// ([`Self::SYMBOL_SHAPE`], [`Self::KEYWORD_SHAPE`],
    /// [`Self::STR_SHAPE`], [`Self::INT_SHAPE`], [`Self::FLOAT_SHAPE`],
    /// [`Self::BOOL_SHAPE`]) so the six canonical embed targets bind
    /// at ONE typed source of truth per role rather than as inline
    /// `SexpShape::X` literals scattered across the `match` body.
    /// Sibling posture to [`Self::label`]'s composition through
    /// [`Self::sexp_shape().label()`] and [`Self::hash_discriminator`]'s
    /// per-role routing through [`Self::SYMBOL_HASH_DISCRIMINATOR`] …
    /// [`Self::BOOL_HASH_DISCRIMINATOR`] — the three per-role axes on
    /// the AtomKind algebra (embed target, diagnostic label, cache-key
    /// byte) each surface their per-role bytes through the SAME
    /// per-role `pub const` shape.
    ///
    /// Composition law: for every [`Atom`] `a`,
    /// `crate::domain::sexp_shape(&Sexp::Atom(a.clone())) ==
    /// a.kind().sexp_shape()`. Pinned by the cross-projection round-trip
    /// test in this module, so a regression that drifts either side
    /// of the typed algebra (an [`Atom::kind`] arm or this
    /// [`Self::sexp_shape`] arm) surfaces immediately rather than as a
    /// silent operator-facing diagnostic drift at every
    /// `LispError::TypeMismatch.got` slot for an atomic witness.
    ///
    /// Post-lift routing pin
    /// `atom_kind_sexp_shape_routes_through_typed_per_role_constants`
    /// catches a regression that re-inlines the six `SexpShape::X`
    /// arm literals here and silently drifts ONE arm from the per-role
    /// `pub const` alias — the routing agreement is a TYPED CONSEQUENCE
    /// of the composition rather than literal discipline at two sites.
    ///
    /// Bidirectional dual: the inverse projection
    /// [`crate::error::SexpShape::as_atom_kind`] (12→6, partial)
    /// covers the 6-of-12 carving of [`SexpShape`] this embed
    /// reaches. The pair `(AtomKind::sexp_shape,
    /// SexpShape::as_atom_kind)` forms an `Iso(AtomKind, AtomShape ⊂
    /// SexpShape)`: every typed marker round-trips through the embed
    /// (`AtomKind::sexp_shape(k).as_atom_kind() == Some(k)` for every
    /// `k: AtomKind`), every atom-shape pre-image recovers the typed
    /// marker. The non-atom shapes (`Nil`, `List`, every quote-family
    /// wrapper) form the kernel of the inverse — `as_atom_kind`
    /// returns `None` for them. See [`crate::error::SexpShape::as_atom_kind`]'s
    /// docstring for the composition law's other direction +
    /// disjointness with the quote-family sibling
    /// `SexpShape::as_quote_form`.
    ///
    /// Theory anchor: THEORY.md §V.1 — knowable platform; the (Atom
    /// variant, SexpShape variant) pairing becomes a TYPE projection
    /// on the substrate algebra rather than six inline arms in
    /// [`crate::domain::sexp_shape`]. A typo or swap at the shape-
    /// projection site is no longer a runtime drift but a compile
    /// error against the typed projection. THEORY.md §II.1 invariant
    /// 2 — free middle; THREE consumers ([`Hash for Atom`] via
    /// [`Self::hash_discriminator`], [`crate::domain::sexp_shape`]
    /// via this method, and the future diagnostic / completion surface
    /// via [`Self::label`]) now route through ONE typed closed-set
    /// match family, so a regression that drifts ONE consumer's
    /// pairing from the others cannot reach the substrate's runtime.
    #[must_use]
    pub fn sexp_shape(self) -> SexpShape {
        match self {
            Self::Symbol => Self::SYMBOL_SHAPE,
            Self::Keyword => Self::KEYWORD_SHAPE,
            Self::Str => Self::STR_SHAPE,
            Self::Int => Self::INT_SHAPE,
            Self::Float => Self::FLOAT_SHAPE,
            Self::Bool => Self::BOOL_SHAPE,
        }
    }
}

// `impl fmt::Display for AtomKind` + `impl std::str::FromStr for AtomKind`
// + `impl crate::ClosedSet for AtomKind` + `pub struct UnknownAtomKind(pub
// String)` are generated by `#[derive(tatara_closed_set::DeriveClosedSet)]` on
// the enum declaration above. `label` delegates to the inherent
// `AtomKind::label` via `#[closed_set(via = "label")]` so the
// domain-canonical lowercase-vocabulary projection stays load-bearing (the
// six labels `"symbol" / "keyword" / "string" / "int" / "float" / "bool"`
// match the `SexpShape` atomic-subset labels byte-for-byte AND the
// diagnostic-rendering shape `LispError::TypeMismatch.got` keys on
// verbatim). The `display` flag emits the substrate-wide
// `f.write_str(Self::label(*self))` block. `#[closed_set(generate_unknown =
// "atom kind")]` emits the typed parse-rejection carrier with the
// substrate-wide `Debug + Clone + PartialEq + Eq + thiserror::Error`
// derives and the `#[error("unknown atom kind: {0}")]` annotation
// byte-for-byte; the explicit label pins the pre-lift wording even though
// the auto-derived `pascal_to_spaced_lowercase("AtomKind")` projects to
// the same `"atom kind"` literal.

/// Static panic message for [`Sexp::expect_quote_form`]'s asserted-total
/// face of the quote-family projection. Pre-lift this literal appeared
/// inline at five `.expect(...)` callsites (`Hash for Sexp`,
/// `Display for Sexp`, `domain::sexp_shape`, `domain::sexp_to_json`,
/// `interop::iac_forge_tag`); post-lift it lives at ONE named const so a
/// regression that drifts the diagnostic at one site silently from the
/// others becomes structurally impossible. Sibling to the per-projection
/// asserted-total faces across the substrate's typed algebras — the
/// message names the invariant the outer pattern proves, not the
/// substring grep'able by tests.
pub const QUOTE_FAMILY_PROJECTION_INVARIANT: &str =
    "matched quote-family variant must project to Some via as_quote_form";

/// Closed-set typed identifier for the four homoiconic prefix-wrappers in
/// the substrate's `Sexp` algebra — `'x` ([`Sexp::Quote`]), `` `x ``
/// ([`Sexp::Quasiquote`]), `,x` ([`Sexp::Unquote`]), `,@x`
/// ([`Sexp::UnquoteSplice`]) — paired with the projections each consumer
/// surface needs ([`Self::prefix`] for [`crate::ast::Sexp`]'s `Display`
/// impl AND the reader's prefix dispatch dual, [`Self::hash_discriminator`]
/// for [`crate::ast::Sexp`]'s `Hash` impl, [`Self::as_unquote_form`] for
/// the 2-of-4 subset gate the template-substitution surface keys on).
///
/// Mirror at the homoiconic-prefix-wrapper boundary of the prior-run
/// `UnquoteForm` (template-marker subset, 2 variants),
/// `CompilerSpecIoStage` (disk-persistence surface),
/// `TemplateInvariantKind` (bytecode-runtime surface), `MacroDefHead`
/// (macro-definition-head closed set), and `KwargPath` (kwargs-path-shape
/// surface) closed-set lifts: those enums key their respective rejection
/// or projection variants on a typed identity carried inside the variant's
/// data shape; this enum keys the FOUR distinct quote-family rendering /
/// hashing / template-substitution sites on a typed marker identity.
/// Adding a fifth homoiconic prefix-wrapper (e.g., a hypothetical `,~`
/// reverse-unquote) requires extending this enum, which rustc-enforces
/// matching at every projection site (`prefix`, `hash_discriminator`,
/// `as_unquote_form`, plus `Sexp::as_quote_form`'s match arm) — the closed
/// set becomes a TYPE rather than four `&'static str` / `u8` literals that
/// could drift independently across `Sexp::Display`'s prefix arm and
/// `Sexp::Hash`'s discriminator arm and the reader's prefix dispatch.
///
/// Subset-gate relationship to [`UnquoteForm`]: the template-substitution
/// surface's [`Sexp::as_unquote`] is now `as_quote_form().and_then(|(qf,
/// inner)| qf.as_unquote_form().map(|uf| (uf, inner)))` — the 2-of-4
/// projection lives at ONE site on this algebra ([`Self::as_unquote_form`])
/// rather than being re-derived at every consumer that wants only the
/// `{Unquote, UnquoteSplice}` subset. A future enum variant that joins
/// the template-substitution subset (e.g. a typed `defalias`-projected
/// fifth marker) extends [`UnquoteForm`] AND
/// [`Self::as_unquote_form`]'s arm together, with rustc binding the
/// extension through the projection's `Option` return type.
///
/// Theory anchor: THEORY.md §II.1 invariant 1 — typed entry; the
/// homoiconic-prefix-wrapper dispatch (the reader's prefix-to-variant
/// gate AND the Display impl's variant-to-prefix dual) IS the rust-level
/// typed-entry / typed-exit gate, and naming its closed-set identity
/// lifts the gate from per-site literal-pair discipline to ONE typed
/// enum the substrate's diagnostic promotions hang off of.
/// THEORY.md §V.1 — knowable platform; the closed set of homoiconic
/// prefix-wrappers becomes a TYPE rather than four `&'static str` / `u8`
/// literals scattered across Hash / Display / interop / sexp_shape — a
/// typo in any one site is no longer a runtime drift but a compile error
/// against the typed projection. THEORY.md §VI.1 — generation over
/// composition; the typed enum lands the structural-completeness floor
/// for the quote-family surface, parallel to how `UnquoteForm` lands it
/// for the template-marker subset and `MacroDefHead` for the
/// macro-definition-head surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, tatara_closed_set::DeriveClosedSet)]
#[closed_set(via = "prefix", display, generate_unknown = "quote form")]
pub enum QuoteForm {
    /// `'x` — literal-quote prefix. The `'` marker; the inner expression
    /// is NOT subject to macro substitution. Projects to NO
    /// `UnquoteForm` (the template-substitution surface ignores quote).
    Quote,
    /// `` `x `` — quasi-quote prefix. The `` ` `` marker; the inner
    /// expression is the template body inside which `,` and `,@` mark
    /// substitution points. Projects to NO `UnquoteForm` (a quasi-quote
    /// is the substitution SCOPE, not a substitution itself).
    Quasiquote,
    /// `,x` — single-value substitution. The `,` marker; the inner
    /// symbol is substituted with its bound value at template
    /// expansion. Projects to `UnquoteForm::Unquote` for the
    /// template-substitution surface.
    Unquote,
    /// `,@x` — list-splice substitution. The `,@` marker; the inner
    /// symbol must be bound to a list, whose elements are flattened
    /// into the containing list at template expansion. Projects to
    /// `UnquoteForm::Splice` for the template-substitution surface.
    UnquoteSplice,
}

impl QuoteForm {
    /// The closed set of four homoiconic prefix-wrappers — single
    /// source of truth that drives every per-variant projection
    /// ([`Self::prefix`] / [`fmt::Display`], [`Self::hash_discriminator`],
    /// [`Self::as_unquote_form`], [`Self::iac_forge_tag`],
    /// [`Self::sexp_shape`], [`Self::wrap`], and the [`Self::FromStr`]
    /// decode sweep keyed on [`Self::prefix`]).
    ///
    /// Adding a hypothetical fifth homoiconic prefix-wrapper (e.g.
    /// a `,~` reverse-unquote, a `,?` conditional-unquote, or a
    /// `#'` Common-Lisp function-quote literal) lands at one
    /// [`Self::ALL`] entry plus one arm per projection — exhaustively
    /// checked by the compiler (the `[Self; 4]` array literal forces
    /// the arity) AND by the per-variant truth-table tests below.
    ///
    /// Sibling closed-set lift to every other typed-shape enum the
    /// substrate carries: this crate's own
    /// [`crate::error::SexpShape::ALL`] (the twelve reachable outer
    /// shapes — superset of this enum's four via [`Self::sexp_shape`]),
    /// [`AtomKind::ALL`] (the six atomic-payload kinds — peer axis
    /// on the same algebra, also a 6-of-12 carving of `SexpShape`),
    /// [`crate::error::UnquoteForm::ALL`] (the two template-substitution
    /// markers — proper 2-of-4 subset of THIS enum via
    /// [`Self::as_unquote_form`]), and the cross-crate `tatara-process`
    /// family (`ConditionKind::ALL`, `ProcessPhase::ALL`,
    /// `ProcessSignal::ALL`, `ChannelKind::ALL`, `IntentKind::ALL`,
    /// `LifetimeKind::ALL`, `RequestorKind::ALL`, `ReceiptKind::ALL`,
    /// …) every one of which paired its typed projection with `ALL`
    /// before this lift.
    ///
    /// Future consumers that compose against `ALL`: LSP / REPL
    /// completion for the operator-facing rendered homoiconic prefix
    /// (every `'`/`` ` ``/`,`/`,@` substring an authoring tool would
    /// surface in a completion list keys on this set's projection
    /// through [`Self::prefix`]); `tatara-check` coverage assertions
    /// over which quote-family wrappers reach a `Sexp::Display` /
    /// `Hash for Sexp` / `as_unquote_form` consumer arm at all — the
    /// typed sweep replaces a per-callsite vocabulary of four
    /// `&'static str` / `u8` literals; any future audit-trail metric
    /// jointly labeled by [`Self::prefix`] (e.g.
    /// `tatara_lisp_quote_family_total{prefix="'"}`) — the metric
    /// label set IS [`Self::ALL`] mapped through [`Self::prefix`];
    /// any future structural rewriter (typed analogue of MLIR's
    /// `op.walk<QuoteFormOp>()`) that wants to sweep over every
    /// quote-family wrapper in a typed sequence.
    pub const ALL: [Self; 4] = [
        Self::Quote,
        Self::Quasiquote,
        Self::Unquote,
        Self::UnquoteSplice,
    ];

    /// Canonical `&'static str` reader-prefix of [`Self::Quote`] —
    /// `"'"`. The ONE canonical bytes-payload on the closed-set
    /// [`QuoteForm`] algebra shared by [`Self::prefix`]'s [`Self::Quote`]
    /// arm AND the [`crate::ast::Sexp`] `Display` arm the arm feeds.
    ///
    /// Sibling posture to the closed set of per-role `pub const`
    /// bytes on the substrate's other closed-set outer algebras:
    /// [`crate::error::MacroDefHead::DEFMACRO_KEYWORD`] /
    /// [`crate::error::MacroDefHead::DEFPOINT_TEMPLATE_KEYWORD`] /
    /// [`crate::error::MacroDefHead::DEFCHECK_KEYWORD`] (per-role
    /// head-keyword algebra on the CL macro-definition surface),
    /// [`crate::ast::Atom::TRUE_LITERAL`] /
    /// [`crate::ast::Atom::FALSE_LITERAL`] (per-role Scheme-bool
    /// spelling algebra on the atomic-payload surface),
    /// [`crate::macro_expand::MacroParams::REST_MARKER`] /
    /// [`crate::macro_expand::MacroParams::OPTIONAL_MARKER`] (per-role
    /// CL lambda-list-keyword algebra on the macro-param surface).
    ///
    /// The `char`-level peer of THIS `&'static str` constant is
    /// [`Self::QUOTE_LEAD`] — the first (and only) char of this
    /// prefix. The structural round-trip law between the two is
    /// `Self::QUOTE_PREFIX.chars().next() == Some(Self::QUOTE_LEAD)`
    /// AND `Self::QUOTE_PREFIX.len() ==
    /// Self::QUOTE_LEAD.len_utf8()` — the ONE `char` byte
    /// composes the ONE `&'static str` prefix. Pinned by
    /// `quote_form_per_role_prefixes_route_through_matching_lead_char_for_single_char_prefixes`.
    ///
    /// A regression that inlines the `"'"` literal at
    /// [`Self::prefix`]'s [`Self::Quote`] arm and drifts the constant
    /// silently (e.g. an ELisp-compat port of the quote prefix to
    /// `#'`, a hypothetical Racket-compat swap to a distinct byte)
    /// fails at the algebra's `prefix()` path-uniformity pin
    /// (`quote_form_prefix_routes_through_typed_per_role_constants`)
    /// rather than at silent reader-family drift where the
    /// [`Sexp::Display`] round-trip breaks.
    pub const QUOTE_PREFIX: &'static str = "'";

    /// Canonical `&'static str` reader-prefix of [`Self::Quasiquote`]
    /// — `` "`" ``. Sibling of [`Self::QUOTE_PREFIX`] on the closed-set
    /// per-role quote-family prefix-bytes axis; see
    /// [`Self::QUOTE_PREFIX`] for the algebra-level round-trip +
    /// disjointness contracts every sibling shares. The `char`-level
    /// peer is [`Self::QUASIQUOTE_LEAD`].
    pub const QUASIQUOTE_PREFIX: &'static str = "`";

    /// Canonical `&'static str` reader-prefix of [`Self::Unquote`] —
    /// `","`. Sibling of [`Self::QUOTE_PREFIX`] on the closed-set
    /// per-role quote-family prefix-bytes axis; see
    /// [`Self::QUOTE_PREFIX`] for the algebra-level round-trip +
    /// disjointness contracts every sibling shares. The `char`-level
    /// peer is [`Self::UNQUOTE_LEAD`] (shared with
    /// [`Self::UNQUOTE_SPLICE_PREFIX`]'s lead byte — see the
    /// [`Self::UNQUOTE_LEAD`] docstring for the shared-lead-char
    /// discipline the two prefixes disambiguate on).
    pub const UNQUOTE_PREFIX: &'static str = ",";

    /// Canonical `&'static str` reader-prefix of [`Self::UnquoteSplice`]
    /// — `",@"`. The ONLY two-char prefix on the closed set; every
    /// other [`Self::PREFIXES`] entry is a single `char` rendered as
    /// `&'static str`. Sibling of [`Self::QUOTE_PREFIX`] on the closed-
    /// set per-role quote-family prefix-bytes axis.
    ///
    /// Structural composition law: `Self::UNQUOTE_SPLICE_PREFIX ==
    /// format!("{}{}", Self::UNQUOTE_LEAD, Self::SPLICE_DISCRIMINATOR)`
    /// — the two-char prefix decomposes cleanly into the ONE shared
    /// lead byte [`Self::UNQUOTE_LEAD`] + the ONE splice-promotion
    /// discriminator [`Self::SPLICE_DISCRIMINATOR`], both `char`-level
    /// constants on this algebra. Pinned by
    /// `quote_form_unquote_splice_prefix_constant_composes_from_unquote_lead_and_splice_discriminator`
    /// (byte-level composition through the per-role `pub const`) as a
    /// section-for-retraction peer of the pre-existing
    /// `quote_form_unquote_splice_prefix_composes_from_unquote_lead_and_splice_discriminator`
    /// pin (byte-level composition through the [`Self::prefix`]
    /// method).
    pub const UNQUOTE_SPLICE_PREFIX: &'static str = ",@";

    /// The closed-set forced-arity ALL array over the quote-family
    /// reader-prefix `&'static str` bytes in canonical declaration
    /// order matching [`Self::ALL`] element-wise. Sibling posture to
    /// [`crate::error::MacroDefHead::KEYWORDS`] (`[&'static str; 3]`
    /// on the CL macro-definition head algebra),
    /// [`crate::ast::Atom::BOOL_LITERALS`] (`[&'static str; 2]` on the
    /// Scheme-bool spelling algebra), and
    /// [`crate::macro_expand::MacroParams::LAMBDA_LIST_KEYWORDS`]
    /// (`[&'static str; 2]` on the CL lambda-list-keyword algebra) —
    /// every closed-set outer projection on the substrate now pins
    /// its canonical bytes at ONE `pub const` per role plus an ALL
    /// array for family-wide consumers.
    ///
    /// Adding a hypothetical fifth homoiconic prefix (a `,~`
    /// reverse-unquote, a `,?` conditional-unquote, a `#'` Common-
    /// Lisp function-quote) extends [`Self::ALL`] AND
    /// [`Self::PREFIXES`] AND [`Self::prefix`]'s arm AND one new
    /// per-role `pub const` in lockstep — rustc's forced-arity check
    /// on `[&'static str; N]` fails compilation if either ALL array
    /// grows without the other.
    ///
    /// Future consumers that compose against [`Self::PREFIXES`]:
    /// - LSP / REPL completion for the operator-facing rendered
    ///   homoiconic prefix bar — the completion set IS
    ///   [`Self::PREFIXES`] rather than four hand-enumerated
    ///   `&'static str` literals per completion provider.
    /// - `tatara-check` coverage assertions that sweep workspace
    ///   `.lisp` files for every canonical quote-family prefix — the
    ///   typed sweep replaces per-consumer inline enumeration of the
    ///   four literals.
    /// - Any future audit-trail metric jointly labeled by
    ///   [`Self::prefix`] (e.g.
    ///   `tatara_lisp_quote_family_total{prefix="'"}`) — the metric
    ///   label set IS [`Self::PREFIXES`] mapped through
    ///   [`Self::prefix`].
    pub const PREFIXES: [&'static str; 4] = [
        Self::QUOTE_PREFIX,
        Self::QUASIQUOTE_PREFIX,
        Self::UNQUOTE_PREFIX,
        Self::UNQUOTE_SPLICE_PREFIX,
    ];

    /// Canonical `&'static str` prefix that paired with the variant
    /// renders the homoiconic form — [`Self::QUOTE_PREFIX`] for
    /// [`Self::Quote`], [`Self::QUASIQUOTE_PREFIX`] for
    /// [`Self::Quasiquote`], [`Self::UNQUOTE_PREFIX`] for
    /// [`Self::Unquote`], [`Self::UNQUOTE_SPLICE_PREFIX`] for
    /// [`Self::UnquoteSplice`]. Threaded through
    /// [`crate::ast::Sexp`]'s `Display` impl so the per-variant prefix
    /// rendering lives at ONE site on this algebra rather than four
    /// inline literal strings across the Display arms.
    ///
    /// Post-lift the four arms route through the per-role `pub const`
    /// bytes on the closed-set [`QuoteForm`] algebra rather than
    /// inline `&'static str` literals — so a rename of ONE canonical
    /// prefix bytes (an ELisp-compat port of `Quote` to `"#'"`, a
    /// hypothetical Racket-compat swap of `Quasiquote`, a Common-Lisp-
    /// standard rename of `UnquoteSplice` to `",."`) lands as ONE
    /// edit to the matching `pub const` — every downstream consumer
    /// that binds to the algebra ([`crate::ast::Sexp`]'s `Display`
    /// impl, the reader's tokenizer round-trip law, the future
    /// canonical-form taggers) inherits the rename mechanically.
    ///
    /// Structural dual of the reader's [`crate::reader::read_quoted`]
    /// dispatch: the reader maps prefix-tokens to `Sexp::{Quote,
    /// Quasiquote, Unquote, UnquoteSplice}` constructors; this method
    /// maps the typed `QuoteForm` marker back to its canonical prefix
    /// string. Adding a fifth prefix extends both sides — the reader's
    /// tokenizer + dispatch AND this method — with rustc enforcing
    /// the pair through the closed-set enum. Round-trip:
    /// `read(format!("{}{inner}", qf.prefix()))` produces the
    /// `Sexp::*` variant matching `qf`, by construction.
    ///
    /// The `&'static str` lifetime is load-bearing: it lets every
    /// consumer (Display arm, future format strings, future interop
    /// canonical-form taggers) project through this method without
    /// an allocation, parallel to how [`UnquoteForm::marker`]
    /// projects its 2-of-4 subset surface.
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Quote => Self::QUOTE_PREFIX,
            Self::Quasiquote => Self::QUASIQUOTE_PREFIX,
            Self::Unquote => Self::UNQUOTE_PREFIX,
            Self::UnquoteSplice => Self::UNQUOTE_SPLICE_PREFIX,
        }
    }

    /// Canonical `'` LEAD `char` of [`Self::Quote`]'s [`Self::prefix`]
    /// (`"'"`) — the ONE canonical `char` on the [`QuoteForm`] algebra the
    /// substrate's Quote-family single-quote lead-byte disjointness
    /// contract binds to.
    ///
    /// Sibling posture to the closed set of `pub const` reader-punctuation
    /// canonical `char` bytes on the substrate:
    /// [`Self::SPLICE_DISCRIMINATOR`] (`'@'`),
    /// [`crate::ast::Atom::STR_DELIMITER`] (`'"'`),
    /// [`crate::ast::Atom::STR_ESCAPE_LEAD`] (`'\\'`),
    /// [`crate::ast::Atom::KEYWORD_MARKER_LEAD`] (`':'`),
    /// [`crate::ast::Atom::BOOL_LITERAL_LEAD`] (`'#'`),
    /// [`crate::ast::Sexp::LIST_OPEN`] (`'('`),
    /// [`crate::ast::Sexp::LIST_CLOSE`] (`')'`),
    /// [`crate::ast::Sexp::COMMENT_LEAD`] (`';'`),
    /// [`crate::ast::Sexp::COMMENT_TERM`] (`'\n'`) — every canonical per-
    /// role byte the reader's tokenizer specialises on is a `pub const`
    /// on its owning closed-set algebra. This constant closes the Quote-
    /// family single-quote lead byte at the SAME algebra as the
    /// [`Self::lead_char`] projection (whose [`Self::Quote`] arm returns
    /// this byte) AND the [`Self::from_lead_char`] inverse (whose match
    /// arm decodes this byte back to [`Self::Quote`]).
    ///
    /// Structural round-trip contract:
    /// `Self::from_lead_char(Self::QUOTE_LEAD) == Some(Self::Quote)`
    /// AND `Self::Quote.lead_char() == Self::QUOTE_LEAD` — pinned by
    /// `quote_form_lead_constants_round_trip_through_lead_char_projections`.
    /// A regression that drifts EITHER the constant OR the paired
    /// projection surfaces at the pin rather than at a silent tokenizer
    /// drift where `'foo` classifies as a bare atom instead of
    /// [`crate::ast::Sexp::Quote`].
    ///
    /// Disjointness contract: `QUOTE_LEAD`'s byte MUST differ from
    /// [`Self::QUASIQUOTE_LEAD`], [`Self::UNQUOTE_LEAD`],
    /// [`Self::SPLICE_DISCRIMINATOR`],
    /// [`crate::ast::Atom::STR_DELIMITER`],
    /// [`crate::ast::Atom::STR_ESCAPE_LEAD`],
    /// [`crate::ast::Atom::KEYWORD_MARKER_LEAD`],
    /// [`crate::ast::Atom::BOOL_LITERAL_LEAD`],
    /// [`crate::ast::Sexp::LIST_OPEN`], [`crate::ast::Sexp::LIST_CLOSE`],
    /// [`crate::ast::Sexp::COMMENT_LEAD`], and
    /// [`crate::ast::Sexp::COMMENT_TERM`] — every other closed-set outer-
    /// marker byte the reader's tokenizer specialises on. A collision
    /// would silently break the reader's outer dispatch. Pinned by
    /// `quote_form_lead_constants_distinct_from_every_other_algebra_marker_char`.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 2 — free middle; the
    /// (Quote-family single-quote lead byte, canonical `'\''`) pairing
    /// binds at ONE constant on the closed-set [`QuoteForm`] algebra
    /// regardless of which paired projection reaches in. THEORY.md §V.1
    /// — knowable platform; the canonical Quote-family lead byte becomes
    /// a TYPE-level constant on the substrate algebra rather than an
    /// inline `'\''` char literal at [`Self::lead_char`]'s [`Self::Quote`]
    /// arm AND at [`Self::from_lead_char`]'s decode arm.
    pub const QUOTE_LEAD: char = '\'';

    /// Canonical `` ` `` LEAD `char` of [`Self::Quasiquote`]'s
    /// [`Self::prefix`] (`` "`" ``) — sibling of [`Self::QUOTE_LEAD`] on
    /// the closed-set quote-family lead-byte axis. See
    /// [`Self::QUOTE_LEAD`] for the algebra-level round-trip +
    /// disjointness contracts every sibling shares. Bound by
    /// [`Self::lead_char`]'s [`Self::Quasiquote`] arm AND
    /// [`Self::from_lead_char`]'s decode arm.
    pub const QUASIQUOTE_LEAD: char = '`';

    /// Canonical `,` LEAD `char` SHARED by [`Self::Unquote`]'s
    /// [`Self::prefix`] (`","`) AND [`Self::UnquoteSplice`]'s
    /// [`Self::prefix`] (`",@"`) — the splice's two-char prefix opens
    /// with this byte and disambiguates on the peek-then-consume
    /// [`Self::SPLICE_DISCRIMINATOR`] second char inside
    /// [`crate::reader::tokenize`]. Sibling of [`Self::QUOTE_LEAD`] on
    /// the closed-set quote-family lead-byte axis; see
    /// [`Self::QUOTE_LEAD`] for the algebra-level round-trip +
    /// disjointness contracts every sibling shares. Bound by
    /// [`Self::lead_char`]'s `Self::Unquote | Self::UnquoteSplice` merged
    /// arm AND [`Self::from_lead_char`]'s decode arm.
    ///
    /// Composition identity with [`Self::SPLICE_DISCRIMINATOR`]:
    /// `format!("{}{}", Self::UNQUOTE_LEAD, Self::SPLICE_DISCRIMINATOR)
    /// == Self::UnquoteSplice.prefix()` — the two byte-level constants
    /// on the closed-set [`QuoteForm`] algebra compose the ONLY two-char
    /// [`Self::prefix`] in the closed set. Pinned by
    /// `quote_form_unquote_splice_prefix_composes_from_unquote_lead_and_splice_discriminator`.
    /// A regression that renames EITHER constant without touching the
    /// paired [`Self::UnquoteSplice`]'s [`Self::prefix`] arm surfaces
    /// here rather than as a silent `,@` reader drift.
    pub const UNQUOTE_LEAD: char = ',';

    /// The closed-set forced-arity ALL array over the quote-family
    /// DISTINCT reader-lead-byte `char`s in canonical declaration
    /// order matching [`Self::ALL`]'s three-of-four distinct-lead-
    /// byte projection through [`Self::lead_char`] — [`Self::QUOTE_LEAD`]
    /// (`'\''` — the [`Self::Quote`] lead byte), [`Self::QUASIQUOTE_LEAD`]
    /// (`` '`' `` — the [`Self::Quasiquote`] lead byte),
    /// [`Self::UNQUOTE_LEAD`] (`','` — the SHARED lead byte of BOTH
    /// [`Self::Unquote`] AND [`Self::UnquoteSplice`], with the splice
    /// promotion living at the reader's peek-then-consume
    /// [`Self::SPLICE_DISCRIMINATOR`] second-char arm rather than at a
    /// distinct lead byte).
    ///
    /// The `[char; 3]` cardinality (vs the peer [`Self::PREFIXES`]
    /// `[&'static str; 4]`) IS the structural axis distinguishing the
    /// DISTINCT-lead-byte sub-vocabulary from the PER-VARIANT-prefix
    /// sub-vocabulary — three-of-four distinct-lead-byte collapse is
    /// definitional (only [`Self::UnquoteSplice`]'s two-char `,@`
    /// prefix shares its lead byte with a sibling variant; every other
    /// variant owns its lead byte outright). The shape asymmetry
    /// between the two ALL arrays encodes the shared-lead-byte
    /// collapse identity on the closed-set [`QuoteForm`] algebra at
    /// the type-system level: a consumer that reaches for
    /// [`Self::LEADS`] reads the distinct-lead-byte cardinality
    /// directly off the array's forced arity rather than through a
    /// per-consumer `HashSet`-then-count over [`Self::PREFIXES`]'s
    /// first chars.
    ///
    /// Sibling posture to [`Self::PREFIXES`] (`[&'static str; 4]` on
    /// the per-variant reader-prefix axis) AND [`Self::IAC_FORGE_TAGS`]
    /// (`[&'static str; 4]` on the per-variant canonical-form tag
    /// axis) — those two ALL arrays close the per-variant axes of the
    /// outer-tokenizer `QuoteForm` closed set; this ALL array closes
    /// the peer DISTINCT-lead-byte axis at the SHAPE-ASYMMETRIC
    /// `[char; N]` cardinality. Also sibling-shape to
    /// [`crate::ast::Sexp::LIST_DELIMITERS`] (`[char; 2]` on the outer-
    /// structural paired-delimiter axis), [`Atom::SELF_ESCAPE_TABLE`]
    /// (`[char; 2]` on the inner-Str-payload self-escape axis), and
    /// [`Atom::BOOL_LITERALS`] (`[&'static str; 2]` on the Scheme-bool
    /// spelling axis) — every closed-set outer projection on the
    /// substrate now pins its canonical bytes at ONE `pub const` per
    /// role plus an ALL array for family-wide consumers.
    ///
    /// Adding a hypothetical fifth homoiconic prefix with a DISTINCT
    /// lead byte (a `~` reverse-unquote, a `?` conditional-unquote, a
    /// `#` reader-macro-lead) extends [`Self::ALL`] AND
    /// [`Self::PREFIXES`] AND [`Self::LEADS`] AND [`Self::lead_char`]'s
    /// arm AND [`Self::from_lead_char`]'s arm AND one new per-role
    /// `pub const` in lockstep — rustc's forced-arity check on
    /// `[char; N]` fails compilation if the LEADS array grows without
    /// the paired algebra constant, and the paired PREFIXES /
    /// IAC_FORGE_TAGS arrays extend by ONE row each in lockstep. A
    /// fifth prefix that SHARES its lead byte with an existing variant
    /// (like the splice's `,@` sharing with unquote's `,`) leaves
    /// [`Self::LEADS`]'s cardinality unchanged — the DISTINCT-lead-
    /// byte set is invariant under such an extension, closing the
    /// splice-family promotion pattern at the ALL-array level.
    ///
    /// Future consumers that compose against [`Self::LEADS`]:
    /// - LSP / REPL completion for the operator-facing reader-entry
    ///   lead-byte set — the completion set IS [`Self::LEADS`] rather
    ///   than three hand-enumerated `char` literals per completion
    ///   provider.
    /// - The reader's outer tokenizer pre-match check that gates the
    ///   quote-family dispatch — the check IS
    ///   `Self::LEADS.contains(&ch)` rather than three inline
    ///   `ch == Self::QUOTE_LEAD || ch == Self::QUASIQUOTE_LEAD ||
    ///   ch == Self::UNQUOTE_LEAD` disjuncts, and the sweep binds
    ///   through the ALL array's forced arity.
    /// - A hypothetical `tatara_lisp_quote_family_lead_total{lead="'"}`
    ///   metric surface — the label-set generator sweeps
    ///   [`Self::LEADS`] verbatim rather than re-typing the three
    ///   distinct-lead bytes inline at each recorder.
    /// - Any future syntax-highlighter / structural editor that needs
    ///   the reader-entry lead-byte set for classification — the
    ///   editor's per-char classifier binds through [`Self::LEADS`]
    ///   rather than three parallel `char`-literal patterns.
    ///
    /// Theory anchor: THEORY.md §III — the typescape; the three
    /// distinct quote-family reader-lead bytes now bind at ONE typed
    /// `[char; 3]` array on the closed-set [`QuoteForm`] algebra
    /// rather than as three inline algebra-constant enumerations at
    /// every consumer that iterates the distinct-lead-byte sub-
    /// vocabulary. THEORY.md §V.1 — knowable platform; the distinct-
    /// lead-byte sub-vocabulary becomes load-bearing typed data on
    /// the closed-set outer [`QuoteForm`] algebra. THEORY.md §VI.1 —
    /// generation over composition; the shared-lead-byte collapse
    /// identity (four variants → three distinct lead bytes)
    /// composes at ONE typed ALL array whose shape-asymmetric
    /// cardinality (3 vs [`Self::PREFIXES`]'s 4) IS the collapse
    /// invariant carried at the type-system level.
    pub const LEADS: [char; 3] = [Self::QUOTE_LEAD, Self::QUASIQUOTE_LEAD, Self::UNQUOTE_LEAD];

    /// Canonical FIRST-char of [`Self::prefix`] — [`Self::QUOTE_LEAD`]
    /// for [`Self::Quote`], [`Self::QUASIQUOTE_LEAD`] for
    /// [`Self::Quasiquote`], [`Self::UNQUOTE_LEAD`] for BOTH
    /// [`Self::Unquote`] AND [`Self::UnquoteSplice`] (the splice's two-
    /// char `,@` prefix shares its lead byte with bare unquote and
    /// disambiguates on the peek-then-consume `@` second char inside
    /// [`crate::reader::tokenize`]).
    /// The three-of-four collapse onto three distinct lead chars is
    /// structurally fixed — the reader's outer tokenizer dispatch
    /// selects between quote-family entry and every non-quote-family
    /// arm on lead char alone, with the `,`-vs-`,@` disambiguation
    /// falling out of the reader's second-char peek.
    ///
    /// Structural dual of [`Self::from_lead_char`]: this method projects
    /// the closed-set marker to its canonical reader-punctuation lead
    /// char; the sibling projects the lead char back to the DEFAULT
    /// marker on that char (`,` → [`Self::Unquote`], with the splice
    /// promotion living at the reader's peek arm rather than at
    /// [`Self::from_lead_char`]'s decode). Every variant round-trips
    /// through the composition `Self::from_lead_char(qf.lead_char())`,
    /// with the `{Unquote, UnquoteSplice}` two-of-four collapsing onto
    /// `Some(Unquote)` per the shared-lead-char structural identity.
    ///
    /// The `const` qualifier is load-bearing: [`crate::reader::tokenize`]
    /// binds its outer-match quote-family dispatch to this projection
    /// via a pre-match `Self::from_lead_char` check, and future consumer
    /// sites (e.g. `const` array literals of every reader-recognized
    /// lead byte the tokenizer could dispatch on, LSP completion
    /// generators that pre-materialize the lead-char set) route through
    /// this projection at compile time. Sibling posture to
    /// [`crate::ast::Atom::STR_DELIMITER`] one axis over on the same
    /// closed-set-lead-char algebra — that constant is the ONE `char`
    /// the `Token::Str` open/close/self-escape/bare-atom-terminator
    /// FOUR sites in the reader pair with; this method is the ONE
    /// projection the `Token::Quoted(QuoteForm)` outer-dispatch AND
    /// the same bare-atom-terminator disjunct pair with across FOUR
    /// per-variant lead chars.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 1 — typed entry; the
    /// reader's per-char quote-family dispatch IS the typed-entry gate,
    /// and lifting its (char, `QuoteForm`) pairing to ONE projection
    /// method plus one inverse (see [`Self::from_lead_char`]) closes
    /// the tokenizer's outer-arm entry surface onto the closed-set
    /// algebra rather than four inline `char` literals scattered across
    /// three tokenizer arms (`'\''` / `` '`' `` / `','` outer-match arms)
    /// AND three bare-atom-terminator disjuncts (`ch == '\''` / `ch ==
    /// '`'` / `ch == ','`). THEORY.md §V.1 — knowable platform; the
    /// closed set of quote-family lead chars becomes a TYPE (the
    /// enum's arms projected through this method) rather than four
    /// literal `char` values scattered across the reader's outer
    /// dispatch AND its bare-atom terminator. THEORY.md §VI.1 —
    /// generation over composition; a fifth homoiconic prefix
    /// (hypothetical `,~` reverse-unquote, `#'` function-quote,
    /// `#[…]` vector-quote) extends [`Self`] AND this method AND
    /// [`Self::from_lead_char`] AND the tokenizer's pre-match check
    /// in lockstep, with rustc binding the extension through
    /// exhaustiveness over the closed enum.
    #[must_use]
    pub const fn lead_char(self) -> char {
        match self {
            Self::Quote => Self::QUOTE_LEAD,
            Self::Quasiquote => Self::QUASIQUOTE_LEAD,
            Self::Unquote | Self::UnquoteSplice => Self::UNQUOTE_LEAD,
        }
    }

    /// Inverse of [`Self::lead_char`] on the three-of-four distinct
    /// lead chars — `'\''` decodes to `Some(Self::Quote)`, `` '`' ``
    /// decodes to `Some(Self::Quasiquote)`, `','` decodes to
    /// `Some(Self::Unquote)` (the DEFAULT variant on the shared `,`
    /// lead char; the two-char `,@` splice promotion lives at
    /// [`crate::reader::tokenize`]'s peek-then-consume `@` disambiguator
    /// rather than at this decode). Every other `char` yields `None` —
    /// the closed-set guarantee on [`Self`] AND on the tokenizer's
    /// outer-arm set (whitespace, `(`, `)`, [`crate::ast::Atom::STR_DELIMITER`],
    /// `;`, bare atom) ensures the four typed markers partition the
    /// three distinct lead chars injectively against every other
    /// tokenizer-recognized entry char.
    ///
    /// ONE consumer entrypoint the reader's `tokenize` binds against:
    /// the outer-match quote-family dispatch was pre-lift a hand-rolled
    /// three-arm cascade (`'\''` / `` '`' `` / `','`) with per-arm
    /// `Token::Quoted(QuoteForm::*)` construction and a fourth
    /// `Token::Quoted(QuoteForm::UnquoteSplice)` arm buried inside the
    /// `','`-arm's peek branch; post-lift the tokenizer pre-checks
    /// `Self::from_lead_char(c)` before the outer match, promotes the
    /// returned `Self::Unquote` to `Self::UnquoteSplice` on second-char
    /// `@`, and emits ONE `Token::Quoted(final_qf)` — the (lead char,
    /// [`Self`] marker) pairing binds at ONE site on the closed-set
    /// algebra rather than at three inline `char` literals across
    /// three outer-match arms. The bare-atom terminator disjunct at
    /// the reader's `Token::Atom` accumulator loop routes through
    /// `Self::from_lead_char(ch).is_some()` so the three
    /// quote-family-lead disjuncts (`ch == '\''` / `ch == '`'` /
    /// `ch == ','`) collapse to ONE gate — a regression that drifts
    /// one bare-atom-terminator disjunct from the outer-dispatch's
    /// quote-family arm becomes structurally impossible because
    /// there is exactly ONE decode both sites consume.
    ///
    /// The two-of-four collapse onto `Some(Self::Unquote)` for the
    /// `,` lead char is INTENTIONAL: `Self::UnquoteSplice` has NO
    /// distinct lead char; the tokenizer must see two consecutive
    /// chars (`,` then `@`) to promote the decoded `Self::Unquote`
    /// to `Self::UnquoteSplice`. Placing the promotion at the
    /// reader's peek arm rather than at this decode keeps the
    /// (char → marker) projection at the closed-set algebra's
    /// character-boundary surface (one char in, one variant out)
    /// and the (two-char sequence → splice) promotion at the
    /// tokenizer's streaming surface (peek and consume the second
    /// char). This split parallels the reader's split of `Token::Str`
    /// into open-delimiter dispatch ([`crate::ast::Atom::STR_DELIMITER`])
    /// AND inner-payload accumulation — the closed-set char algebra
    /// decodes the entry char; the streaming reader handles multi-
    /// char follow-through.
    ///
    /// Sibling to [`crate::ast::Atom::from_lexeme`] one axis over on
    /// the same typed-entry family — that method decodes a bare-atom
    /// lexeme into a typed [`crate::ast::Atom`] variant; this method
    /// decodes a single lead char into a typed [`Self`] variant. Both
    /// map the reader's per-char / per-lexeme classification surface
    /// onto the substrate's closed-set algebra so the reader's outer
    /// dispatch binds through ONE typed decode rather than through
    /// scattered per-arm `char` / `&str` literal patterns.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 1 — typed entry; the
    /// reader's per-char quote-family classification IS the typed-entry
    /// gate. THEORY.md §V.1 — knowable platform; the reader's outer
    /// dispatch AND the bare-atom terminator each route through ONE
    /// typed decode against the closed-set algebra rather than through
    /// three (or four) parallel `char`-literal patterns that could
    /// drift independently — a regression that renames one lead char
    /// without updating the sibling site fails at rustc / test time
    /// rather than as a silent tokenizer drift.
    #[must_use]
    pub const fn from_lead_char(c: char) -> Option<Self> {
        match c {
            Self::QUOTE_LEAD => Some(Self::Quote),
            Self::QUASIQUOTE_LEAD => Some(Self::Quasiquote),
            Self::UNQUOTE_LEAD => Some(Self::Unquote),
            _ => None,
        }
    }

    /// Canonical SECOND char of [`Self::UnquoteSplice`]'s two-char `,@`
    /// [`Self::prefix`] — the ONE `'@'` byte the reader's peek-then-
    /// consume splice-promotion arm inside [`crate::reader::tokenize`]
    /// disambiguates on. Sibling posture to [`crate::ast::Atom::STR_DELIMITER`]
    /// (one-char Str-payload delimiter shared across four `"`-round-
    /// trip sites) AND to [`crate::ast::Sexp::COMMENT_LEAD`] (one-char
    /// line-comment lead shared across two `;`-boundary sites) — those
    /// two constants project a single byte onto their respective closed-
    /// set algebras (`Atom` and outer-`Sexp`); this constant projects
    /// the single byte that composes the `,` [`Self::Unquote`]
    /// [`Self::lead_char`] into the two-char [`Self::UnquoteSplice`]
    /// [`Self::prefix`] onto the same closed-set [`Self`] algebra.
    ///
    /// The `,@` splice is the ONLY multi-char [`Self::prefix`] in the
    /// closed set — [`Self::Quote`] / [`Self::Quasiquote`] / [`Self::Unquote`]
    /// each render as a single [`Self::lead_char`] byte; only
    /// [`Self::UnquoteSplice`] appends this discriminator. The
    /// composition [`Self::Unquote::prefix()`] + `SPLICE_DISCRIMINATOR`
    /// == [`Self::UnquoteSplice::prefix()`] IS the structural identity
    /// the reader's peek arm depends on — pinned by
    /// `quote_form_unquote_splice_prefix_composes_from_unquote_prefix_and_splice_discriminator`.
    /// A future hypothetical fifth homoiconic prefix with its own two-
    /// char extension (e.g. `,~` reverse-unquote via a `~` discriminator,
    /// `#'` function-quote via a `'` discriminator) extends [`Self`]
    /// AND a per-variant promotion peer (extending
    /// [`Self::promote_via_next_char`]) in lockstep — rustc binds the
    /// extension through exhaustiveness over the closed enum.
    ///
    /// The `const` qualifier is load-bearing: [`Self::promote_via_next_char`]'s
    /// body binds through this constant in a `const fn` context so the
    /// reader's peek arm consumes the promotion table at compile time.
    /// Sibling posture to [`crate::ast::Atom::STR_DELIMITER`],
    /// [`crate::ast::Atom::KEYWORD_MARKER`], [`crate::ast::Sexp::LIST_OPEN`],
    /// [`crate::ast::Sexp::LIST_CLOSE`], [`crate::ast::Sexp::COMMENT_LEAD`] —
    /// every canonical reader-punctuation constant on the substrate is a
    /// `pub const` on its owning closed-set algebra.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 1 — typed entry; the
    /// reader's two-char splice-promotion gate IS the typed-entry gate
    /// on the `,@` boundary, and lifting the `@` discriminator to ONE
    /// canonical byte on the closed-set algebra closes the gate's
    /// entry-char identity onto the algebra rather than at an inline
    /// `char` literal at the reader's peek arm. THEORY.md §V.1 —
    /// knowable platform; the splice-promotion discriminator becomes a
    /// TYPED byte on the substrate algebra rather than an inline `'@'`
    /// scattered across the reader — a regression that renames the byte
    /// without updating the sibling promotion peer fails at rustc /
    /// test time rather than as a silent tokenizer drift where `,@xs`
    /// forms silently degrade to `,` + `@xs` two-token sequences.
    pub const SPLICE_DISCRIMINATOR: char = '@';

    /// Closed-set forced-arity ALL array over the canonical promotion
    /// triples on the substrate's quote-family algebra —
    /// `(head_variant, next_char_discriminator, promoted_variant)` for
    /// every `(head, next)` pair whose [`Self::promote_via_next_char`]
    /// projection yields `Some(promoted)`. Post-lift the promotion
    /// algebra's canonical triples bind at ONE typed
    /// `[(Self, char, Self); N]` array on the closed-set [`QuoteForm`]
    /// algebra rather than at zero-primitive-plus-inline-arm-literals
    /// inside [`Self::promote_via_next_char`]'s match body.
    ///
    /// The substrate's current promotion algebra is the singleton
    /// `[(Self::Unquote, Self::SPLICE_DISCRIMINATOR, Self::UnquoteSplice)]`
    /// — the ONLY (variant, next-char) → longer-variant mapping the
    /// reader's peek-then-consume `@` promotion arm depends on. Its
    /// forced-arity `1` is INTENTIONAL and load-bearing: [`Self::UnquoteSplice`]
    /// is the ONLY variant with a two-char [`Self::prefix`], so the
    /// promotion table has exactly ONE `Some` arm and every other
    /// pairing rejects — the closed set of promotions is the singleton
    /// `{(Unquote, '@') → UnquoteSplice}` on the `Self × char →
    /// Option<Self>` product. Pinned bit-for-bit by
    /// `quote_form_promotions_has_expected_cardinality` (forced-arity)
    /// AND `quote_form_promotions_pin_legacy_splice_promotion_triple`
    /// (identity of the singleton entry). A future fifth homoiconic
    /// prefix with its own two-char extension (a hypothetical `,~`
    /// reverse-unquote via a `~` discriminator, a `#'` function-quote
    /// via a `'` discriminator, a `,?` conditional-unquote via a `?`
    /// discriminator) extends [`Self::ALL`] AND appends ONE new
    /// promotion triple to [`Self::PROMOTIONS`] AND extends
    /// [`Self::promote_via_next_char`]'s match body in lockstep —
    /// rustc's forced-arity check on the `[(Self, char, Self); N]`
    /// array fails compilation if the array's cardinality grows
    /// without a matching arm on the projection method.
    ///
    /// Sibling posture to the closed-set forced-arity ALL arrays across
    /// the substrate's [`QuoteForm`] algebra — [`Self::ALL`]
    /// (`[Self; 4]` — the closed set of variants),
    /// [`Self::PREFIXES`] (`[&'static str; 4]` — the reader-prefix
    /// `&'static str` axis), [`Self::LABELS`] (`[&'static str; 4]` —
    /// the diagnostic-label `&'static str` axis),
    /// [`Self::IAC_FORGE_TAGS`] (`[&'static str; 4]` — the iac-forge
    /// canonical-form tag `&'static str` axis),
    /// [`Self::LEADS`] (`[char; 3]` — the reader-lead `char` axis with
    /// shape-asymmetric cardinality reflecting the shared-lead-byte
    /// collapse of the `,` prefix across [`Self::Unquote`] AND
    /// [`Self::UnquoteSplice`]), and [`Self::HASH_DISCRIMINATORS`]
    /// (`[u8; 4]` — the outer-Sexp cache-key byte axis). This lift adds
    /// the SEVENTH per-family axis on the algebra — the (head, disc,
    /// promoted) triple axis on the closed-set promotion product.
    /// Each of the seven axes now pins its per-role canonical data at
    /// ONE `pub const` per role PLUS an ALL array for family-wide
    /// consumers, across the same closed set of four variants (or
    /// three-of-four for the shape-asymmetric [`Self::LEADS`] axis's
    /// shared-lead-char collapse, or one-of-four for the promotion-
    /// asymmetric [`Self::PROMOTIONS`] axis's single-arm collapse).
    ///
    /// Composition identity (pinned by
    /// `quote_form_promotions_align_with_promote_via_next_char_for_every_entry`):
    /// for every `(head, disc, promoted)` in [`Self::PROMOTIONS`],
    /// `head.promote_via_next_char(disc) == Some(promoted)`. The
    /// projection's Some-arm binds through [`Self::PROMOTIONS`]`[i].2`
    /// (the promoted-variant column of the ONE promotion triple) — a
    /// regression that drifts the triple's promoted-variant column
    /// silently redirects every reader `,@` sequence to a phantom
    /// variant AND fails the alignment pin at rustc / test time
    /// rather than at silent tokenizer drift where every `,@xs` form
    /// tokenizes to the wrong closed-set marker.
    ///
    /// Rejection contract (pinned by
    /// `quote_form_promotions_close_promote_via_next_char_against_every_non_promotion_pair`):
    /// for every `(head, next)` pair NOT in [`Self::PROMOTIONS`]'s
    /// projection to `(Self × char)`, `head.promote_via_next_char(next)
    /// == None`. Sweeps the [`Self::ALL`] × (rejection-char sweep)
    /// product against the promotion set's complement — a regression
    /// that widened the promotion algebra (e.g. phantom-promoted
    /// [`Self::Quote`] on `'@'` after a copy-paste drift on the match
    /// arm) surfaces at test time rather than at silent tokenizer
    /// drift where bare `'@xs` forms silently degrade to a phantom
    /// [`Self::UnquoteSplice`]-shaped sequence.
    ///
    /// Composition law (rendered-prefix identity, pinned by
    /// `quote_form_promotions_compose_prefix_from_source_prefix_and_discriminator_for_every_entry`):
    /// for every `(head, disc, promoted)` in [`Self::PROMOTIONS`],
    /// `format!("{}{}", head.prefix(), disc) == promoted.prefix()`.
    /// The (head prefix + discriminator) source-text composition
    /// agrees byte-for-byte with the promoted variant's rendered
    /// prefix — the reader's peek-then-consume arm's rendered
    /// prefix identity closes the read↔write duality across the
    /// promotion algebra. Sibling to the pre-existing
    /// `quote_form_promote_via_next_char_composes_prefix_from_source_prefix_and_next_char`
    /// which pins the same law through the projection method rather
    /// than through the triple's data directly — this pin closes the
    /// law at the constant, that pin closes it at the projection.
    ///
    /// `pub(crate)` because the promotion algebra is an implementation
    /// detail of the substrate's reader — exposing it publicly would
    /// leak the promotion-table shape through the API without enabling
    /// any external consumer the public projections
    /// ([`Self::promote_via_next_char`], [`Self::prefix`],
    /// [`Self::from_lead_char`]) don't already serve — same visibility
    /// rationale as [`Self::HASH_DISCRIMINATORS`] on the sibling
    /// cache-key axis.
    ///
    /// The `#[allow(dead_code)]` posture matches
    /// [`Self::HASH_DISCRIMINATORS`] / [`AtomKind::HASH_DISCRIMINATORS`]:
    /// the substrate's current [`Self::promote_via_next_char`] body
    /// dispatches through ONE match arm bound to the ONE promotion
    /// triple's promoted-variant column ([`Self::PROMOTIONS`]`[0].2`),
    /// with the head-pattern + discriminator-pattern arm literals
    /// preserved for the const-fn match's pattern surface (patterns
    /// cannot be array-indexing expressions in the current const-fn
    /// grammar). The lift lands the substrate primitive so future
    /// consumers keyed on the whole promotion algebra (a future
    /// `tatara-check` predicate `(check-promotion-algebra-injective …)`
    /// that verifies each `(head, disc)` pair projects to a unique
    /// promoted variant, a future LSP structural-navigation filter
    /// that keys on the promotion algebra's cardinality, a future
    /// `TypedRewriter<PromotionOp>` sweep zipping ALL / PREFIXES /
    /// LABELS / IAC_FORGE_TAGS / HASH_DISCRIMINATORS / PROMOTIONS in
    /// lockstep for a family-wide render) bind to ONE `[(Self, char,
    /// Self); N]` primitive rather than re-deriving the promotion
    /// triples inline per callsite. Matches the preemptive-primitive
    /// posture the prior-run [`Self::HASH_DISCRIMINATORS`] +
    /// [`AtomKind::HASH_DISCRIMINATORS`] +
    /// [`crate::error::StructuralKind::HASH_DISCRIMINATORS`] lifts
    /// carried before their downstream consumers materialized.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 1 — typed entry; the
    /// reader's two-char quote-family classification IS the typed-
    /// entry gate on the `,@` boundary, and lifting the promotion
    /// algebra's canonical triples to ONE typed `[(Self, char, Self);
    /// N]` primitive on the closed-set algebra closes the gate's
    /// two-char entry surface onto the algebra rather than at inline
    /// arm literals scattered across the const-fn match body.
    /// THEORY.md §III — the typescape; the singleton promotion triple
    /// binds at ONE typed `pub const` on the closed-set [`QuoteForm`]
    /// algebra rather than at zero-primitive-plus-inline-arm-literals
    /// at [`Self::promote_via_next_char`]'s match arm. THEORY.md §V.1
    /// — knowable platform; the family's cardinality becomes a
    /// TYPE-level constant on the substrate algebra rather than a
    /// per-consumer runtime dispatch through the match table.
    /// THEORY.md §VI.1 — generation over composition; the family-wide
    /// contract sweeps (alignment with
    /// [`Self::promote_via_next_char`], pairwise disjointness across
    /// the head-discriminator product, rendered-prefix composition
    /// identity) emerge from the composition of ONE substrate
    /// primitive (this `pub(crate) const` array) rather than as
    /// per-arm inline assertions duplicated at each call site.
    ///
    /// Frontier inspiration: MLIR's typed rewriter registry
    /// (`mlir::PatternApplicator`) carries a per-op-family
    /// `[(source_pattern, matcher, rewritten_op)]` static rewrite
    /// table at the closed-set boundary — the (source, matcher,
    /// target) triple axis becomes typed data on the IR algebra
    /// rather than dispatch tables scattered across per-pattern
    /// callsites. Translated through the substrate's [`QuoteForm`]
    /// closed-set marker, the reader's promotion rewrite table
    /// becomes ONE typed `[(head_variant, next_char, promoted_variant);
    /// N]` array on the algebra. Where MLIR's registry carries the
    /// rewrite table dynamically on the pattern applicator's runtime
    /// state, this substrate carries it statically as `pub const` on
    /// the closed-set marker — the pattern-matching evaluation lands
    /// at rustc-time through const-fn match arm binding to
    /// [`Self::PROMOTIONS`]`[i].2` rather than at runtime through a
    /// dynamic registry lookup.
    #[allow(dead_code)]
    pub(crate) const PROMOTIONS: [(Self, char, Self); 1] = [(
        Self::Unquote,
        Self::SPLICE_DISCRIMINATOR,
        Self::UnquoteSplice,
    )];

    /// Promotion table on the closed-set quote-family algebra —
    /// `Some(Self::UnquoteSplice)` iff `self == Self::Unquote &&
    /// next == Self::SPLICE_DISCRIMINATOR`, else `None`. Encodes the
    /// substrate's ONE (variant, next-char) → longer-variant mapping —
    /// `,` [`Self::Unquote`] followed by `@` [`Self::SPLICE_DISCRIMINATOR`]
    /// promotes to `,@` [`Self::UnquoteSplice`]. Every other pairing
    /// (including [`Self::Quote`] / [`Self::Quasiquote`] / [`Self::UnquoteSplice`]
    /// on ANY next-char, AND [`Self::Unquote`] on any non-discriminator
    /// next-char) yields `None` — the closed set of promotions is the
    /// singleton `{(Unquote, '@') → UnquoteSplice}` on the `Self × char
    /// → Option<Self>` product.
    ///
    /// Structural sibling of [`Self::from_lead_char`] one axis over on
    /// the same typed-entry family: [`Self::from_lead_char`] decodes
    /// ONE lead char to its DEFAULT variant on that char; this method
    /// decodes ONE (default variant, second char) pair to its PROMOTED
    /// variant. Together the two methods close the reader's outer
    /// quote-family entry surface onto the algebra: the tokenizer
    /// consumes ONE lead char through [`Self::from_lead_char`], then
    /// OPTIONALLY consumes one second char through this method — the
    /// (lead char, second char) → typed marker projection binds at TWO
    /// typed decodes rather than at inline `char`-literal patterns
    /// scattered across the outer-match dispatch arm.
    ///
    /// ONE consumer entrypoint the reader's `tokenize` binds against:
    /// the peek-then-consume `@` promotion inside the outer-match
    /// quote-family dispatch was pre-lift a hand-rolled inline check
    /// `matches!(qf_head, QuoteForm::Unquote) &&
    /// chars.peek().map(|&(_, c)| c) == Some('@')` paired with a
    /// per-branch `QuoteForm::UnquoteSplice` construction. The pairing
    /// was load-bearing yet only enforced by callsite discipline at a
    /// SEVENTH consumer site (alongside `Hash`, `Display`, `sexp_shape`,
    /// `wrap`, `iac_forge_tag`, `as_unquote_form`) the prior closed-set
    /// `QuoteForm` lifts did not reach. Post-lift the reader's peek
    /// arm routes through this method, so the (Unquote, '@') →
    /// UnquoteSplice promotion binds at ONE site on the typed algebra.
    /// A regression that drifts the promotion table (e.g. re-inlines
    /// `matches!(qf_head, QuoteForm::Quote)` at the peek arm and
    /// silently promotes bare `'` to a phantom variant) becomes a
    /// typed compile error against the `Option<Self>` return type.
    ///
    /// The single-promotion collapse (only `(Unquote, '@')` triggers)
    /// is INTENTIONAL: [`Self::UnquoteSplice`] is the ONLY variant with
    /// a two-char [`Self::prefix`], so the promotion table has exactly
    /// ONE `Some` arm and every other pairing rejects. Placing the
    /// promotion at the closed-set algebra rather than at the reader's
    /// peek arm keeps the streaming reader's two-char peek-then-consume
    /// shape at ONE site (the reader) while the (variant × second
    /// char) → promoted variant projection lives on the substrate
    /// algebra — parallel to the split that [`Self::from_lead_char`]
    /// closes for the one-char entry surface. This split parallels the
    /// reader's split of `Token::Str` into open-delimiter dispatch
    /// ([`crate::ast::Atom::STR_DELIMITER`]) AND inner-payload
    /// accumulation — the closed-set char algebra decodes the entry
    /// chars; the streaming reader handles the peek-and-consume
    /// follow-through.
    ///
    /// Composition identity: for every `qf: QuoteForm` and every
    /// `c: char`, if `qf.promote_via_next_char(c) == Some(promoted)`
    /// then `format!("{}{}", qf.prefix(), c) == promoted.prefix()`.
    /// Pinned by
    /// `quote_form_promote_via_next_char_composes_prefix_from_source_prefix_and_next_char`
    /// across the singleton promotion arm — the pin asserts the
    /// (variant, next char) → promoted-variant projection agrees with
    /// the reader's rendered [`Self::prefix`] composition, so a
    /// regression that drifts one side of the identity (a promotion
    /// arm rerouted through the wrong variant, or a prefix renamed
    /// without updating the promotion table) surfaces immediately.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 1 — typed entry; the
    /// reader's two-char quote-family classification IS the typed-
    /// entry gate on the `,@` boundary. THEORY.md §V.1 — knowable
    /// platform; the (variant, next char) → promoted variant table
    /// becomes a TYPE projection on the substrate algebra rather than
    /// at an inline `matches!(qf, Unquote) && c == '@'` pattern
    /// scattered at the reader's peek arm. THEORY.md §VI.1 —
    /// generation over composition; a fifth homoiconic prefix with its
    /// own two-char extension extends [`Self`] AND this method's match
    /// arm in lockstep — rustc binds the extension through
    /// exhaustiveness over the closed enum, and the `Option<Self>`
    /// return shape leaves the promotion table structurally open for
    /// future variants to append their own `Some` arms without
    /// touching the existing arms' semantics.
    ///
    /// Frontier inspiration: Racket's `read-syntax` two-char
    /// discriminator table (`(quote-abbrev-mapping char) → syntax`)
    /// that maps `(#\' → 'quote)`, `(#\` → 'quasiquote)`, `(#\, →
    /// 'unquote)`, `(#\, #\@) → 'unquote-splicing)` on the reader's
    /// typed abbreviation surface. Translated through the substrate's
    /// [`QuoteForm`] outer-marker algebra, the `(#\, #\@) → 'unquote-
    /// splicing)` two-char arm becomes ONE typed `(Self::Unquote, '@')
    /// → Some(Self::UnquoteSplice)` promotion on the closed-set
    /// algebra with rustc binding the promotion identity through
    /// exhaustiveness. Where Racket carries the promotion table
    /// dynamically on the reader's abbreviation-mapping parameter,
    /// this substrate carries it statically as `pub const fn` on the
    /// closed-set marker.
    #[must_use]
    pub const fn promote_via_next_char(self, next: char) -> Option<Self> {
        // The Some-arm's promoted-variant column routes through
        // `Self::PROMOTIONS[0].2` — the ONE promotion triple on the
        // closed-set algebra — so a regression that drifts the
        // promoted-variant column of the singleton triple silently
        // redirects every reader `,@` sequence to a phantom variant AND
        // fails the alignment pin
        // `quote_form_promotions_align_with_promote_via_next_char_for_every_entry`
        // at rustc / test time rather than at silent tokenizer drift.
        // The head-pattern (`Self::Unquote`) + discriminator-pattern
        // (`Self::SPLICE_DISCRIMINATOR`) arm literals stay inline
        // because patterns cannot be array-indexing expressions in the
        // current const-fn grammar; the alignment pin catches head /
        // discriminator column drift by construction.
        match (self, next) {
            (Self::Unquote, Self::SPLICE_DISCRIMINATOR) => Some(Self::PROMOTIONS[0].2),
            _ => None,
        }
    }

    /// Canonical `u8` cache-key byte for [`Self::Quote`]'s
    /// [`Self::hash_discriminator`] arm — `3`. ONE canonical byte on
    /// the closed-set [`QuoteForm`] algebra shared by
    /// [`Self::hash_discriminator`]'s [`Self::Quote`] arm AND every
    /// downstream consumer (the [`Hash for Sexp`](crate::ast::Sexp)
    /// cache-key body, the expansion cache
    /// (`crate::macro_expand::Expander::cache`) that keys on that hash).
    ///
    /// Sibling posture to the closed set of per-role `pub(crate) const`
    /// / `pub const` bytes on the substrate's other closed-set outer
    /// algebras: [`Self::QUOTE_PREFIX`] / [`Self::QUASIQUOTE_PREFIX`]
    /// / [`Self::UNQUOTE_PREFIX`] / [`Self::UNQUOTE_SPLICE_PREFIX`]
    /// (per-role reader-prefix `&'static str` algebra on the SAME
    /// [`QuoteForm`] closed set — commit a08e61f),
    /// [`Self::QUOTE_LABEL`] / [`Self::QUASIQUOTE_LABEL`] /
    /// [`Self::UNQUOTE_LABEL`] / [`Self::UNQUOTE_SPLICE_LABEL`] (per-
    /// role diagnostic-label `&'static str` algebra on the SAME closed
    /// set — commit 70be157), [`Self::QUOTE_IAC_FORGE_TAG`] /
    /// [`Self::QUASIQUOTE_IAC_FORGE_TAG`] / [`Self::UNQUOTE_IAC_FORGE_TAG`]
    /// / [`Self::UNQUOTE_SPLICE_IAC_FORGE_TAG`] (per-role iac-forge
    /// canonical-form tag `&'static str` algebra on the SAME closed set
    /// — commit bdd624b). This constant closes the FOURTH per-role
    /// axis on [`QuoteForm`] — the `u8` cache-key axis paired with the
    /// three pre-existing `&'static str` axes.
    ///
    /// The FOUR canonical bytes `{3, 4, 5, 6}` partition the outer-
    /// [`crate::ast::Sexp`] `Hash` body's quote-family arm-set against
    /// the reserved bytes the non-quote-family arms use (`0u8` for
    /// [`crate::error::StructuralKind::Nil`] via
    /// [`crate::error::StructuralKind::hash_discriminator`], `1u8` for
    /// [`crate::ast::Sexp::Atom`]'s outer-carve marker via the
    /// pre-existing inline `1u8` at [`Hash for Sexp`](crate::ast::Sexp)'s
    /// atom arm, `2u8` for [`crate::error::StructuralKind::List`] via
    /// [`crate::error::StructuralKind::hash_discriminator`]) — the
    /// three carvings of the outer-[`crate::ast::Sexp`] cache-key
    /// space jointly cover the `{0, 1, 2, 3, 4, 5, 6}` byte-set with
    /// no gaps AND no overlaps.
    ///
    /// A regression that inlines the `3` literal at
    /// [`Self::hash_discriminator`]'s [`Self::Quote`] arm and drifts
    /// the constant silently (e.g. a re-numbering that collides with
    /// the reserved `2u8` for [`crate::error::StructuralKind::List`],
    /// silently mis-hashing every cached expansion across the substrate)
    /// fails at the algebra's `hash_discriminator()` path-uniformity
    /// pin
    /// (`quote_form_hash_discriminator_routes_through_typed_per_role_constants`)
    /// rather than at silent cache-key drift where
    /// `crate::macro_expand::Expander::cache` mis-collides live
    /// expansions.
    ///
    /// `pub(crate)` because the byte-discriminator surface is an
    /// implementation detail of the substrate's [`Hash for Sexp`](crate::ast::Sexp)
    /// cache-key contract; exposing it publicly would leak the cache-
    /// key shape through the API without enabling any external
    /// consumer the public projections ([`Self::as_quote_form`],
    /// [`Self::prefix`], [`Self::as_unquote_form`]) don't already
    /// serve — same visibility rationale as [`Self::hash_discriminator`]
    /// itself.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 5 — composition
    /// preserves proofs; the alias-chain composition law
    /// `QuoteForm::HASH_DISCRIMINATORS[i] ==
    /// QuoteForm::ALL[i].hash_discriminator()` binds the family-wide
    /// array to the projection method at rustc time, pinned by byte
    /// equality. THEORY.md §III — the typescape; the four canonical
    /// cache-key bytes bind at ONE `pub(crate) const` per role on the
    /// typed algebra rather than as inline `u8` literals in the
    /// `hash_discriminator` match arms.
    pub(crate) const QUOTE_HASH_DISCRIMINATOR: u8 = 3;

    /// Canonical `u8` cache-key byte for [`Self::Quasiquote`]'s
    /// [`Self::hash_discriminator`] arm — `4`. Sibling of
    /// [`Self::QUOTE_HASH_DISCRIMINATOR`] on the closed-set per-role
    /// quote-family cache-key-byte axis; see
    /// [`Self::QUOTE_HASH_DISCRIMINATOR`] for the algebra-level round-
    /// trip + disjointness contracts every sibling shares.
    pub(crate) const QUASIQUOTE_HASH_DISCRIMINATOR: u8 = 4;

    /// Canonical `u8` cache-key byte for [`Self::Unquote`]'s
    /// [`Self::hash_discriminator`] arm — `5`. Sibling of
    /// [`Self::QUOTE_HASH_DISCRIMINATOR`] on the closed-set per-role
    /// quote-family cache-key-byte axis. Byte-for-byte distinct from
    /// [`Self::UNQUOTE_SPLICE_HASH_DISCRIMINATOR`] — the two template-
    /// substitution arms partition `{5, 6}` on the outer-Sexp cache-
    /// key space.
    pub(crate) const UNQUOTE_HASH_DISCRIMINATOR: u8 = 5;

    /// Canonical `u8` cache-key byte for [`Self::UnquoteSplice`]'s
    /// [`Self::hash_discriminator`] arm — `6`. Sibling of
    /// [`Self::QUOTE_HASH_DISCRIMINATOR`] on the closed-set per-role
    /// quote-family cache-key-byte axis. The HIGHEST byte on the
    /// closed set — a future fifth quote-family variant would extend
    /// the partition to `{3, 4, 5, 6, 7}` and land the new
    /// discriminator at `7u8`.
    pub(crate) const UNQUOTE_SPLICE_HASH_DISCRIMINATOR: u8 = 6;

    /// Closed-set forced-arity ALL array over the canonical cache-key
    /// `u8` bytes, in declaration order matching [`Self::ALL`] element-
    /// wise (pinned by `quote_form_hash_discriminators_align_with_all_by_index`).
    /// Sibling posture to [`Self::PREFIXES`] (`[&'static str; 4]` —
    /// the reader-prefix `&'static str` axis on the SAME closed set),
    /// [`Self::LABELS`] (`[&'static str; 4]` — the diagnostic-label
    /// `&'static str` axis), [`Self::IAC_FORGE_TAGS`] (`[&'static str;
    /// 4]` — the iac-forge canonical-form tag `&'static str` axis) —
    /// every closed-set outer projection on the substrate's
    /// [`QuoteForm`] algebra now pins its per-role canonical bytes at
    /// ONE `pub(crate) const` / `pub const` per role PLUS an ALL array
    /// for family-wide consumers, across ALL FOUR production
    /// vocabularies the closed set carries (reader prefix, diagnostic
    /// label, iac-forge canonical-form tag, outer-Sexp cache-key byte).
    ///
    /// Pre-lift the four cache-key bytes had NO per-role primitive on
    /// this closed-set algebra — a consumer with a [`QuoteForm`]
    /// variant in hand at compile time reaching for the canonical byte
    /// had to spell `QuoteForm::Unquote.hash_discriminator()` (runtime
    /// dispatch through the match arm) OR reach across into the inline
    /// `5u8` at the pre-lift match arm's [`Self::Unquote`] branch and
    /// re-derive the (variant, byte) pairing at the call site.
    /// Post-lift the FOUR canonical bytes bind at ONE `pub(crate) const`
    /// per role on the typed [`QuoteForm`] algebra AND at
    /// [`Self::HASH_DISCRIMINATORS`] as a family-wide forced-arity
    /// array — a future substrate-facing cache-key introspection tool
    /// (a `tatara-check` predicate that asserts every quote-family
    /// arm's discriminator disjoint from the reserved
    /// [`crate::ast::Sexp::Atom`] byte, a Sekiban audit-trail metric
    /// jointly labeled by the cache-key partition, a future
    /// `TypedRewriter<QuoteFormOp>` sweep zipping ALL / PREFIXES /
    /// LABELS / IAC_FORGE_TAGS / HASH_DISCRIMINATORS in lockstep for a
    /// family-wide (variant, four-vocabulary quadruple) render) reads
    /// through the typed constants without re-deriving the four-arm
    /// carving inline.
    ///
    /// Each entry is byte-for-byte identical to the pre-lift inline
    /// `u8` literal at the corresponding [`Self::hash_discriminator`]
    /// arm — pinned by
    /// `quote_form_hash_discriminators_pin_legacy_cache_key_bytes` so
    /// a regression that drifts ONE `pub(crate) const` from its pre-
    /// lift byte silently invalidates every cached expansion AND mis-
    /// collides with the reserved bytes the non-quote-family arms use,
    /// fails-loudly at the alias test rather than at a silent
    /// [`crate::macro_expand::Expander::cache`] mis-hash. Adding a
    /// hypothetical fifth homoiconic prefix (a `,~` reverse-unquote, a
    /// `,?` conditional-unquote) extends [`Self::ALL`] AND
    /// [`Self::HASH_DISCRIMINATORS`] AND adds ONE per-role
    /// `pub(crate) const` in lockstep — rustc's forced-arity check on
    /// the two `[_; N]` arrays fails compilation if EITHER array grows
    /// without the other, closing the extensibility gap that pre-lift
    /// silently allowed a discriminator collision on `7u8` (the next
    /// free byte).
    ///
    /// Theory anchor: THEORY.md §III — the typescape; the four
    /// canonical cache-key bytes bind at ONE typed `[u8; 4]` array on
    /// the closed-set [`QuoteForm`] algebra rather than at zero-
    /// primitive-plus-four-inline-`u8`-literals scattered across the
    /// [`Self::hash_discriminator`] match arms. THEORY.md §V.1 —
    /// knowable platform; the family's cardinality becomes a TYPE-
    /// level constant on the substrate algebra rather than a per-
    /// consumer runtime dispatch through the match table. THEORY.md
    /// §V.3 — three-pillar attestation; the cache-key partition is
    /// the substrate's outer-Sexp `intent_hash` composition axis for
    /// every quote-family arm — binding the four bytes on the typed
    /// algebra makes attestation-key drift a compile error rather
    /// than a silent BLAKE3 mis-hash. THEORY.md §VI.1 — generation
    /// over composition; the family-wide contract sweeps (alignment
    /// with `ALL`, pairwise disjointness, membership through
    /// [`Self::hash_discriminator`]) emerge from the composition of
    /// TWO substrate primitives (this `pub(crate) const` array + the
    /// four per-role `pub(crate) const *_HASH_DISCRIMINATOR` aliases)
    /// rather than as per-variant inline assertions duplicated at
    /// each call site.
    ///
    /// The `#[allow(dead_code)]` posture is deliberate: the substrate's
    /// current [`Hash for Sexp`](crate::ast::Sexp) body composes
    /// through the per-variant [`Self::hash_discriminator`] projection
    /// arm-by-arm rather than sweeping the family-wide array, so no
    /// non-test caller currently reaches this ALL array directly. The
    /// lift lands the substrate primitive so future consumers keyed
    /// on the whole family (a future
    /// [`crate::macro_expand::Expander`] cache-warmup pass that hashes
    /// the quote-family byte-set upfront, a future `tatara-check`
    /// predicate `(check-cache-key-partition-disjoint …)` that
    /// verifies the `{3, 4, 5, 6}` partition against the reserved
    /// `{0, 1, 2}` bytes structurally, a future
    /// `TypedRewriter<QuoteFormOp>` sweep zipping ALL / PREFIXES /
    /// LABELS / IAC_FORGE_TAGS / HASH_DISCRIMINATORS in lockstep for a
    /// family-wide (variant, four-vocabulary quadruple) render) bind
    /// to ONE `[u8; 4]` primitive rather than re-deriving the array
    /// inline per callsite. Matches the preemptive-primitive posture
    /// the prior-run [`crate::error::UnquoteForm::hash_discriminator`]
    /// lift carried before its downstream consumers materialized.
    #[allow(dead_code)]
    pub(crate) const HASH_DISCRIMINATORS: [u8; 4] = [
        Self::QUOTE_HASH_DISCRIMINATOR,
        Self::QUASIQUOTE_HASH_DISCRIMINATOR,
        Self::UNQUOTE_HASH_DISCRIMINATOR,
        Self::UNQUOTE_SPLICE_HASH_DISCRIMINATOR,
    ];

    /// Stable, per-variant byte discriminator that paired with the
    /// recursive inner hash builds the substrate's `Hash for Sexp`
    /// projection — `3` for [`Self::Quote`], `4` for
    /// [`Self::Quasiquote`], `5` for [`Self::Unquote`], `6` for
    /// [`Self::UnquoteSplice`]. The byte values are load-bearing
    /// because the expansion cache (`Expander::cache`) keys on the
    /// hash of `(macro_name, args)` — changing a discriminator silently
    /// invalidates every cached expansion AND mis-collides with the
    /// reserved bytes the non-quote-family Hash arms use (`0` for
    /// `Nil`, `1` for `Atom`, `2` for `List`). The closed set ensures
    /// the four arms partition `{3, 4, 5, 6}` injectively against the
    /// reserved bytes — a future quote-family extension must extend
    /// this method AND the non-quote-family arms in lockstep, with
    /// rustc binding the consistency through exhaustiveness over the
    /// closed enum.
    ///
    /// Post-lift the four arms route through the per-role
    /// `pub(crate) const` bytes on the closed-set [`QuoteForm`]
    /// algebra ([`Self::QUOTE_HASH_DISCRIMINATOR`],
    /// [`Self::QUASIQUOTE_HASH_DISCRIMINATOR`],
    /// [`Self::UNQUOTE_HASH_DISCRIMINATOR`],
    /// [`Self::UNQUOTE_SPLICE_HASH_DISCRIMINATOR`]) rather than
    /// inline `u8` literals — so a re-numbering that would silently
    /// invalidate every cached expansion lands as ONE edit to the
    /// matching `pub(crate) const` rather than at four scattered
    /// arm-literals. Every downstream consumer that binds to the
    /// algebra ([`Hash for Sexp`](crate::ast::Sexp)'s outer sweep,
    /// the [`crate::macro_expand::Expander::cache`] cache-key
    /// composition, the future coverage-tool sweeps) inherits the
    /// rename mechanically.
    ///
    /// `pub(crate)` because the byte-discriminator surface is an
    /// implementation detail of the substrate's `Hash for Sexp` cache-
    /// key contract; exposing it publicly would leak the cache-key
    /// shape through the API without enabling any external consumer
    /// the public projections (`Sexp::as_quote_form`, `Self::prefix`,
    /// `Self::as_unquote_form`) don't already serve.
    #[must_use]
    pub(crate) fn hash_discriminator(self) -> u8 {
        match self {
            Self::Quote => Self::QUOTE_HASH_DISCRIMINATOR,
            Self::Quasiquote => Self::QUASIQUOTE_HASH_DISCRIMINATOR,
            Self::Unquote => Self::UNQUOTE_HASH_DISCRIMINATOR,
            Self::UnquoteSplice => Self::UNQUOTE_SPLICE_HASH_DISCRIMINATOR,
        }
    }

    /// Project the 4-of-4 quote-family marker into the 2-of-4
    /// template-substitution subset — `Some(UnquoteForm::Unquote)` for
    /// [`Self::Unquote`], `Some(UnquoteForm::Splice)` for
    /// [`Self::UnquoteSplice`], `None` for [`Self::Quote`] /
    /// [`Self::Quasiquote`] (the literal-quote and quasi-quote
    /// prefixes are wrappers, NOT substitution points). ONE projection
    /// on this algebra the [`crate::ast::Sexp::as_unquote`] derivation
    /// routes through — the (Sexp variant, UnquoteForm marker) pairing
    /// now binds at the typed [`crate::ast::Sexp::as_quote_form`]
    /// projection's output composed with this method's output, instead
    /// of being re-derived per-arm inside `Sexp::as_unquote`.
    ///
    /// The closed-set guarantee on [`UnquoteForm`] (exactly
    /// `Unquote ⊎ Splice`) AND on [`Self`] (exactly
    /// `Quote ⊎ Quasiquote ⊎ Unquote ⊎ UnquoteSplice`) ensures that the
    /// 2-of-4 subset is structurally fixed: a future variant joining
    /// the template-substitution surface extends both enums AND this
    /// method's match arm together, with rustc binding the extension
    /// through the projection's `Option` return type.
    #[must_use]
    pub fn as_unquote_form(self) -> Option<UnquoteForm> {
        match self {
            Self::Unquote => Some(UnquoteForm::Unquote),
            Self::UnquoteSplice => Some(UnquoteForm::Splice),
            Self::Quote | Self::Quasiquote => None,
        }
    }

    /// Canonical `&'static str` iac-forge canonical-form tag of
    /// [`Self::Quote`] — `"quote"`. The ONE canonical bytes-payload on
    /// the closed-set [`QuoteForm`] algebra shared by [`Self::iac_forge_tag`]'s
    /// [`Self::Quote`] arm AND the [`crate::interop`] `From<&Sexp> for
    /// iac_forge::sexpr::SExpr` arm the projection feeds.
    ///
    /// Sibling posture to the closed set of per-role `pub const` bytes
    /// on the substrate's other closed-set outer algebras:
    /// [`Self::QUOTE_PREFIX`] / [`Self::QUASIQUOTE_PREFIX`] /
    /// [`Self::UNQUOTE_PREFIX`] / [`Self::UNQUOTE_SPLICE_PREFIX`] (per-
    /// role reader-prefix algebra on the SAME [`QuoteForm`] closed set),
    /// [`crate::error::MacroDefHead::DEFMACRO_KEYWORD`] /
    /// [`crate::error::MacroDefHead::DEFPOINT_TEMPLATE_KEYWORD`] /
    /// [`crate::error::MacroDefHead::DEFCHECK_KEYWORD`] (per-role
    /// head-keyword algebra on the CL macro-definition surface),
    /// [`Atom::TRUE_LITERAL`] / [`Atom::FALSE_LITERAL`] (per-role
    /// Scheme-bool spelling algebra on the atomic-payload surface).
    ///
    /// The (canonical iac-forge tag) axis lives ORTHOGONAL to the
    /// (canonical reader prefix) axis: `Self::QUOTE_PREFIX` (`"'"`) and
    /// `Self::QUOTE_IAC_FORGE_TAG` (`"quote"`) both project the same
    /// variant but through two distinct byte vocabularies — the reader
    /// axis for the Lisp source-code surface, the iac-forge axis for
    /// the cross-crate canonical-form surface (BLAKE3 attestation,
    /// render cache). A regression that inlines the `"quote"` literal
    /// at [`Self::iac_forge_tag`]'s [`Self::Quote`] arm and drifts the
    /// constant silently (e.g. a hypothetical rename to `"literal-quote"`
    /// on the iac-forge side while leaving the prefix `"'"` intact)
    /// fails at the algebra's `iac_forge_tag()` path-uniformity pin
    /// (`quote_form_iac_forge_tag_routes_through_typed_per_role_constants`)
    /// rather than at silent canonical-form drift where downstream
    /// BLAKE3 attestation keys silently mis-hash.
    pub const QUOTE_IAC_FORGE_TAG: &'static str = "quote";

    /// Canonical `&'static str` iac-forge canonical-form tag of
    /// [`Self::Quasiquote`] — `"quasiquote"`. Sibling of
    /// [`Self::QUOTE_IAC_FORGE_TAG`] on the closed-set per-role
    /// quote-family iac-forge tag-bytes axis; see
    /// [`Self::QUOTE_IAC_FORGE_TAG`] for the algebra-level round-trip +
    /// disjointness contracts every sibling shares.
    pub const QUASIQUOTE_IAC_FORGE_TAG: &'static str = "quasiquote";

    /// Canonical `&'static str` iac-forge canonical-form tag of
    /// [`Self::Unquote`] — `"unquote"`. Sibling of
    /// [`Self::QUOTE_IAC_FORGE_TAG`] on the closed-set per-role
    /// quote-family iac-forge tag-bytes axis.
    ///
    /// Byte-identical to the substrate's shorter diagnostic label
    /// [`crate::error::SexpShape::Unquote`]'s label projection
    /// (`SexpShape::label` returns `"unquote"` for this variant) — the
    /// two projections happen to agree on this variant's bytes but
    /// live at distinct algebraic layers (iac-forge canonical form vs
    /// substrate diagnostic surface); the divergence is load-bearing on
    /// the [`Self::UnquoteSplice`] arm (`"unquote-splicing"` vs
    /// `"unquote-splice"`) and this byte-level agreement here does not
    /// license a consolidation of the two axes. Pinned by
    /// `quote_form_iac_forge_tag_diverges_from_sexp_shape_label_for_unquote_splice`.
    pub const UNQUOTE_IAC_FORGE_TAG: &'static str = "unquote";

    /// Canonical `&'static str` iac-forge canonical-form tag of
    /// [`Self::UnquoteSplice`] — `"unquote-splicing"`. The Common-Lisp-
    /// canonical spelling: a `,@x` form encodes as `(unquote-splicing x)`
    /// rather than `(unquote-splice x)`. That tag-string choice is
    /// INTENTIONALLY DISTINCT from the substrate's shorter diagnostic
    /// label projected by [`crate::error::SexpShape::label`] (which
    /// renders `[`Self::UnquoteSplice`]` as `"unquote-splice"` — the
    /// shorter idiom appropriate for `expected …, got unquote-splice`
    /// error surfaces). The two projections key the SAME closed set on
    /// TWO distinct boundaries — pinning the divergence at the typed
    /// per-role `pub const` documents the intent structurally: a
    /// future "consolidation" PR that homogenizes them would have to
    /// touch this constant explicitly, surfacing the boundary-distinct
    /// invariant at code-review time rather than silently.
    ///
    /// Sibling of [`Self::QUOTE_IAC_FORGE_TAG`] on the closed-set
    /// per-role quote-family iac-forge tag-bytes axis. The ONLY entry
    /// on this axis whose bytes disagree with the peer-axis
    /// [`crate::error::SexpShape::label`] projection — pinned by
    /// `quote_form_iac_forge_tag_diverges_from_sexp_shape_label_for_unquote_splice`
    /// alongside the three matched-arm agreements.
    pub const UNQUOTE_SPLICE_IAC_FORGE_TAG: &'static str = "unquote-splicing";

    /// The closed-set forced-arity ALL array over the quote-family
    /// iac-forge canonical-form tag `&'static str` bytes in canonical
    /// declaration order matching [`Self::ALL`] element-wise. Sibling
    /// posture to [`Self::PREFIXES`] (`[&'static str; 4]` on the
    /// reader-prefix axis of the SAME [`QuoteForm`] closed set),
    /// [`crate::error::MacroDefHead::KEYWORDS`] (`[&'static str; 3]`
    /// on the CL macro-definition head algebra),
    /// [`Atom::BOOL_LITERALS`] (`[&'static str; 2]` on the Scheme-bool
    /// spelling algebra), and
    /// [`crate::macro_expand::MacroParams::LAMBDA_LIST_KEYWORDS`]
    /// (`[&'static str; 2]` on the CL lambda-list-keyword algebra) —
    /// every closed-set outer projection on the substrate now pins its
    /// canonical bytes at ONE `pub const` per role plus an ALL array
    /// for family-wide consumers.
    ///
    /// The (canonical iac-forge tag) axis + the (canonical reader
    /// prefix) axis together span the two production byte-vocabularies
    /// the [`QuoteForm`] closed set carries — [`Self::PREFIXES`] holds
    /// the Lisp source-code prefixes (`"'"`, `` "`" ``, `","`, `",@"`)
    /// the reader tokenizes on, [`Self::IAC_FORGE_TAGS`] holds the
    /// cross-crate canonical-form tag strings (`"quote"`,
    /// `"quasiquote"`, `"unquote"`, `"unquote-splicing"`) the
    /// iac-forge interop layer round-trips through. Adding a
    /// hypothetical fifth homoiconic prefix (a `,~` reverse-unquote, a
    /// `,?` conditional-unquote, a `#'` Common-Lisp function-quote)
    /// extends [`Self::ALL`] AND [`Self::PREFIXES`] AND
    /// [`Self::IAC_FORGE_TAGS`] AND [`Self::prefix`]'s arm AND
    /// [`Self::iac_forge_tag`]'s arm AND two new per-role `pub const`s
    /// (one on each axis) in lockstep — rustc's forced-arity check on
    /// `[&'static str; N]` fails compilation if any of the three ALL
    /// arrays grows without the others.
    ///
    /// Future consumers that compose against [`Self::IAC_FORGE_TAGS`]:
    /// - Cross-crate canonical-form completion (an authoring tool
    ///   surfacing every legal iac-forge tag in a `(<tag> <inner>)`
    ///   template — the completion set IS [`Self::IAC_FORGE_TAGS`]
    ///   rather than four hand-enumerated `&'static str` literals per
    ///   completion provider).
    /// - `tatara-check` coverage assertions that sweep workspace
    ///   attestation payloads for every canonical iac-forge tag —
    ///   the typed sweep replaces per-consumer inline enumeration of
    ///   the four literals.
    /// - Any future audit-trail metric jointly labeled by
    ///   [`Self::iac_forge_tag`] (e.g.
    ///   `tatara_lisp_iac_forge_tag_total{tag="quote"}`) — the metric
    ///   label set IS [`Self::IAC_FORGE_TAGS`] mapped through
    ///   [`Self::iac_forge_tag`].
    pub const IAC_FORGE_TAGS: [&'static str; 4] = [
        Self::QUOTE_IAC_FORGE_TAG,
        Self::QUASIQUOTE_IAC_FORGE_TAG,
        Self::UNQUOTE_IAC_FORGE_TAG,
        Self::UNQUOTE_SPLICE_IAC_FORGE_TAG,
    ];

    /// Canonical iac-forge interop tag — the symbol head the canonical
    /// 2-element-list encoding of a quote-family wrapper uses when
    /// projecting `tatara_lisp::Sexp` into `iac_forge::sexpr::SExpr`:
    /// `"quote"` for [`Self::Quote`], `"quasiquote"` for
    /// [`Self::Quasiquote`], `"unquote"` for [`Self::Unquote`],
    /// `"unquote-splicing"` for [`Self::UnquoteSplice`].
    ///
    /// The mapping is Common-Lisp-canonical: a `,@x` form encodes as
    /// `(unquote-splicing x)` rather than `(unquote-splice x)`. That
    /// tag-string choice is INTENTIONALLY DISTINCT from the substrate's
    /// shorter diagnostic label projected by
    /// [`crate::error::SexpShape::label`] (which renders
    /// `[`Self::UnquoteSplice`]` as `"unquote-splice"` — the shorter
    /// idiom appropriate for `expected …, got unquote-splice` error
    /// surfaces). The two projections key the SAME closed set on TWO
    /// distinct boundaries:
    ///
    /// * `iac_forge_tag` — cross-crate canonical form, BLAKE3 attestation
    ///   keys, render-cache shape (load-bearing for byte-identical
    ///   inter-crate compatibility with the iac-forge ecosystem).
    /// * `SexpShape::label` — operator-facing diagnostic label,
    ///   `LispError::TypeMismatch.got` rendering, REPL/LSP
    ///   shape-of-witness surface.
    ///
    /// Pre-lift the four canonical iac-forge tag strings lived inline
    /// across four arms in [`crate::interop`]'s
    /// `From<&Sexp> for iac_forge::sexpr::SExpr` impl, paired with the
    /// matching `Sexp::{Quote, Quasiquote, Unquote, UnquoteSplice}`
    /// patterns. The pairing was load-bearing yet only enforced by
    /// callsite discipline at a FOURTH consumer site (alongside `Hash`,
    /// `Display`, and `Sexp::as_unquote`) the prior closed-set
    /// `QuoteForm` lift did not reach (the `iac-forge` feature gate
    /// kept that site's drift risk silent in the default build). After
    /// this lift the interop arms collapse to ONE arm routing through
    /// [`crate::ast::Sexp::as_quote_form`] + this method, so the
    /// (Sexp variant, canonical tag string) pairing binds at ONE site
    /// on the substrate algebra regardless of which consumer surface
    /// (`Hash`, `Display`, `Sexp::as_unquote`, iac-forge interop)
    /// needs it.
    ///
    /// The `&'static str` lifetime is load-bearing: every iac-forge
    /// consumer projects through this method into the canonical
    /// 2-element-list head without an allocation, parallel to how
    /// [`Self::prefix`], [`UnquoteForm::marker`], and
    /// [`crate::error::SexpShape::label`] project their respective
    /// closed-set surfaces. A future homoiconic prefix-wrapper (e.g.
    /// hypothetical `,~` reverse-unquote) extends [`Self`] AND this
    /// method's match arm together — rustc binds the iac-forge
    /// canonical-form surface to the algebra through exhaustiveness.
    ///
    /// Theory anchor: THEORY.md §V.1 — knowable platform; the
    /// quote-family canonical-form tag set becomes a TYPE projection
    /// on the substrate algebra rather than four `&'static str`
    /// literals scattered across the `interop` arms (parallel to how
    /// `Self::prefix` lifts the Display↔reader prefix and
    /// `Self::hash_discriminator` lifts the cache-key bytes).
    /// THEORY.md §VI.1 — generation over composition; the (Sexp
    /// variant, iac-forge tag) pairing appeared at the four
    /// `interop.rs` arms — past the ≥2 PRIME-DIRECTIVE trigger once
    /// the structural shape is named. THEORY.md §II.1 invariant 1 —
    /// typed entry; the cross-crate canonical-form projection IS the
    /// typed-exit gate at the iac-forge boundary, and naming its
    /// closed-set tag identity lifts the gate from per-site literal
    /// discipline to ONE method the iac-forge round-trip discipline
    /// binds against.
    #[must_use]
    pub fn iac_forge_tag(self) -> &'static str {
        match self {
            Self::Quote => Self::QUOTE_IAC_FORGE_TAG,
            Self::Quasiquote => Self::QUASIQUOTE_IAC_FORGE_TAG,
            Self::Unquote => Self::UNQUOTE_IAC_FORGE_TAG,
            Self::UnquoteSplice => Self::UNQUOTE_SPLICE_IAC_FORGE_TAG,
        }
    }

    /// Inverse of [`Self::iac_forge_tag`] on the four-arm canonical CL
    /// tag closed set — `"quote"` decodes to `Some(Self::Quote)`,
    /// `"quasiquote"` decodes to `Some(Self::Quasiquote)`, `"unquote"`
    /// decodes to `Some(Self::Unquote)`, `"unquote-splicing"` decodes to
    /// `Some(Self::UnquoteSplice)`. Every other `tag` (empty string,
    /// PascalCase drift, the shorter substrate diagnostic label
    /// `"unquote-splice"` — which is INTENTIONALLY distinct from the
    /// CL canonical `"unquote-splicing"` per the substrate's
    /// two-vocabulary axis pinned by
    /// `quote_form_iac_forge_tag_diverges_from_sexp_shape_label_for_unquote_splice`,
    /// every arbitrary word not in the four-arm image) yields `None`.
    ///
    /// Structural roundtrip law (pinned by
    /// `quote_form_iac_forge_tag_round_trips_through_from_iac_forge_tag`):
    /// for every `qf: QuoteForm`,
    /// `Self::from_iac_forge_tag(qf.iac_forge_tag()) == Some(qf)`.
    /// Sibling posture to [`Self::from_lead_char`]'s inverse-of-
    /// [`Self::lead_char`] contract on the reader-lead-char axis —
    /// both name the typed inverse decoder on a per-role projection
    /// axis of the same closed set, with `Option<Self>` shape mirroring
    /// the partial decode over an unbounded string codomain into the
    /// four-arm typed closed set.
    ///
    /// Load-bearing use case: cross-crate iac-forge canonical-form
    /// inbound decoding. Pre-lift a consumer parsing a canonical
    /// `(<tag> <inner>)` list from an `iac_forge::sexpr::SExpr` (a
    /// downstream deserialization codepath, an LSP quick-fix that
    /// completes an iac-forge canonical-form skeleton, a
    /// `tatara-check` predicate that reads back an attested
    /// canonical form and re-typed-witnesses its shape) would have
    /// to hand-roll `match tag { "quote" => QuoteForm::Quote,
    /// "quasiquote" => …, "unquote-splicing" => …, _ => return None
    /// }` at each callsite; post-lift the (tag, typed variant)
    /// decode binds at ONE typed method on the substrate algebra
    /// composed as a linear sweep over [`Self::ALL`] keyed on
    /// [`Self::iac_forge_tag`]. The (tag literals, decode arms)
    /// pairing lives at ONE canonical site (the four
    /// [`Self::QUOTE_IAC_FORGE_TAG`] / [`Self::QUASIQUOTE_IAC_FORGE_TAG`]
    /// / [`Self::UNQUOTE_IAC_FORGE_TAG`] /
    /// [`Self::UNQUOTE_SPLICE_IAC_FORGE_TAG`] per-role constants
    /// [`Self::iac_forge_tag`]'s outbound arms bind to) rather than
    /// at TWO — the outbound projection (existing) plus a hand-rolled
    /// inbound decoder duplicated per callsite.
    ///
    /// Boundary distinction with [`Self::from_str`] (the substrate's
    /// [`FromStr`] impl derived via `#[closed_set(via = "prefix")]`):
    /// [`Self::from_str`] decodes the reader-punctuation vocabulary
    /// (`"'"`, `` "`" ``, `","`, `",@"`); THIS method decodes the
    /// cross-crate iac-forge canonical-form vocabulary (`"quote"`,
    /// `"quasiquote"`, `"unquote"`, `"unquote-splicing"`). The two
    /// closed-set inverse decoders key the SAME four-arm outer set
    /// through TWO orthogonal byte vocabularies — pinning them at
    /// distinct methods documents the axis-orthogonality
    /// [`Self::PREFIXES`] vs [`Self::IAC_FORGE_TAGS`] carries at the
    /// per-role forced-arity ALL array level. A consumer with a
    /// reader-punctuation byte in hand routes through [`FromStr`];
    /// a consumer with an iac-forge canonical tag in hand routes
    /// through THIS method — the vocabulary axis binds at the
    /// decoder-method boundary rather than at per-consumer inline
    /// dispatch.
    ///
    /// Case-sensitive by design — matches the case-sensitive
    /// [`FromStr`] posture (which decodes reader punctuation) and
    /// every other closed-set FromStr on the substrate. Non-const
    /// because `&str` equality is not const-evaluable on stable at
    /// substrate MSRV (parallel to how [`Self::FromStr`]'s decode
    /// body is non-const while [`Self::from_lead_char`] is
    /// `const fn` because `char` equality IS const-evaluable);
    /// callers that need a decode-at-compile-time surface stay on
    /// the reader-lead-char decoder.
    ///
    /// Post-lift the (iac-forge tag, typed variant) inverse decoder
    /// closes the FIFTH inverse-projection axis on the outer-`QuoteForm`
    /// algebra alongside [`Self::from_lead_char`] (the reader-lead-char
    /// axis inverse), [`Self::FromStr`] (the reader-prefix axis
    /// inverse, derived via `#[closed_set(via = "prefix")]`),
    /// [`crate::error::SexpShape::as_quote_form`] (the outer-shape
    /// carving inverse embedding), and
    /// [`crate::error::UnquoteForm::to_quote_form`] (the
    /// substitution-subset embedding inverse). The full outer
    /// quote-family algebra now closes ALL FIVE inverse-projection
    /// axes matched with their forward-projection siblings
    /// ([`Self::lead_char`] / [`Self::prefix`] / [`Self::sexp_shape`]
    /// / [`Self::as_unquote_form`] on the forward side).
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 3 — typed exit; the
    /// inbound iac-forge canonical-form decode surface becomes a
    /// TYPE projection on the closed-set [`QuoteForm`] algebra
    /// rather than an inline match at every downstream consumer.
    /// THEORY.md §V.1 — knowable platform; the closed set of
    /// canonical CL tags becomes a decoder codomain rather than
    /// four inline `&'static str` literals scattered across future
    /// consumers that could drift independently. THEORY.md §VI.1 —
    /// generation over composition; the (tag, typed variant)
    /// pairing decodes at ONE typed method on the algebra composed
    /// from the pre-existing [`Self::ALL`] typed set and the
    /// [`Self::iac_forge_tag`] outbound projection — no new
    /// per-role primitive, the decode is a typed CONSEQUENCE of
    /// the existing family-wide primitives. Sibling posture to
    /// [`Self::from_lead_char`] which similarly composes as an
    /// inverse over [`Self::lead_char`] without introducing a new
    /// per-role primitive.
    ///
    /// Frontier inspiration: MLIR's typed-attribute
    /// `parseType(str) -> Optional<Type>` factory on the closed-set
    /// typed-attribute registry — the same inverse-decode shape on
    /// a Rust closed-set enum, where the (tag, typed variant)
    /// decode binds at ONE typed factory rather than at every
    /// downstream operation's parseAttribute callback. Racket's
    /// `(assq tag tag-alist)` typed lookup over a closed
    /// association list — the inverse decode projects through the
    /// ALL array without hand-rolling a per-tag match; `Self::ALL
    /// .iter().find(qf.iac_forge_tag() == tag)` is the Rust-typed
    /// peer on the closed-set outer-[`QuoteForm`] algebra with the
    /// ALL array standing in for Racket's typed association-list
    /// spine.
    #[must_use]
    pub fn from_iac_forge_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|qf| qf.iac_forge_tag() == tag)
    }

    /// Project the typed marker into its matching [`crate::error::SexpShape`]
    /// variant — `Quote → SexpShape::Quote`, `Quasiquote → SexpShape::Quasiquote`,
    /// `Unquote → SexpShape::Unquote`, `UnquoteSplice → SexpShape::UnquoteSplice`.
    /// ONE projection on the closed-set quote-family algebra the substrate's
    /// outer-shape projection ([`crate::domain::sexp_shape`]) routes through
    /// for the four quote-family arms — so the (Sexp variant, SexpShape
    /// variant) pairing binds at ONE site on the typed algebra rather than
    /// at four byte-identical inline arms in [`crate::domain::sexp_shape`].
    ///
    /// The SIXTH consumer of the closed-set [`QuoteForm`] algebra, sibling
    /// of [`Self::prefix`] (Display / reader prefix-string surface),
    /// [`Self::hash_discriminator`] (Hash cache-key bytes surface),
    /// [`Self::as_unquote_form`] (2-of-4 template-substitution subset gate),
    /// [`Self::iac_forge_tag`] (cross-crate canonical-form tag surface), and
    /// [`Self::wrap`] (reader's marker → `Sexp::*` constructor surface).
    /// Composes with [`SexpShape::label`] to yield the short diagnostic
    /// label string the substrate's `LispError::TypeMismatch.got` slot
    /// renders — the (QuoteForm variant, SexpShape variant, short label)
    /// triple binds end-to-end through the typed algebra so a regression
    /// that drifts the short label silently between the typed marker and
    /// the diagnostic surface is structurally impossible.
    ///
    /// Bidirectional dual: the inverse projection
    /// [`crate::error::SexpShape::as_quote_form`] (12→4, partial)
    /// covers the 4-of-12 carving of [`SexpShape`] this embed reaches.
    /// The pair `(QuoteForm::sexp_shape,
    /// SexpShape::as_quote_form)` forms an `Iso(QuoteForm, QuoteShape ⊂
    /// SexpShape)`: every typed marker round-trips through the embed
    /// (`QuoteForm::sexp_shape(qf).as_quote_form() == Some(qf)` for
    /// every `qf: QuoteForm`), every quote-shape pre-image recovers
    /// the typed marker. The non-quote-family shapes (`Nil`, `List`,
    /// every atomic-payload variant) form the kernel of the inverse —
    /// `as_quote_form` returns `None` for them. See
    /// [`crate::error::SexpShape::as_quote_form`]'s docstring for the
    /// composition law's other direction + disjointness with the
    /// atomic-payload sibling `SexpShape::as_atom_kind`.
    ///
    /// Canonical [`SexpShape`] embed target for the [`Self::Quote`]
    /// quote-family arm on the QuoteForm ⊂ SexpShape carving —
    /// [`SexpShape::Quote`]. Per-role peer of `Self::Quote` on the
    /// closed-set outer-shape embed axis; consumers with a `QuoteForm`
    /// variant in hand at compile time bind the canonical embed target
    /// through ONE typed `pub const` per role rather than through
    /// runtime dispatch via [`Self::sexp_shape`] or by re-deriving the
    /// QuoteForm ⊂ SexpShape variant pairing inline.
    ///
    /// Sibling posture to [`Self::QUOTE_LABEL`] (the per-role
    /// diagnostic label alias) and [`Self::QUOTE_HASH_DISCRIMINATOR`]
    /// (the per-role outer-Sexp cache-key byte) on the closed-set
    /// QuoteForm algebra — each closes a distinct per-role
    /// sub-vocabulary axis on the QuoteForm carving. This constant
    /// closes the FOURTH per-role axis on [`QuoteForm`] (the
    /// `SexpShape`-embed axis, paired with the pre-existing
    /// `&'static str` reader-prefix + diagnostic-label +
    /// cross-crate iac-forge-tag axes AND the `u8` cache-key axis) at
    /// ONE typed alias through the peer superset variant on the
    /// [`SexpShape`] closed set.
    ///
    /// Sibling posture to the peer 6-of-12 atomic-payload carving's
    /// per-role SHAPE aliases ([`AtomKind::SYMBOL_SHAPE`] …
    /// [`AtomKind::BOOL_SHAPE`] — every one an alias of its
    /// [`SexpShape`] peer on the AtomKind ⊂ SexpShape 6-of-12
    /// carving). Post-lift the SexpShape's per-carving embed-target
    /// axis is uniformly surfaced through per-role `pub const *_SHAPE`
    /// aliases on every sub-carving that carries a bidirectional
    /// (embed, project) `Iso(_, _ ⊂ SexpShape)` — first `AtomKind` (6),
    /// now `QuoteForm` (4).
    pub const QUOTE_SHAPE: SexpShape = SexpShape::Quote;

    /// Canonical [`SexpShape`] embed target for the [`Self::Quasiquote`]
    /// quote-family arm on the QuoteForm ⊂ SexpShape carving —
    /// [`SexpShape::Quasiquote`]. Per-role peer of `Self::Quasiquote`.
    /// See [`Self::QUOTE_SHAPE`] for the alias-chain shape every
    /// sibling shares.
    pub const QUASIQUOTE_SHAPE: SexpShape = SexpShape::Quasiquote;

    /// Canonical [`SexpShape`] embed target for the [`Self::Unquote`]
    /// quote-family arm on the QuoteForm ⊂ SexpShape carving —
    /// [`SexpShape::Unquote`]. Per-role peer of `Self::Unquote`.
    pub const UNQUOTE_SHAPE: SexpShape = SexpShape::Unquote;

    /// Canonical [`SexpShape`] embed target for the [`Self::UnquoteSplice`]
    /// quote-family arm on the QuoteForm ⊂ SexpShape carving —
    /// [`SexpShape::UnquoteSplice`]. Per-role peer of
    /// `Self::UnquoteSplice`.
    pub const UNQUOTE_SPLICE_SHAPE: SexpShape = SexpShape::UnquoteSplice;

    /// Closed-set forced-arity ALL array over the canonical
    /// [`SexpShape`] embed targets on the QuoteForm ⊂ SexpShape
    /// 4-of-12 carving, in declaration order matching [`Self::ALL`]
    /// element-wise (pinned by
    /// `quote_form_shapes_align_with_all_by_index`). Sibling posture
    /// to [`Self::LABELS`] (`[&'static str; 4]` — per-role diagnostic
    /// bytes), [`Self::PREFIXES`] (`[&'static str; 4]` — per-role
    /// reader-punctuation bytes), [`Self::IAC_FORGE_TAGS`] (`[&'static
    /// str; 4]` — cross-crate iac-forge canonical-form tag bytes), and
    /// [`Self::HASH_DISCRIMINATORS`] (`[u8; 4]` — per-role outer-Sexp
    /// cache-key bytes) on the SAME closed-set QuoteForm algebra;
    /// where those four arrays lift per-role `&'static str` and `u8`
    /// sub-vocabularies onto the substrate, this array lifts the
    /// per-role [`SexpShape`] embed-target sub-vocabulary at the same
    /// `[_; 4]` forced arity.
    ///
    /// Sibling posture to [`AtomKind::SHAPES`] (`[SexpShape; 6]`) —
    /// the peer atomic-payload carving's family-wide embed-target
    /// array on the AtomKind ⊂ SexpShape 6-of-12 carving. Together the
    /// two `SHAPES` arrays cover the TWO bidirectional sub-carvings of
    /// [`SexpShape`] (`Iso(AtomKind, AtomShape ⊂ SexpShape)` + `Iso(QuoteForm,
    /// QuoteShape ⊂ SexpShape)`) — a family-wide sweep zipping every
    /// carving's `ALL` + `SHAPES` in lockstep now closes over TWO
    /// carvings' 10-of-12 embed targets at ONE typed pair-of-arrays
    /// each.
    ///
    /// Pre-lift the four [`SexpShape`] embed targets had NO per-role
    /// primitive on this closed-set algebra — a consumer with a
    /// `QuoteForm` variant in hand at compile time reaching for the
    /// canonical embed target had to spell
    /// `QuoteForm::Quote.sexp_shape()` (runtime dispatch through the
    /// four-arm match body) OR re-derive the QuoteForm ⊂ SexpShape
    /// variant pairing at the call site by importing both enums and
    /// spelling `SexpShape::Quote` inline. Post-lift the FOUR canonical
    /// embed targets bind at ONE `pub const` per role on the typed
    /// [`QuoteForm`] algebra AND at [`Self::SHAPES`] as a family-wide
    /// forced-arity array — a future LSP / REPL completion bar keyed
    /// on `QuoteForm::SHAPES` for the "which SexpShape does this
    /// QuoteForm embed into?" outer-shape column, a `tatara-check`
    /// coverage sweep zipping `QuoteForm::ALL` / `LABELS` / `PREFIXES`
    /// / `IAC_FORGE_TAGS` / `HASH_DISCRIMINATORS` / `SHAPES` in
    /// lockstep for a family-wide (variant, label, prefix, iac-forge
    /// tag, byte, embed-target) sextuple render, or a Sekiban
    /// audit-trail metric jointly labeled by the embed-target's
    /// SexpShape identity reads through the typed constants on this
    /// subset algebra without re-deriving the 4-of-12 carving inline.
    ///
    /// Round-trip identity with the inverse projection
    /// [`crate::error::SexpShape::as_quote_form`]: for every index `i`,
    /// `Self::SHAPES[i].as_quote_form() == Some(Self::ALL[i])`
    /// (pinned by
    /// `quote_form_shapes_align_with_all_by_index_through_as_quote_form`) —
    /// the embed / project section closes as a family-wide array-
    /// indexed law rather than as a per-variant assertion sweep.
    /// Adding a hypothetical fifth quote-family wrapper (e.g. `,~`
    /// reverse-unquote, `,?` conditional-unquote, `#'` Common-Lisp
    /// function-quote) extends [`Self::ALL`] AND [`Self::SHAPES`] AND
    /// [`SexpShape::ALL`] AND adds ONE per-role `pub const *_SHAPE` in
    /// lockstep — rustc's forced-arity check on the two `[_; N]`
    /// arrays fails compilation if EITHER ALL array grows without the
    /// other, AND the peer [`SexpShape::as_quote_form`] arm must grow
    /// in lockstep to preserve the round-trip identity.
    ///
    /// Theory anchor: THEORY.md §III — the typescape; the four
    /// canonical [`SexpShape`] embed targets bind at ONE typed
    /// `[SexpShape; 4]` array on the closed-set QuoteForm algebra
    /// rather than at zero-primitive-on-this-subset-plus-four-inline-
    /// lookups scattered across the substrate. Closes the FOURTH
    /// per-role `pub const` axis on the QuoteForm carving alongside
    /// the pre-existing LABELS + PREFIXES + IAC_FORGE_TAGS +
    /// HASH_DISCRIMINATORS axes. THEORY.md §V.1 — knowable platform;
    /// the family's cardinality becomes a TYPE-level constant on the
    /// substrate algebra rather than a per-consumer runtime dispatch
    /// through the composition. THEORY.md §II.1 invariant 2 — free
    /// middle; the (embed, project) pair binds at THREE typed sites
    /// now — the projection method [`Self::sexp_shape`], this family-
    /// wide array, AND the peer inverse
    /// [`crate::error::SexpShape::as_quote_form`] — with rustc-enforced
    /// consistency across all three. THEORY.md §VI.1 — generation
    /// over composition; the family-wide contract sweeps (alignment
    /// with `ALL`, round-trip through `as_quote_form`, membership
    /// through `sexp_shape`, pairwise injectivity across the four
    /// embed targets) emerge from the composition of TWO substrate
    /// primitives (this `pub const` array + the four per-role
    /// `pub const *_SHAPE` aliases) rather than as per-variant inline
    /// assertions duplicated at each call site.
    pub const SHAPES: [SexpShape; 4] = [
        Self::QUOTE_SHAPE,
        Self::QUASIQUOTE_SHAPE,
        Self::UNQUOTE_SHAPE,
        Self::UNQUOTE_SPLICE_SHAPE,
    ];

    /// Project the typed marker into its matching [`crate::error::SexpShape`]
    /// variant — `Quote → SexpShape::Quote`, `Quasiquote → SexpShape::Quasiquote`,
    /// `Unquote → SexpShape::Unquote`, `UnquoteSplice → SexpShape::UnquoteSplice`.
    /// ONE projection on the closed-set quote-family algebra the substrate's
    /// outer-shape projection ([`crate::domain::sexp_shape`]) routes through
    /// for the four quote-family arms — so the (Sexp variant, SexpShape
    /// variant) pairing binds at ONE site on the typed algebra rather than
    /// at four byte-identical inline arms in [`crate::domain::sexp_shape`].
    ///
    /// The SIXTH consumer of the closed-set [`QuoteForm`] algebra, sibling
    /// of [`Self::prefix`] (Display / reader prefix-string surface),
    /// [`Self::hash_discriminator`] (Hash cache-key bytes surface),
    /// [`Self::as_unquote_form`] (2-of-4 template-substitution subset gate),
    /// [`Self::iac_forge_tag`] (cross-crate canonical-form tag surface), and
    /// [`Self::wrap`] (reader's marker → `Sexp::*` constructor surface).
    /// Composes with [`SexpShape::label`] to yield the short diagnostic
    /// label string the substrate's `LispError::TypeMismatch.got` slot
    /// renders — the (QuoteForm variant, SexpShape variant, short label)
    /// triple binds end-to-end through the typed algebra so a regression
    /// that drifts the short label silently between the typed marker and
    /// the diagnostic surface is structurally impossible.
    ///
    /// Each arm routes through the per-role `pub const` on `impl Self`
    /// ([`Self::QUOTE_SHAPE`], [`Self::QUASIQUOTE_SHAPE`],
    /// [`Self::UNQUOTE_SHAPE`], [`Self::UNQUOTE_SPLICE_SHAPE`]) so the
    /// four canonical embed targets bind at ONE typed source of truth
    /// per role rather than as inline `SexpShape::X` literals scattered
    /// across the `match` body. Sibling posture to
    /// [`AtomKind::sexp_shape`]'s post-lift routing through
    /// [`AtomKind::SYMBOL_SHAPE`] … [`AtomKind::BOOL_SHAPE`] on the peer
    /// 6-of-12 atomic-payload carving — the per-role `pub const *_SHAPE`
    /// routing is now uniform across every sub-carving of [`SexpShape`]
    /// that has a bidirectional (embed, project) isomorphism, closing
    /// the (embed-target constant, embed-target array, projection
    /// method) trio on each sub-carving in lockstep.
    ///
    /// Post-lift routing pin
    /// `quote_form_sexp_shape_routes_through_typed_per_role_constants`
    /// catches a regression that re-inlines the four `SexpShape::X` arm
    /// literals here and silently drifts ONE arm from the per-role
    /// `pub const` alias — the routing agreement is a TYPED CONSEQUENCE
    /// of the composition rather than literal discipline at two sites.
    ///
    /// Bidirectional dual: the inverse projection
    /// [`crate::error::SexpShape::as_quote_form`] (12→4, partial)
    /// covers the 4-of-12 carving of [`SexpShape`] this embed reaches.
    /// The pair `(QuoteForm::sexp_shape,
    /// SexpShape::as_quote_form)` forms an `Iso(QuoteForm, QuoteShape ⊂
    /// SexpShape)`: every typed marker round-trips through the embed
    /// (`QuoteForm::sexp_shape(qf).as_quote_form() == Some(qf)` for
    /// every `qf: QuoteForm`), every quote-shape pre-image recovers
    /// the typed marker. The non-quote-family shapes (`Nil`, `List`,
    /// every atomic-payload variant) form the kernel of the inverse —
    /// `as_quote_form` returns `None` for them. See
    /// [`crate::error::SexpShape::as_quote_form`]'s docstring for the
    /// composition law's other direction + disjointness with the
    /// atomic-payload sibling `SexpShape::as_atom_kind`.
    ///
    /// Theory anchor: THEORY.md §V.1 — knowable platform; the (QuoteForm
    /// variant, SexpShape variant) pairing becomes a TYPE projection on
    /// the substrate algebra rather than four inline arms in
    /// [`crate::domain::sexp_shape`]. A typo or swap at the shape-projection
    /// site is no longer a runtime drift but a compile error against the
    /// typed projection. THEORY.md §II.1 invariant 2 — free middle; SIX
    /// consumers of the [`QuoteForm`] algebra now route through ONE typed
    /// closed-set match family, so a regression that drifts ONE consumer's
    /// pairing from the others cannot reach the substrate's runtime.
    /// THEORY.md §VI.1 — generation over composition; the (Sexp variant,
    /// SexpShape variant) pairing appeared at four arms in `sexp_shape` —
    /// past the ≥2 PRIME-DIRECTIVE trigger once the structural shape is
    /// named.
    #[must_use]
    pub fn sexp_shape(self) -> SexpShape {
        match self {
            Self::Quote => Self::QUOTE_SHAPE,
            Self::Quasiquote => Self::QUASIQUOTE_SHAPE,
            Self::Unquote => Self::UNQUOTE_SHAPE,
            Self::UnquoteSplice => Self::UNQUOTE_SPLICE_SHAPE,
        }
    }

    /// Project the typed marker to its canonical short diagnostic label —
    /// `"quote"` for [`Self::Quote`], `"quasiquote"` for
    /// [`Self::Quasiquote`], `"unquote"` for [`Self::Unquote`],
    /// `"unquote-splice"` for [`Self::UnquoteSplice`]. Body composes
    /// through `self.sexp_shape().label()` — routing through
    /// [`Self::sexp_shape`] (the typed 4-of-12 outer-value → SexpShape
    /// projection) then [`SexpShape::label`] (the canonical 12-arm
    /// diagnostic-label projection) so the (QuoteForm variant, short
    /// diagnostic string) pairing lives at ONE canonical site
    /// ([`SexpShape::label`]'s four quote-family arms in `error.rs`)
    /// rather than at four inline `&'static str` arms on the closed-set
    /// `QuoteForm` algebra.
    ///
    /// The outer-shape peer of [`crate::ast::Sexp::type_name`] one
    /// algebra layer up (`self.shape().label()` on outer-`Sexp`) and of
    /// [`crate::ast::Atom::label`] one algebra layer down
    /// (`self.kind().label()` on outer-`Atom` through [`AtomKind`]).
    /// Where `Atom::label` composes through the atomic-payload 6-of-12
    /// carving via [`AtomKind`] into [`SexpShape::label`], this method
    /// composes through the quote-family 4-of-12 carving directly onto
    /// [`SexpShape::label`] — the (label, sexp_shape, hash_discriminator)
    /// trio the outer-`Atom` algebra closed one lift back
    /// (`Atom::hash_discriminator`, e49f550) is now mirrored on the
    /// `QuoteForm` algebra: `prefix` (reader punctuation) and
    /// `iac_forge_tag` (CL canonical form) key the SAME closed set on
    /// their own boundaries, and `label` keys it on the substrate's
    /// operator-facing diagnostic boundary.
    ///
    /// Composition law: `qf.label() == qf.sexp_shape().label()` for every
    /// `qf: QuoteForm`. Pinned by
    /// `quote_form_label_composes_through_sexp_shape_label_for_every_variant`
    /// across all four variants — the pin asserts pointer-equality on the
    /// returned `&'static str` so a regression that re-inlines the four
    /// literals here (and gains its own drift surface separate from the
    /// canonical [`SexpShape::label`] site) surfaces immediately. Sibling
    /// of `atom_label_composes_through_kind_label_for_every_variant` one
    /// algebra layer down (on the outer-`Atom` value / `AtomKind` marker
    /// pair) and
    /// `sexp_type_name_method_composes_through_shape_label_for_every_outer_shape`
    /// one algebra layer up (on the outer-`Sexp` value / `SexpShape`
    /// marker pair).
    ///
    /// Cross-algebra agreement law: for every `qf: QuoteForm` and every
    /// `inner: Sexp`, `qf.label() == qf.wrap(inner).type_name()`. The
    /// (QuoteForm variant, canonical label) pairing lands at the SAME
    /// `&'static str` regardless of whether the consumer holds the typed
    /// marker directly or an outer-`Sexp` wrapper produced from
    /// [`Self::wrap`] — so a regression that drifts one algebra layer's
    /// label from the other (a `QuoteForm::label` re-inlined onto a
    /// different literal, a `Sexp::type_name` re-routed through a stale
    /// shape projection, a `QuoteForm::sexp_shape` arm that swaps two
    /// markers) fails-loudly here rather than as a silent operator-facing
    /// diagnostic drift at every consumer that pattern-matches on the
    /// outer-`Sexp` label vs the outer-`QuoteForm` label independently.
    /// Pinned by `quote_form_label_agrees_with_sexp_type_name_at_every_quote_form_arm`.
    ///
    /// Divergence law (boundary distinction with [`Self::iac_forge_tag`]):
    /// at the [`Self::UnquoteSplice`] arm, `qf.label() == "unquote-splice"`
    /// while `qf.iac_forge_tag() == "unquote-splicing"`. The two
    /// projections key the SAME closed-set on TWO distinct boundaries
    /// (substrate diagnostic surface vs cross-crate CL canonical form)
    /// and their intentional divergence at the `Splice` arm is pinned by
    /// `quote_form_label_diverges_from_iac_forge_tag_for_unquote_splice`
    /// — sibling posture to
    /// `quote_form_iac_forge_tag_diverges_from_sexp_shape_label_for_unquote_splice`
    /// which pinned the divergence at the `sexp_shape().label()`
    /// composition; this pin lifts the divergence contract onto the new
    /// typed peer.
    ///
    /// The `&'static str` lifetime is load-bearing: every future consumer
    /// with a `QuoteForm` in hand wanting the substrate's short
    /// diagnostic string projects through this method into the
    /// `LispError::TypeMismatch.got` slot / REPL / LSP surface without an
    /// allocation, parallel to how [`Self::prefix`] projects the reader
    /// punctuation and [`Self::iac_forge_tag`] projects the CL canonical
    /// tag. A future homoiconic prefix-wrapper (e.g. hypothetical `,~`
    /// reverse-unquote) extends [`Self`] AND [`SexpShape::label`]
    /// together — rustc binds the diagnostic surface to the algebra
    /// through the closed-set composition without touching this method.
    ///
    /// Theory anchor: THEORY.md §V.1 — knowable platform; the (QuoteForm
    /// variant, canonical short label) pairing becomes a TYPE projection
    /// on the substrate algebra composed through the pre-existing outer-
    /// shape projection, rather than at a per-callsite
    /// `.sexp_shape().label()` two-hop the load-bearing pin already
    /// carries as a composition-law contract. THEORY.md §II.1 invariant 2
    /// — free middle; the outer-`QuoteForm` diagnostic-label algebra now
    /// closes over THREE typed layers (`QuoteForm` → [`SexpShape`] →
    /// `&'static str`) with rustc-enforced consistency across each — a
    /// regression that drifts ONE layer's mapping from the others cannot
    /// reach the substrate's runtime typed-witness surface,
    /// `LispError::TypeMismatch.got` slot, or [`crate::error::SexpWitness::shape`]
    /// projection. THEORY.md §VI.1 — generation over composition; the
    /// outer-value diagnostic-label projection is the missing algebra
    /// layer between the outer `QuoteForm` and the pre-existing marker-
    /// level label projection — the two pre-existing typed layers become
    /// a full THREE-layer typed composition through ONE new named
    /// projection, closing the (prefix, iac_forge_tag, sexp_shape,
    /// hash_discriminator, label) quintet on the outer-`QuoteForm`
    /// algebra.
    ///
    /// Frontier inspiration: MLIR's `mlir::OperationName::getStringRef()`
    /// composed with an op-family typed projection — narrowing a
    /// closed-set op-family value through its typed identity yields the
    /// canonical diagnostic string identity in ONE typed composition on
    /// the op-family algebra. Translated through the substrate's
    /// [`QuoteForm`] outer-marker algebra, `qf.sexp_shape().label()`
    /// closes the (typed marker, canonical diagnostic label) pairing at
    /// ONE typed projection on the marker algebra composed through the
    /// outer-shape's per-carving canonical site. Racket's `(quote-kind
    /// qf)` composed with `(kind-label kind)` on the quote-family
    /// taxonomy — the typed diagnostic label emerges from a two-hop
    /// composition on the closed-set marker through the typed outer-shape
    /// identity. `QuoteForm::label` is the Rust-typed peer on the
    /// closed-set outer-[`QuoteForm`] algebra with [`SexpShape`] standing
    /// in for Racket's quote-family taxonomy.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.sexp_shape().label()
    }

    /// Canonical `&'static str` bytes for the [`Self::Quote`] quote-family
    /// marker — aliases [`SexpShape::QUOTE_LABEL`] on the QuoteForm ⊂
    /// SexpShape carving so the marker-level per-role bytes bind at ONE
    /// `pub const` on the parent superset's quote-family arm rather than
    /// at TWO sites (the per-role `pub const` AND a parallel inline
    /// literal). Per-role peer of `Self::Quote` on the closed-set quote-
    /// family algebra; consumers reach for `QuoteForm::QUOTE_LABEL` when
    /// the caller has a variant in hand at compile time and wants the
    /// canonical diagnostic bytes without runtime dispatch through
    /// [`Self::label`].
    ///
    /// Sibling posture to the peer 6-of-12 atomic-payload carving's per-
    /// role LABEL aliases ([`crate::ast::AtomKind::SYMBOL_LABEL`] …
    /// [`crate::ast::AtomKind::BOOL_LABEL`] — every one an alias of its
    /// [`SexpShape`] peer) and the peer 2-of-12 structural-residual
    /// carving's per-role LABEL aliases
    /// ([`crate::error::StructuralKind::NIL_LABEL`] +
    /// [`crate::error::StructuralKind::LIST_LABEL`]) — this closes the
    /// fourth and final closed-set sub-carving of [`SexpShape`] whose
    /// per-role diagnostic-label bytes are surfaced through the same
    /// alias-chain shape rather than reachable only through the
    /// composition [`Self::sexp_shape`] + [`SexpShape::label`]. Every
    /// SexpShape sub-carving (atomic payload, quote family, structural
    /// residual) now exposes its per-role LABEL bytes at ONE `pub const`
    /// per role on its subset algebra AS WELL AS at the parent
    /// superset's `SexpShape::*_LABEL`.
    ///
    /// The prefix-family peer of THIS `&'static str` constant is
    /// [`Self::QUOTE_PREFIX`] (`"'"` — reader-punctuation byte); the
    /// canonical-form peer is [`Self::QUOTE_IAC_FORGE_TAG`] (`"quote"` —
    /// cross-crate iac-forge tag). At the `Quote` arm the label and
    /// the iac-forge tag agree byte-for-byte (`"quote"`); the divergence
    /// axis lives at [`Self::UNQUOTE_SPLICE_LABEL`] (`"unquote-splice"`)
    /// vs [`Self::UNQUOTE_SPLICE_IAC_FORGE_TAG`] (`"unquote-splicing"`).
    /// The three parallel per-role `pub const` families (prefix, label,
    /// iac-forge tag) close the (reader, diagnostic, canonical-form)
    /// triple on the outer-`QuoteForm` algebra.
    pub const QUOTE_LABEL: &'static str = SexpShape::QUOTE_LABEL;

    /// Canonical `&'static str` bytes for the [`Self::Quasiquote`] quote-
    /// family marker — aliases [`SexpShape::QUASIQUOTE_LABEL`] on the
    /// QuoteForm ⊂ SexpShape carving. Per-role peer of `Self::Quasiquote`.
    /// See [`Self::QUOTE_LABEL`] for the alias-chain shape every sibling
    /// shares.
    pub const QUASIQUOTE_LABEL: &'static str = SexpShape::QUASIQUOTE_LABEL;

    /// Canonical `&'static str` bytes for the [`Self::Unquote`] quote-
    /// family marker — aliases [`SexpShape::UNQUOTE_LABEL`] on the
    /// QuoteForm ⊂ SexpShape carving. Per-role peer of `Self::Unquote`.
    /// See [`Self::QUOTE_LABEL`] for the alias-chain shape every sibling
    /// shares.
    pub const UNQUOTE_LABEL: &'static str = SexpShape::UNQUOTE_LABEL;

    /// Canonical `&'static str` bytes for the [`Self::UnquoteSplice`]
    /// quote-family marker — aliases [`SexpShape::UNQUOTE_SPLICE_LABEL`]
    /// on the QuoteForm ⊂ SexpShape carving. Per-role peer of
    /// `Self::UnquoteSplice`; the `"unquote-splice"` short label matches
    /// [`SexpShape::UNQUOTE_SPLICE_LABEL`] byte-for-byte and diverges
    /// INTENTIONALLY from [`Self::UNQUOTE_SPLICE_IAC_FORGE_TAG`]
    /// (`"unquote-splicing"`) — the two projections key the SAME closed
    /// set on TWO distinct boundaries (substrate diagnostic surface vs
    /// cross-crate Common-Lisp canonical form). The divergence is pinned
    /// by `quote_form_label_diverges_from_iac_forge_tag_for_unquote_splice`
    /// on the runtime projection and by the byte-equality pins on THIS
    /// constant vs its iac-forge peer on the per-role `pub const`
    /// surface. See [`Self::QUOTE_LABEL`] for the alias-chain shape
    /// every sibling shares.
    pub const UNQUOTE_SPLICE_LABEL: &'static str = SexpShape::UNQUOTE_SPLICE_LABEL;

    /// Closed-set forced-arity ALL array over the canonical quote-family
    /// marker `&'static str` bytes, in declaration order matching
    /// [`Self::ALL`] element-wise (pinned by
    /// `quote_form_labels_align_with_all_by_index`). Sibling posture to
    /// [`crate::error::SexpShape::LABELS`] (`[&'static str; 12]` — the
    /// superset carving this QuoteForm subset embeds into),
    /// [`crate::ast::AtomKind::LABELS`] (`[&'static str; 6]` — the peer
    /// 6-of-12 atomic-payload carving's ALL array),
    /// [`crate::error::StructuralKind::LABELS`] (`[&'static str; 2]` —
    /// the peer 2-of-12 structural-residual carving's ALL array),
    /// [`Self::PREFIXES`] (`[&'static str; 4]` — reader-prefix axis on
    /// this same algebra), and [`Self::IAC_FORGE_TAGS`]
    /// (`[&'static str; 4]` — canonical-form tag axis on this same
    /// algebra) — every closed-set outer projection on the substrate
    /// that carries an `&'static str`-per-variant label now pins its
    /// per-role canonical bytes at ONE `pub const` per role PLUS an ALL
    /// array for family-wide consumers.
    ///
    /// Pre-lift the four quote-family marker labels had NO per-role
    /// primitive on this closed-set algebra — a consumer with a
    /// [`QuoteForm`] variant in hand at compile time reaching for the
    /// canonical diagnostic bytes had to spell `QuoteForm::Quote.label()`
    /// (runtime dispatch through the composition [`Self::sexp_shape`] +
    /// [`SexpShape::label`]) OR reach across the algebra boundary into
    /// [`SexpShape::QUOTE_LABEL`] and re-derive the QuoteForm ⊂
    /// SexpShape variant pairing at the call site. Post-lift the FOUR
    /// canonical labels bind at ONE `pub const` per role on the typed
    /// [`QuoteForm`] algebra AND at [`Self::LABELS`] as a family-wide
    /// forced-arity array — a future LSP / REPL completion bar keyed on
    /// `QuoteForm::LABELS` for the "quote-family" carving-axis column,
    /// a `tatara-check` coverage sweep over the quote-family arms of a
    /// `TypeMismatch.got` corpus, or a Sekiban audit-trail metric
    /// jointly labeled by the quote-family marker
    /// (`tatara_lisp_quote_family_label_total{label="quote"}`) reads
    /// through the typed constants on this subset algebra without re-
    /// deriving the 4-of-12 carving inline OR reaching across into the
    /// superset's twelve-entry `SexpShape::LABELS` array + filtering.
    ///
    /// Each entry is byte-for-byte identical to the corresponding
    /// [`SexpShape`] quote-family arm — an intentional cross-axis
    /// overlap pinned by
    /// `quote_form_per_role_labels_alias_sexp_shape_per_role_labels_byte_for_byte`
    /// so a future label rename on EITHER side (a `SexpShape`
    /// `"quote"` → `"cite"` drift, a `QuoteForm` rename that skips the
    /// alias, a hypothetical Racket-compat swap of `"quasiquote"`)
    /// fails-loudly at the alias test rather than as a silent operator-
    /// facing vocabulary fracture. Adding a hypothetical fifth
    /// homoiconic prefix-wrapper (a `,~` reverse-unquote, a `,?`
    /// conditional-unquote, a `#'` Common-Lisp function-quote) extends
    /// [`Self::ALL`] AND [`Self::LABELS`] AND adds ONE per-role
    /// `pub const` alias in lockstep — rustc's forced-arity check on
    /// the two `[_; N]` arrays fails compilation if EITHER ALL array
    /// grows without the other.
    ///
    /// Theory anchor: THEORY.md §III — the typescape; the four
    /// canonical quote-family marker labels bind at ONE typed
    /// `[&'static str; 4]` array on the closed-set [`QuoteForm`]
    /// algebra rather than at zero-primitive-on-this-subset-plus-four-
    /// inline-lookups scattered across the substrate. Closes the
    /// fourth SexpShape sub-carving's per-role LABEL parity with
    /// [`AtomKind`] and [`crate::error::StructuralKind`]. THEORY.md
    /// §V.1 — knowable platform; the family's cardinality becomes a
    /// TYPE-level constant on the substrate algebra rather than a per-
    /// consumer runtime dispatch through the composition. The alias-
    /// chain shape is load-bearing: a [`SexpShape`]-side rename
    /// propagates through the const-eval alias chain byte-for-byte
    /// without silent drift. THEORY.md §VI.1 — generation over
    /// composition; the family-wide contract sweeps (alignment with
    /// [`Self::ALL`], pairwise disjointness, membership through
    /// [`Self::label`]) emerge from the composition of TWO substrate
    /// primitives (this `pub const` array + the four per-role
    /// `pub const *_LABEL` aliases) rather than as per-variant inline
    /// assertions duplicated at each call site. THEORY.md §II.1
    /// invariant 5 — composition preserves proofs; the alias-chain
    /// composition law `QuoteForm::LABELS[i] ==
    /// QuoteForm::ALL[i].sexp_shape().label()` binds the family-wide
    /// array to the composition through [`Self::sexp_shape`] +
    /// [`SexpShape::label`] at rustc time.
    pub const LABELS: [&'static str; 4] = [
        Self::QUOTE_LABEL,
        Self::QUASIQUOTE_LABEL,
        Self::UNQUOTE_LABEL,
        Self::UNQUOTE_SPLICE_LABEL,
    ];

    /// Project the typed marker back into its matching `Sexp::*` wrapper
    /// variant applied to `inner` — the structural inverse of
    /// [`crate::ast::Sexp::as_quote_form`]. [`Self::Quote`] yields
    /// [`Sexp::Quote`], [`Self::Quasiquote`] yields [`Sexp::Quasiquote`],
    /// [`Self::Unquote`] yields [`Sexp::Unquote`], [`Self::UnquoteSplice`]
    /// yields [`Sexp::UnquoteSplice`], each boxing `inner` into the
    /// corresponding tuple-variant constructor (`fn(Box<Sexp>) -> Sexp`).
    ///
    /// Round-trip identity with [`crate::ast::Sexp::as_quote_form`] — the
    /// structural law every consumer can pin against:
    ///
    /// ```ignore
    /// // for every (qf, inner): qf.wrap(inner.clone()).as_quote_form() == Some((qf, &inner))
    /// // for every Sexp s matching the quote family:
    /// //     let (qf, inner) = s.as_quote_form().unwrap();
    /// //     qf.wrap(inner.clone()) == s
    /// ```
    ///
    /// Consumer: [`crate::reader::read_quoted`] — the FIFTH consumer site
    /// of the closed-set `QuoteForm` algebra (sibling to `Hash for Sexp`'s
    /// `hash_discriminator` arm, `Display for Sexp`'s `prefix` arm,
    /// `Sexp::as_unquote`'s `as_unquote_form` subset-gate composition, and
    /// the feature-gated `From<&Sexp> for iac_forge::SExpr`'s
    /// `iac_forge_tag` arm). Pre-lift the reader's parse dispatch carried
    /// its own parallel closed set: a local `Token::{Quote, Quasiquote,
    /// Unquote, UnquoteSplice}` enum paired with the matching `Sexp::*`
    /// tuple-variant constructors threaded as `fn(Box<Sexp>) -> Sexp`
    /// arguments to `read_quoted`. The (Token variant, Sexp::* constructor)
    /// pairing was load-bearing yet only enforced by callsite discipline
    /// at the FIFTH consumer site the prior `QuoteForm` lifts did not
    /// reach — a regression that swapped `Sexp::Quote` and
    /// `Sexp::Quasiquote` between the parser arms type-checked but
    /// silently corrupted every program's quote-family parse.
    ///
    /// Post-lift the reader's `Token` collapses to ONE typed variant
    /// `Token::Quoted(QuoteForm)`, the parser's four prefix arms collapse
    /// to ONE arm `Some((Token::Quoted(qf), _)) => read_quoted(it,
    /// eof_pos, qf)`, and `read_quoted` routes through this projection to
    /// produce the matching `Sexp::*` variant. The (QuoteForm variant,
    /// Sexp::* constructor) pairing now binds at ONE site on the typed
    /// algebra — rustc enforces exhaustiveness across [`Self`]'s closed
    /// set, so a regression that drifts the (marker, constructor) pair
    /// becomes a typed compile error rather than a silent program-text
    /// corruption.
    ///
    /// The `Sexp` (owned) return type complements [`Sexp::as_quote_form`]'s
    /// `&Sexp` (borrowed) — `wrap` consumes the inner body to build the
    /// new wrapper, `as_quote_form` borrows the inner body from the
    /// existing wrapper. The asymmetry is intentional: at the reader's
    /// parse-then-wrap boundary the inner is fresh from `parse(...)?` and
    /// has no caller-owned binding; the typed `Box::new(inner)` allocation
    /// lives at ONE site rather than four (one per pre-lift parser arm),
    /// so a future allocation-policy change (e.g. arena-allocated wrappers
    /// for span-aware Sexp) lands as ONE edit.
    ///
    /// Theory anchor: THEORY.md §II.1 invariant 1 — typed entry; the
    /// reader's prefix-token → Sexp-wrapper gate IS the rust-level
    /// typed-entry gate at the source-text boundary, and naming the
    /// typed projection from [`QuoteForm`] back to the `Sexp::*` wrapper
    /// lifts the gate from per-arm constructor literals to ONE method
    /// the closed-set algebra owns — parallel to how [`Self::prefix`]
    /// lifts the Display↔reader prefix-string surface. THEORY.md §II.1
    /// invariant 2 — free middle; ALL FIVE consumers (Hash, Display,
    /// as_unquote, iac-forge interop, reader's parse) now route through
    /// the SAME closed-set algebra so a regression that drifts ONE
    /// consumer's pairing from the others cannot reach the substrate's
    /// runtime. THEORY.md §V.1 — knowable platform; the (QuoteForm
    /// variant, Sexp::* constructor) pairing becomes a TYPE projection on
    /// the substrate algebra rather than four `fn(Box<Sexp>) -> Sexp`
    /// function pointers threaded as call arguments. A typo or
    /// swap is no longer a runtime drift but a compile error against the
    /// typed projection. THEORY.md §VI.1 — generation over composition;
    /// the (QuoteForm variant, Sexp::* constructor) pairing appeared at
    /// the four reader arms — past the ≥2 PRIME-DIRECTIVE trigger once
    /// the structural shape is named. The typed projection lands the
    /// structural-completeness floor for the reader's quote-family
    /// surface, completing the FIVE-consumer closure of the
    /// `QuoteForm` algebra.
    #[must_use]
    pub fn wrap(self, inner: Sexp) -> Sexp {
        let boxed = Box::new(inner);
        match self {
            Self::Quote => Sexp::Quote(boxed),
            Self::Quasiquote => Sexp::Quasiquote(boxed),
            Self::Unquote => Sexp::Unquote(boxed),
            Self::UnquoteSplice => Sexp::UnquoteSplice(boxed),
        }
    }
}

// `impl fmt::Display for QuoteForm` is generated by
// `#[derive(tatara_closed_set::DeriveClosedSet)]` + `#[closed_set(display)]` on
// the enum declaration above — emits the substrate-wide
// `f.write_str(Self::prefix(*self))` block byte-for-byte.

// `impl std::str::FromStr for QuoteForm` + `impl crate::ClosedSet for
// QuoteForm` + `pub struct UnknownQuoteForm(pub String)` are generated by
// `#[derive(tatara_closed_set::DeriveClosedSet)]` on the enum declaration
// above. `label` delegates to the inherent `QuoteForm::prefix` via
// `#[closed_set(via = "prefix")]` so the domain-canonical
// reader-punctuation projection (`"'" / "`" / "," / ",@"`) stays
// load-bearing at the inherent surface while the trait surface unifies
// every closed-set implementor's projection name onto `label`.
// `#[closed_set(generate_unknown = "quote form")]` emits the typed
// parse-rejection carrier with the substrate-wide `Debug + Clone +
// PartialEq + Eq + thiserror::Error` derives and the `#[error("unknown
// quote form: {0}")]` annotation byte-for-byte; the explicit label pins
// the pre-lift wording even though the auto-derived
// `pascal_to_spaced_lowercase("QuoteForm")` projects to the same
// `"quote form"` literal. The FromStr decode is a linear sweep over
// `QuoteForm::ALL` keyed on `prefix`: every successful decode round-trips
// through `prefix()`, cross-axis labels from `SexpShape` (`"quote" /
// "quasiquote" / ...`) and `iac_forge_tag` (`"unquote-splicing"`) reject —
// pinned by `quote_form_prefix_round_trips_through_from_str` +
// `quote_form_from_str_rejects_sexp_shape_labels_on_homoiconic_prefix_axis`.

// ── Const-eval well-formedness helpers over `&'static str` arrays, ported
//    from the retired fork's ast.rs. These are what the closed-set carriers'
//    label-array invariants are proved WITH: every `const _: () = assert_…`
//    witness is evaluated at `cargo check` time, so a duplicated or empty or
//    non-ASCII label is a COMPILE error, not a test failure.

pub const fn assert_str_array_pairwise_distinct<const N: usize>(arr: &[&'static str; N]) {
    let mut i = 0;
    while i < N {
        let mut j = i + 1;
        while j < N {
            if str_bytes_equal(arr[i], arr[j]) {
                panic!(
                    "assert_str_array_pairwise_distinct: family-wide \
                     &'static str array carries a duplicate entry \
                     across two positions — the substrate's pairwise-\
                     distinctness contract on the array is broken; \
                     every consumer that pattern-matches the array's \
                     entries as DISJOINT arms (bool-literal dispatch, \
                     atomic-kind label decode, quote-family prefix \
                     decode, iac-forge tag decode, quote-family label \
                     projection) relies on this invariant"
                );
            }
            j += 1;
        }
        i += 1;
    }
}

/// Const-fn byte-equality helper for `assert_str_array_pairwise_
/// distinct` — compares two `&'static str`s byte-for-byte through
/// their [`str::as_bytes`] projections. Lifted as a co-located
/// module-private helper rather than an inline sweep so the outer
/// helper's triangular `(i, j)` pair-walk mirrors the shape of
/// [`assert_char_array_pairwise_distinct`] at the outer method
/// axis without an inline byte-loop obscuring the `(i, j)` sweep.
///
/// The equality relation this helper computes is EXACTLY
/// [`str::eq`] (i.e. byte-for-byte length + content equality on
/// the underlying UTF-8 bytes), just re-derived in a const-eval
/// friendly shape since `str::eq` / `<[u8]>::eq` are not (yet)
/// callable from `const fn` context on the substrate's toolchain.
/// A future toolchain that stabilises `const fn str::eq` collapses
/// this helper to a one-line `str::eq(a, b)` delegation.
const fn str_bytes_equal(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut k = 0;
    while k < a.len() {
        if a[k] != b[k] {
            return false;
        }
        k += 1;
    }
    true
}

// Compile-time pairwise-distinctness witnesses — one `const _: () =
// assert_str_array_pairwise_distinct(&…)` per family-wide `[&'static
// str; N]` array on the substrate's closed-set outer algebras. Each
// invocation is const-evaluated at `cargo check` time; a regression
// that silently collides two entries fails the build rather than the
// test suite. Sibling to the runtime `_pairwise_distinct` tests at
// `ast.rs`'s tests module — the two enforce the same theorem at TWO
// stages of the toolchain, so a build that skips tests still catches
// the regression here, and a build that runs tests catches it a
// second time as a safety net if the const-eval sweep is ever
// silently dropped. Peer to the seven `assert_char_array_pairwise_
// distinct` witnesses above on the (element-type) axis: `char` covers
// the reader-boundary vocabulary; `&'static str` covers the closed-
// set outer-algebras' family-wide label / prefix / tag / literal
// vocabularies.
const _: () = assert_str_array_pairwise_distinct(&Atom::BOOL_LITERALS);
const _: () = assert_str_array_pairwise_distinct(&AtomKind::LABELS);
const _: () = assert_str_array_pairwise_distinct(&QuoteForm::PREFIXES);
const _: () = assert_str_array_pairwise_distinct(&QuoteForm::IAC_FORGE_TAGS);
const _: () = assert_str_array_pairwise_distinct(&QuoteForm::LABELS);

// Compile-time FULL-ARRAY per-position ORDER pins — one `const _: () =
// assert_str_array_slice_equals_str_array::<N, N, 0>(&…, &[literal strs;
// N])` per family-wide `[&'static str; N]` scalar-composed substrate
// array on the closed-set outer-algebras' label / prefix / tag / literal
// vocabularies. Each invocation exercises the
// [`assert_str_array_slice_equals_str_array`] helper at its FULL-ARRAY
// corner (`M == N`, `START == 0`) — the SLICE-EQUALS-ARRAY sweep
// collapses to the ALL-positions-equal-peer-array pointwise identity
// `arr == [s_0, s_1, …, s_{N-1}]`. The peer literal-str sub-array on
// the RHS pins BOTH (a) the per-position ORDER of the outer array's
// declaration (the CANONICAL variant-declaration order every
// closed-set outer-algebra consumer depends on) AND (b) each per-role
// `pub const *_LABEL` / `*_PREFIX` / `*_TAG` / `TRUE_LITERAL` /
// `FALSE_LITERAL` alias's canonical `&'static str` value the outer
// array's slots re-export. Strictly STRONGER on the (contract-strength)
// axis than the sibling `_pairwise_distinct` witnesses at lines
// 1173..=1177 above: those bind each array's IMAGE SET via INJECTIVITY
// but are SILENT on which SLOT each str lands at — a regression that
// swapped `Atom::TRUE_LITERAL` (`"#t"`) and `Atom::FALSE_LITERAL`
// (`"#f"`) (drifting `Atom::BOOL_LITERALS` from `["#t", "#f"]` to
// `["#f", "#t"]`) preserves the pairwise-distinctness witness (both
// strs still distinct) but silently misaligns every consumer indexing
// `BOOL_LITERALS[0]` for the canonical `true`-lexeme dispatch. A
// silent value-drift of a per-role scalar (e.g. an ELisp-compat
// rename of `AtomKind::SYMBOL_LABEL` from `"symbol"` to `"sym"`, a
// Racket-compat rename of `QuoteForm::QUASIQUOTE_LABEL`) that flows
// through the composed array without updating the outer array's
// literal image ALSO fails HERE where the pairwise-distinctness
// witness stays silent. Post-lift the ARRAY-LEVEL per-position order
// binds at rustc time via ONE `const _` witness per array; a drift
// at either the per-role `pub const *_LABEL` / `*_PREFIX` / `*_TAG`
// str OR the array declaration's ordering fails at `cargo check`
// BEFORE any test scheduler runs.
//
// Sibling posture to the EIGHT-witness (char)-row FULL-ARRAY per-
// position ORDER cluster at lines 893..=914 above (the eight
// `assert_char_array_slice_equals_char_array::<N, N, 0>(&…, &[literal
// chars; N])` witnesses on the substrate's `[char; N]` reader-boundary
// vocabulary) and the FOUR-witness (u8)-row FULL-ARRAY per-position
// ORDER cluster below in this file (four
// `assert_u8_array_slice_equals_u8_array::<N, N, 0>(&…, &[literal
// bytes; N])` witnesses on the sub-carving `[u8; N]`
// `HASH_DISCRIMINATORS` arrays). Together the eight (char)-row + four
// (u8)-row + these five (str)-row witnesses close the (element-type
// × contract-shape) matrix's FULL-ARRAY per-position ORDER column
// across ALL THREE scalar element-type rows (char, u8, &'static str)
// EXHAUSTIVELY at rustc time.
//
// The five pinned arrays appear here in canonical (owning-algebra,
// per-role-alias-count-ascending) order:
// * `Atom::BOOL_LITERALS == ["#t", "#f"]` — Scheme-canonical bool-
//   lexeme spellings on the `Atom` algebra (two per-role literals).
// * `AtomKind::LABELS == ["symbol", "keyword", "string", "int",
//   "float", "bool"]` — atomic-payload diagnostic labels on the
//   `AtomKind` subset algebra (six per-role labels).
// * `QuoteForm::PREFIXES == ["'", "`", ",", ",@"]` — quote-family
//   reader-punctuation prefix bytes on the `QuoteForm` algebra (four
//   per-role prefixes; the fourth `",@"` is the ONLY two-char prefix
//   on the closed set — pinned separately by
//   `quote_form_unquote_splice_prefix_constant_composes_from_unquote_lead_and_splice_discriminator`).
// * `QuoteForm::LABELS == ["quote", "quasiquote", "unquote",
//   "unquote-splice"]` — diagnostic labels on the `QuoteForm` algebra
//   (four per-role labels; the fourth `"unquote-splice"` is the
//   substrate's SHORTER diagnostic idiom for `LispError::TypeMismatch.got`
//   surfaces — INTENTIONALLY DISTINCT from the iac-forge tag's
//   Common-Lisp-canonical `"unquote-splicing"` spelling).
// * `QuoteForm::IAC_FORGE_TAGS == ["quote", "quasiquote", "unquote",
//   "unquote-splicing"]` — cross-crate canonical-form tags on the
//   `QuoteForm` algebra (four per-role tags; the fourth
//   `"unquote-splicing"` is Common-Lisp-canonical and distinct from
//   `QuoteForm::LABELS[3]`'s shorter `"unquote-splice"` spelling —
//   pinned by
//   `quote_form_iac_forge_tag_and_label_disagree_only_on_unquote_splice_arm`).
const _: () =
    assert_str_array_slice_equals_str_array::<2, 2, 0>(&Atom::BOOL_LITERALS, &["#t", "#f"]);
const _: () = assert_str_array_slice_equals_str_array::<6, 6, 0>(
    &AtomKind::LABELS,
    &["symbol", "keyword", "string", "int", "float", "bool"],
);
const _: () = assert_str_array_slice_equals_str_array::<4, 4, 0>(
    &QuoteForm::PREFIXES,
    &["'", "`", ",", ",@"],
);
const _: () = assert_str_array_slice_equals_str_array::<4, 4, 0>(
    &QuoteForm::LABELS,
    &["quote", "quasiquote", "unquote", "unquote-splice"],
);
const _: () = assert_str_array_slice_equals_str_array::<4, 4, 0>(
    &QuoteForm::IAC_FORGE_TAGS,
    &["quote", "quasiquote", "unquote", "unquote-splicing"],
);

/// Compile-time contract verifier — panics at const evaluation time if
/// any entry of `arr` has zero length under [`str::len`] (equivalently
/// `str::is_empty`).
///
/// Contract-orthogonal peer to [`assert_str_array_pairwise_distinct`]
/// on the (contract-shape) column of the (`&'static str`) row of the
/// (element-type × contract-shape) matrix: where the pairwise-
/// distinctness sibling binds INTRA-array `∀ i ≠ j : arr[i] ≠ arr[j]`
/// (INJECTIVITY on the multiset of entries), this NONEMPTY-CARDINALITY-
/// LOWER-BOUND sibling binds the strictly-weaker per-entry cardinality
/// gate `∀ i : arr[i].len() > 0` (NO ENTRY is the zero-length byte
/// sequence). The two contracts compose orthogonally: an array that
/// carries `["", ""]` fails the pairwise-distinctness contract at the
/// zero-length-pair corner (pinned by
/// `assert_str_array_pairwise_distinct_rejects_length_zero_collision`),
/// but an array that carries `["", "a"]` passes pairwise-distinctness
/// while failing NONEMPTY — so the NONEMPTY sibling closes the
/// remaining `""`-carrying corner that INJECTIVITY alone cannot pin.
/// The inner test is a direct [`str::is_empty`] delegation — const-
/// stable since Rust 1.39 and const-callable on the substrate's
/// toolchain — so no `str_bytes_equal`-shaped auxiliary helper is
/// needed for this contract (the peer `_pairwise_distinct` sibling
/// uses `str_bytes_equal` because it must compare TWO strings byte-
/// for-byte while `const fn str::eq` remains unstable; this sibling
/// only tests ONE string's length so it delegates directly).
///
/// The invariant is load-bearing for every consumer that spells a
/// closed-set variant through its `&'static str` label — every
/// [`AtomKind::label`] / [`QuoteForm::label`] / `parse_label` /
/// `find_by_label` composition on the ClosedSet trait's family-wide
/// label vocabularies (`Atom::BOOL_LITERALS` on the bool-literal
/// dispatch; `AtomKind::LABELS` on the atomic-payload kind decode;
/// `QuoteForm::LABELS` on the quote-family diagnostic vocabulary;
/// `QuoteForm::PREFIXES` on the reader-boundary prefix vocabulary;
/// `QuoteForm::IAC_FORGE_TAGS` on the canonical iac-forge tag
/// vocabulary) treats each entry as a NONEMPTY identifier and would
/// silently mis-behave on a `""` entry: `parse_label("")` would decode
/// the empty string to that variant (silently making empty user input a
/// valid variant spelling); `find_by_label("")` would return `Some(v)`
/// rather than `None`; every string-search consumer that scans for a
/// prefix or contains a label as a substring would spuriously match on
/// every input (since `""` is a prefix and a substring of every
/// string). Post-lift a regression that silently re-inlined one label
/// constant to `""` (e.g. `AtomKind::SYMBOL_LABEL = "";`) fails at
/// `cargo check` BEFORE any test scheduler runs.
///
/// Adding a new family-wide `[&'static str; N]` label / prefix / tag /
/// literal vocabulary to the substrate: pair the declaration with
/// `const _: () = assert_str_array_all_nonempty(&Self::FOO_ARRAY);`
/// co-located after the array's declaration and the NONEMPTY-CARDINALITY-
/// LOWER-BOUND contract binds at compile time. The rustc-forced arity
/// `[&'static str; N]` composes with this const-eval sweep so BOTH
/// cardinality AND per-entry nonempty are compile-time theorems on the
/// SAME array.
///
/// Runtime callability: the function is a normal `pub const fn`, so
/// callers CAN also invoke it at runtime — pinned by
/// `assert_str_array_all_nonempty_panics_at_runtime_on_head_empty` /
/// `_interior_empty` / `_tail_empty` and
/// `assert_str_array_all_nonempty_panic_message_names_the_helper`. The
/// panic site carries the `"STR-EMPTY-ENTRY"` axis-provenance string
/// chosen DISTINCT from every sibling helper's axis vocabulary
/// (`"duplicate"` on the pairwise-distinct sibling; `"STR-DISJOINTNESS-
/// VIOLATION"` on the arrays-disjoint sibling; `"STR-SUBSET-VIOLATION"`
/// on the within-finite-set sibling) so a diagnostic that names the
/// failed axis routes UNAMBIGUOUSLY to THIS specific NONEMPTY helper.
///
/// Theory grounding:
/// - THEORY.md §V.1 — knowable platform; the family-wide nonempty-
///   cardinality-lower-bound contract on the `&'static str` label
///   vocabulary becomes a TYPE-LEVEL theorem the substrate carries per
///   array declaration rather than a runtime test the developer must
///   remember to write per label constant.
/// - THEORY.md §II.1 invariant 1 — typed entry; a closed-set variant's
///   label projection is the entry-point discriminator into the typed
///   algebra, and a `""` entry would silently break the discriminator
///   at the boundary between untyped `&str` input and typed enum
///   variant.
/// - THEORY.md §VI.1 — generation over composition; the const-eval
///   sweep IS the generative shape. Every new closed-set label array
///   adds ONE `const _` line to get the NONEMPTY theorem rather than
///   re-deriving a per-array runtime iterator sweep at each call site.
pub const fn assert_str_array_all_nonempty<const N: usize>(arr: &[&'static str; N]) {
    let mut i = 0;
    while i < N {
        if arr[i].is_empty() {
            panic!(
                "assert_str_array_all_nonempty: STR-EMPTY-ENTRY — the \
                 family-wide &'static str array carries a zero-length \
                 entry at some position — the substrate's NONEMPTY-\
                 CARDINALITY-LOWER-BOUND contract on the array is \
                 broken; every consumer that spells a closed-set variant \
                 through its `&'static str` label (AtomKind / QuoteForm \
                 / UnquoteForm / StructuralKind / SexpShape label \
                 projections; the reader-boundary prefix / tag \
                 vocabularies; the bool-literal dispatch) treats each \
                 entry as a NONEMPTY identifier — `parse_label(\"\")` \
                 would silently decode the empty string to the offending \
                 variant, `find_by_label(\"\")` would return `Some(v)` \
                 rather than `None`, every substring / prefix scan would \
                 spuriously match on every input. Fix at the ARRAY-\
                 DECLARATION site by removing the `\"\"` entry OR by \
                 giving it a nonempty canonical spelling"
            );
        }
        i += 1;
    }
}

// Compile-time NONEMPTY-CARDINALITY-LOWER-BOUND witnesses — one
// `const _: () = assert_str_array_all_nonempty(&…)` per family-wide
// `[&'static str; N]` array on the substrate's closed-set outer
// algebras. Each invocation is const-evaluated at `cargo check` time; a
// regression that silently re-inlined one label constant to `""` fails
// the build rather than deferring to a per-consumer misbehavior at
// runtime. Sibling to the pairwise-distinctness witnesses above — those
// pin INJECTIVITY on each array, these pin the strictly-weaker per-
// entry cardinality gate on the SAME arrays. The two contracts compose
// orthogonally on every closed-set outer algebra's label vocabulary.
// The five arrays covered here mirror the five arrays already pinned
// by the `_pairwise_distinct` witnesses above (`Atom::BOOL_LITERALS`,
// `AtomKind::LABELS`, `QuoteForm::PREFIXES`, `QuoteForm::IAC_FORGE_TAGS`,
// `QuoteForm::LABELS`) — the (array × contract-shape) coverage matrix
// on the (`&'static str`) row of this file now holds at every
// (INJECTIVITY, NONEMPTY) corner for the five outer-algebra arrays
// declared here. Analogous witnesses on the (`&'static str`) arrays
// declared under `crate::error` (`CompilerSpecIoStage::LABELS`,
// `MacroDefHead::KEYWORDS`, `MacroParams::LAMBDA_LIST_KEYWORDS`,
// `UnquoteForm::MARKERS` / `IAC_FORGE_TAGS` / `LABELS`,
// `KwargPathKind::LABELS`, `ExpectedKwargShape::LABELS`,
// `SexpShape::LABELS`, `StructuralKind::LABELS`) land co-located with
// the pre-existing `_pairwise_distinct` witnesses at that file's
// module-level prelude.
const _: () = assert_str_array_all_nonempty(&Atom::BOOL_LITERALS);
const _: () = assert_str_array_all_nonempty(&AtomKind::LABELS);
const _: () = assert_str_array_all_nonempty(&QuoteForm::PREFIXES);
const _: () = assert_str_array_all_nonempty(&QuoteForm::IAC_FORGE_TAGS);
const _: () = assert_str_array_all_nonempty(&QuoteForm::LABELS);

/// Compile-time contract verifier — panics at const evaluation time if
/// any entry of `arr` carries a byte outside the seven-bit ASCII range
/// (`>= 0x80`, the first byte of any non-ASCII UTF-8 sequence).
///
/// Per-entry peer to [`assert_str_array_all_nonempty`] on the (per-
/// entry × contract-shape) axis of the (`&'static str`) row: where the
/// NONEMPTY sibling pins the length-lower-bound gate (`∀ i :
/// arr[i].len() > 0`), this ASCII sibling pins the byte-range
/// containment gate (`∀ i, ∀ b ∈ arr[i].as_bytes() : b <= 0x7F`). The
/// two contracts compose orthogonally — an array carrying `["café"]`
/// passes NONEMPTY while failing ASCII; an array carrying `["", "a"]`
/// passes ASCII while failing NONEMPTY. Together with the INJECTIVITY
/// sibling ([`assert_str_array_pairwise_distinct`]) the substrate's
/// (per-entry × contract-shape) coverage matrix on the (`&'static
/// str`) row closes at THREE corners: {NONEMPTY, ASCII, INJECTIVITY}
/// on the SAME five outer-algebra arrays declared here.
///
/// The invariant is load-bearing for every consumer that ships an
/// array entry through a downstream surface whose canonical form is
/// seven-bit-clean — Kubernetes annotation keys + label values (RFC
/// 1123 subset of ASCII); YAML flow-scalar map keys the reader-boundary
/// prefix vocabulary threads through the four-Lisp projection; BLAKE3
/// hash inputs on the three-pillar attestation chain (identical byte
/// sequences hash identically regardless of encoding, but authoring a
/// non-ASCII label silently invites U+FEFF BOMs and Unicode-normalization
/// drift on the wire); Rust `matches!(s, "quote" | …)` byte-pattern
/// arms the compiler lowers to `[u8]` comparison. Post-lift a
/// regression that silently re-inlined one label constant to a byte-
/// equivalent non-ASCII spelling (e.g. `AtomKind::SYMBOL_LABEL =
/// "sýmbol";`, a lookalike that would parse as a Rust `&'static str`
/// but ship non-ASCII bytes at every consumer) fails at `cargo check`
/// BEFORE any test scheduler runs.
///
/// Adding a new family-wide `[&'static str; N]` label / prefix / tag /
/// literal vocabulary whose canonical spelling is seven-bit-clean:
/// pair the declaration with `const _: () =
/// assert_str_array_all_ascii(&Self::FOO_ARRAY);` co-located after
/// the array's declaration and the ASCII-BYTE-RANGE contract binds at
/// compile time. The rustc-forced arity `[&'static str; N]` composes
/// with this const-eval sweep so BOTH cardinality AND per-entry ASCII
/// are compile-time theorems on the SAME array declaration.
///
/// Runtime callability: the function is a normal `pub const fn`, so
/// callers CAN also invoke it at runtime — pinned by
/// `assert_str_array_all_ascii_panics_at_runtime_on_head_non_ascii` /
/// `_interior_non_ascii` / `_tail_non_ascii` and
/// `assert_str_array_all_ascii_panic_message_names_the_helper_and_axis`.
/// The panic site carries the `"STR-NON-ASCII-ENTRY"` axis-provenance
/// string chosen DISTINCT from every sibling helper's axis vocabulary
/// (`"duplicate"` on the pairwise-distinct sibling; `"STR-EMPTY-
/// ENTRY"` on the per-entry NONEMPTY sibling; `"STR-DISJOINTNESS-
/// VIOLATION"` on the arrays-disjoint sibling; `"STR-SUBSET-
/// VIOLATION"` on the within-finite-set sibling) so a diagnostic that
/// names the failed axis routes UNAMBIGUOUSLY to THIS specific ASCII
/// helper.
///
/// Theory grounding:
/// - THEORY.md §V.1 — knowable platform; the family-wide per-entry
///   ASCII-byte-range contract on the substrate's `&'static str`
///   label vocabulary becomes a TYPE-LEVEL theorem the substrate
///   carries per array declaration rather than a runtime test the
///   developer must remember to write per label constant.
/// - THEORY.md §II.1 invariant 1 — typed entry; a closed-set variant's
///   label projection is the entry-point discriminator into the typed
///   algebra, and a non-ASCII byte in that projection silently
///   escapes the seven-bit-clean assumption every downstream wire
///   surface encodes into its own byte-level parser.
/// - THEORY.md §VI.1 — generation over composition; the const-eval
///   byte-range sweep IS the generative shape. Every new closed-set
///   label array adds ONE `const _` line to get the ASCII theorem
///   rather than re-deriving a per-array runtime iterator sweep at
///   each call site.
pub const fn assert_str_array_all_ascii<const N: usize>(arr: &[&'static str; N]) {
    let mut i = 0;
    while i < N {
        let bytes = arr[i].as_bytes();
        let mut j = 0;
        while j < bytes.len() {
            if bytes[j] > 0x7F {
                panic!(
                    "assert_str_array_all_ascii: STR-NON-ASCII-ENTRY — \
                     the family-wide &'static str array carries an \
                     entry with a byte outside the seven-bit ASCII \
                     range (>= 0x80) at some position — the \
                     substrate's ASCII-BYTE-RANGE contract on the \
                     array is broken; every consumer that ships an \
                     entry through a seven-bit-clean downstream \
                     surface (K8s annotation keys + label values; \
                     YAML flow-scalar map keys; BLAKE3 hash inputs on \
                     the three-pillar attestation chain; Rust \
                     `matches!(s, ...)` byte-pattern arms) treats \
                     each entry as ASCII — a non-ASCII byte silently \
                     invites Unicode-normalization drift on the wire, \
                     BOM injection, and lookalike-label collisions \
                     that byte-equality parsing cannot detect. Fix at \
                     the ARRAY-DECLARATION site by re-inlining the \
                     offending label constant to its seven-bit-clean \
                     canonical spelling"
                );
            }
            j += 1;
        }
        i += 1;
    }
}

// Compile-time ASCII-BYTE-RANGE witnesses — one `const _: () =
// assert_str_array_all_ascii(&…)` per family-wide `[&'static str; N]`
// array on the substrate's closed-set outer algebras. Each invocation
// is const-evaluated at `cargo check` time; a regression that
// silently re-inlined one label constant to a lookalike non-ASCII
// spelling fails the build rather than deferring to a per-consumer
// byte-parse misbehavior at runtime. Sibling to the NONEMPTY witnesses
// above — those pin the per-entry length-lower-bound gate on each
// array, these pin the strictly-orthogonal per-entry byte-range gate
// on the SAME arrays. The two contracts compose orthogonally on every
// closed-set outer algebra's label vocabulary. The five arrays covered
// here mirror the five arrays already pinned by the `_pairwise_distinct`
// AND `_all_nonempty` witnesses above — the (per-entry × contract-shape)
// coverage matrix on the (`&'static str`) row of this file now holds
// at THREE corners {NONEMPTY, ASCII, INJECTIVITY} for the five outer-
// algebra arrays declared here. Analogous witnesses on the (`&'static
// str`) arrays declared under `crate::error` land co-located with the
// pre-existing `_pairwise_distinct` + `_all_nonempty` witnesses at
// that file's module-level prelude.
const _: () = assert_str_array_all_ascii(&Atom::BOOL_LITERALS);
const _: () = assert_str_array_all_ascii(&AtomKind::LABELS);
const _: () = assert_str_array_all_ascii(&QuoteForm::PREFIXES);
const _: () = assert_str_array_all_ascii(&QuoteForm::IAC_FORGE_TAGS);
const _: () = assert_str_array_all_ascii(&QuoteForm::LABELS);

/// Compile-time contract verifier — panics at const evaluation time if
/// any entry of `a` aliases any entry of `b` byte-for-byte through
/// [`str::as_bytes`].
///
/// Row-dual peer to [`assert_char_arrays_disjoint`] and
/// [`assert_u8_arrays_disjoint`] on the (element-type) axis of the
/// (element-type × contract-shape) matrix at the (disjointness)
/// column: where the (char) sibling closes the reader-boundary char
/// DISJOINTNESS corner and the (u8) sibling closes the outer-`Sexp`
/// cache-key `u8` DISJOINTNESS corner at compile time, this (`&'static
/// str`) sibling closes the outer-algebras' family-wide `[&'static
/// str; N]` label / prefix / tag / literal DISJOINTNESS corner on the
/// SAME contract-shape column. Together with the pre-existing
/// [`assert_char_arrays_disjoint`] + [`assert_u8_arrays_disjoint`]
/// row-siblings the three helpers close the (element-type ∈
/// {char, u8, &'static str} × contract-shape ∈ {disjointness})
/// 3-corner row of the DISJOINTNESS column at ONE peer const-fn helper
/// per element-type. Contract-orthogonal peer to
/// [`assert_str_array_pairwise_distinct`] on the (INJECTIVITY,
/// DISJOINTNESS) axis of the (contract-shape) column on the SAME
/// (`&'static str`) row: where the pairwise-distinctness sibling binds
/// INTRA-array `∀ i ≠ j : arr[i] ≠ arr[j]`, this DISJOINTNESS sibling
/// binds INTER-array `∀ i, j : a[i] ≠ b[j]` — the two together give
/// every `&'static str` sub-vocabulary on the substrate BOTH intra-
/// array injectivity AND inter-array disjointness at compile time.
///
/// The invariant is load-bearing for every consumer that partitions
/// the two arrays' distinct-values sets into disjoint sub-vocabularies
/// of a shared outer surface —
/// [`QuoteForm::PREFIXES`] (`["'", "\`", ",", ",@"]` — reader-boundary
/// prefix tokens the tokenizer scans in [`crate::reader::tokenize`])
/// is intentionally-closed disjoint from
/// [`QuoteForm::LABELS`] (`["quote", "quasiquote", "unquote",
/// "unquote-splice"]` — human-diagnostic labels the error module
/// projects through [`QuoteForm::label`]) so a reader-boundary token
/// never aliases a diagnostic label spelling; the SAME `QuoteForm::
/// PREFIXES` array is disjoint from
/// [`QuoteForm::IAC_FORGE_TAGS`] (`["quote", "quasiquote", "unquote",
/// "unquote-splicing"]` — canonical iac-forge interop symbol heads the
/// [`crate::interop`] round-trip pins through
/// [`QuoteForm::from_iac_forge_tag`]) so the reader-boundary vocabulary
/// stays clean of the canonical-form serialization vocabulary; the
/// SAME `QuoteForm::PREFIXES` array is disjoint from
/// [`AtomKind::LABELS`] (`["symbol", "keyword", "string", "int",
/// "float", "bool"]` — atomic-payload kind labels) so a reader-boundary
/// prefix never aliases an atom-kind diagnostic label; AND
/// [`AtomKind::LABELS`] is intentionally-closed disjoint from
/// [`QuoteForm::LABELS`] so a diagnostic that identifies "the token is
/// an atom of kind X" never aliases "the token is a quote form of
/// kind Y". Every future `[&'static str; N]` pair on the substrate
/// whose distinct-values sets must remain disjoint sub-vocabularies of
/// a shared outer surface participates in the SAME compile-time
/// guarantee via one `const _` line per pair.
///
/// Pre-lift the four disjointness relations lived only as runtime
/// tests (`quote_form_prefixes_disjoint_from_quote_form_labels`,
/// `quote_form_prefixes_disjoint_from_iac_forge_tags`,
/// `quote_form_prefixes_disjoint_from_atom_kind_labels`,
/// `atom_kind_labels_disjoint_from_quote_form_labels`) or implicitly
/// through the outer-algebra's non-aliasing composition rule; post-
/// lift the ARRAY-LEVEL disjointness of the four pinned pairs binds at
/// rustc time via one `const _` line per pair. A regression that
/// silently renamed one of `QuoteForm::PREFIXES`'s entries to a string
/// that aliased a `QuoteForm::LABELS` / `QuoteForm::IAC_FORGE_TAGS` /
/// `AtomKind::LABELS` entry (or vice versa) fails at `cargo check`
/// BEFORE any test scheduler runs.
///
/// Adding a new `[&'static str; N]` sub-vocabulary whose distinct-
/// values set must remain disjoint from another substrate `[&'static
/// str; M]` array's distinct-values set: pair the declaration with
/// `const _: () = assert_str_arrays_disjoint::<N, M>(&Self::FOO_ARRAY,
/// &Other::BAR_ARRAY);` co-located after the array's declaration and
/// the DISJOINTNESS contract binds at compile time. The rustc-forced
/// arities `[&'static str; N]` and `[&'static str; M]` compose with
/// this const-eval sweep so BOTH cardinality-pair AND cross-array
/// disjointness are compile-time theorems on the SAME (a, b) str-array
/// pair.
///
/// Runtime callability: the function is a normal `pub const fn`, so
/// callers CAN also invoke it at runtime — pinned by
/// `assert_str_arrays_disjoint_panics_at_runtime_on_collision` and
/// `assert_str_arrays_disjoint_panic_message_names_the_helper_and_str_disjointness_violation_axis`.
/// The panic site carries the `"STR-DISJOINTNESS-VIOLATION"` axis-
/// provenance string chosen DISTINCT from every sibling helper's axis
/// vocabulary (`"duplicate"` on the ARRAY-side pairwise-distinct
/// sibling; `"CHAR-DISJOINTNESS-VIOLATION"` on the (char) row-dual
/// DISJOINTNESS sibling; `"U8-DISJOINTNESS-VIOLATION"` on the (u8)
/// row-dual DISJOINTNESS sibling; `"CHAR-SUBSET-VIOLATION"` on the
/// (char) SUBSET-embedding sibling; `"SUBSET-VIOLATION"` on the (u8)
/// finite-set SUBSET-only sibling; `"RANGE-SUBSET-VIOLATION"` on the
/// (u8) range SUBSET-only sibling; `"OUT-OF-SET"` / `"SET-BYTE-
/// MISSING"` on the (u8) covers-finite-set sibling; `"OUT-OF-RANGE"` /
/// `"MISSING"` on the (u8) covers-inclusive-range sibling; `"ARITY-
/// MISMATCH"` on both (u8) `_permutes_*` compound helpers; `"SET-NOT-
/// PAIRWISE-DISTINCT"` on the (u8) SET-side well-formedness sibling)
/// so a diagnostic that names the failed axis routes UNAMBIGUOUSLY to
/// THIS specific `&'static str` DISJOINTNESS helper. The `"STR-"`
/// prefix disambiguates from the (char) + (u8) row-dual DISJOINTNESS
/// siblings; the shared `"-DISJOINTNESS-VIOLATION"` suffix lets
/// callers grep any row's DISJOINTNESS sibling by
/// `"DISJOINTNESS-VIOLATION"` alone.
///
/// Byte-equality reuse: the helper delegates to the same module-
/// private [`str_bytes_equal`] const-fn helper the sibling
/// [`assert_str_array_pairwise_distinct`] uses — a single canonical
/// site for `&'static str` byte-equality in const context, so a future
/// toolchain stabilising `const fn str::eq` collapses BOTH callers at
/// ONE edit rather than two.
///
/// Theory grounding:
/// - THEORY.md §V.1 — knowable platform; the family-wide cross-array
///   disjointness contract on the substrate's `&'static str`
///   vocabulary becomes a TYPE-LEVEL theorem the substrate carries per
///   (a, b) str-array pair rather than a runtime test the developer
///   must remember to write per pair.
/// - THEORY.md §III — the typescape; the (element-type × contract-
///   shape) matrix now carries the DISJOINTNESS corner on THREE rows
///   ({char, u8, `&'static str`}) at ONE peer const-fn helper per row.
///   The (element-type ∈ {char, u8, `&'static str`}) × (contract-shape
///   ∈ {pairwise-distinctness (INJECTIVITY), disjointness}) 3×2 =
///   6-corner face on the array-pair-contract prism is now closed at
///   SIX peer const-fn helpers.
/// - THEORY.md §VI.1 — generation over composition; the const-eval
///   cross-array byte-membership sweep IS the generative shape. Every
///   new `[&'static str; N]` sub-vocabulary array whose distinct-
///   values set is an intentionally-disjoint peer of another substrate
///   `[&'static str; M]` array adds ONE `const _` line to get the
///   disjointness theorem rather than re-deriving a per-pair runtime
///   iterator sweep at each call site.
/// - THEORY.md §II.1 invariant 5 — composition preserves proofs; the
///   DISJOINTNESS proof at declaration site AND the outer-algebra's
///   arm-set partition contract (`QuoteForm::PREFIXES` on the reader-
///   boundary token surface vs. `QuoteForm::LABELS` on the human-
///   diagnostic label surface, etc.) regenerate through the SAME
///   `const _` witnesses at the ARRAY level.
///
/// Frontier inspiration: Lean 4's `Finset.disjoint_iff` unfolded to
/// `∀ a ∈ s, ∀ b ∈ t, a ≠ b` at the concrete two-array
/// `[&'static str; N] × [&'static str; M]` monomorphic realisation.
/// The (char, u8, `&'static str`) row triple mirrors Lean's
/// polymorphic `[DecidableEq α] → Finset α → Finset α → Prop`
/// realised at the three concrete element-type instantiations the
/// substrate closes at compile time.
pub const fn assert_str_arrays_disjoint<const N: usize, const M: usize>(
    a: &[&'static str; N],
    b: &[&'static str; M],
) {
    let mut i = 0;
    while i < N {
        let mut j = 0;
        while j < M {
            if str_bytes_equal(a[i], b[j]) {
                panic!(
                    "assert_str_arrays_disjoint: STR-DISJOINTNESS-\
                     VIOLATION — the two family-wide &'static str \
                     arrays `a` and `b` share an entry at some (i, j) \
                     position pair. The substrate's CROSS-ARRAY \
                     DISJOINTNESS contract on the pair is broken; \
                     every consumer that partitions the two arrays' \
                     distinct-values sets into disjoint sub-\
                     vocabularies of a shared outer surface (the \
                     reader-boundary prefix vocabulary at \
                     `QuoteForm::PREFIXES` vs the human-diagnostic \
                     label vocabulary at `QuoteForm::LABELS`; the \
                     reader-boundary prefix vocabulary vs the \
                     canonical iac-forge tag vocabulary at \
                     `QuoteForm::IAC_FORGE_TAGS`; the reader-boundary \
                     prefix vocabulary vs the atomic-kind label \
                     vocabulary at `AtomKind::LABELS`; the atomic-kind \
                     label vocabulary vs the quote-family label \
                     vocabulary; any future typed-disjointness pair on \
                     the substrate's `&'static str` sub-vocabularies) \
                     relies on the two arrays' distinct-values sets \
                     NOT sharing an entry. Fix at WHICHEVER ARRAY-\
                     DECLARATION site drifted (the symmetric \
                     disjointness relation carries no built-in axis-\
                     provenance role split between `a` and `b`) by \
                     renaming the offending entry on one array OR re-\
                     shaping the partition to route the shared entry \
                     to a single sub-vocabulary"
                );
            }
            j += 1;
        }
        i += 1;
    }
}

// Compile-time DISJOINTNESS witnesses — the FOUR substrate-pinned
// (a, b) `[&'static str; N] × [&'static str; M]` pairs whose distinct-
// values sets are intentionally-closed disjoint sub-vocabularies of a
// shared outer surface. Pre-lift the four disjointness relations lived
// only as runtime tests (or implicitly through the outer-algebra's
// non-aliasing composition rule); post-lift the ARRAY-LEVEL
// disjointness of the four pinned pairs binds at rustc time via one
// `const _` line per pair. A regression that silently renamed
// `QuoteForm::QUOTE_PREFIX` (`"'"`) to `"quote"` (aliasing
// `QuoteForm::QUOTE_LABEL` and `QuoteForm::QUOTE_IAC_FORGE_TAG`),
// renamed `AtomKind::SYMBOL_LABEL` (`"symbol"`) to `"quote"` (aliasing
// `QuoteForm::QUOTE_LABEL`), or drifted any entry of one array to
// bytes shared with an entry of a disjoint peer array fails at
// `cargo check` BEFORE any test scheduler runs. Sibling to the FIVE
// `assert_char_arrays_disjoint` witnesses and the TWO
// `assert_u8_arrays_disjoint` witnesses above on the (element-type)
// axis: `char` covers the reader-boundary char sub-vocabularies;
// `u8` covers the outer-`Sexp` cache-key discriminator sub-
// vocabularies; `&'static str` covers the outer-algebras' family-wide
// label / prefix / tag vocabularies.
//
// The four pinned pairs are (all under the shared `&'static str`
// element-type):
//   1. `QuoteForm::PREFIXES` ∩ `QuoteForm::LABELS`         = ∅
//   2. `QuoteForm::PREFIXES` ∩ `QuoteForm::IAC_FORGE_TAGS` = ∅
//   3. `QuoteForm::PREFIXES` ∩ `AtomKind::LABELS`          = ∅
//   4. `AtomKind::LABELS`    ∩ `QuoteForm::LABELS`         = ∅
//
// The TWO remaining disjointness pairs on the twelve-arm
// `SexpShape::LABELS` partition triple
// (`AtomKind::LABELS`, `QuoteForm::LABELS`, `StructuralKind::LABELS`)
// — namely (`AtomKind::LABELS`, `StructuralKind::LABELS`) and
// (`QuoteForm::LABELS`, `StructuralKind::LABELS`) — are pinned as
// peer `const _` lines in `error.rs` (the file where the
// `StructuralKind` host type lives). Split by host-file, unified in
// theorem: together with pair 4 above they cover every pair on the
// three-element sub-vocabulary triple (C(3, 2) = 3), closing the
// DISJOINT-UNION proof
//   `SexpShape::LABELS ≡ AtomKind::LABELS ⊕ QuoteForm::LABELS ⊕
//    StructuralKind::LABELS`
// at compile time.
//
// Note on the intentionally-NOT-pinned pair
// (`QuoteForm::LABELS`, `QuoteForm::IAC_FORGE_TAGS`): the two
// deliberately OVERLAP on three of four arms (`"quote"`, `"quasiquote"`,
// `"unquote"`) because both surfaces spell those three quote-family
// heads with the Common-Lisp-canonical name — `QuoteForm::LABELS` for
// diagnostics, `QuoteForm::IAC_FORGE_TAGS` for canonical serialization.
// Only the fourth arm differs (`QuoteForm::UNQUOTE_SPLICE_LABEL` at
// `"unquote-splice"` vs `QuoteForm::UNQUOTE_SPLICE_IAC_FORGE_TAG` at
// `"unquote-splicing"`) so the pair is NOT disjoint and would fail
// this witness. Pinning it here would be a category error — the
// overlap is load-bearing, not accidental.
const _: () = assert_str_arrays_disjoint::<4, 4>(&QuoteForm::PREFIXES, &QuoteForm::LABELS);
const _: () = assert_str_arrays_disjoint::<4, 4>(&QuoteForm::PREFIXES, &QuoteForm::IAC_FORGE_TAGS);
const _: () = assert_str_arrays_disjoint::<4, 6>(&QuoteForm::PREFIXES, &AtomKind::LABELS);
const _: () = assert_str_arrays_disjoint::<6, 4>(&AtomKind::LABELS, &QuoteForm::LABELS);

/// Compile-time contract verifier — panics at const evaluation time if
/// any entry of `arr` is NOT a member of `set` (byte-for-byte through
/// [`str::as_bytes`]).
///
/// Row-dual peer to [`assert_char_array_within_char_finite_set`] and
/// [`assert_u8_array_within_u8_finite_set`] on the (element-type) axis
/// of the (element-type × contract-shape) matrix at the (subset-
/// embedding) column: where the (char) sibling closes the reader-
/// boundary `[char; N] ⊆ [char; M]` corner and the (u8) sibling closes
/// the outer-`Sexp` cache-key `[u8; N] ⊆ [u8; M]` finite-set embedding
/// corner at compile time, this (`&'static str`) sibling closes the
/// outer-algebras' family-wide `[&'static str; N] ⊆ [&'static str; M]`
/// label / prefix / tag SUB-VOCABULARY carving corner. Together with
/// the pre-existing [`assert_char_array_within_char_finite_set`] +
/// [`assert_u8_array_within_u8_finite_set`] row-siblings the three
/// helpers close the (element-type ∈ {char, u8, &'static str} ×
/// contract-shape ∈ {subset-embedding}) 3-corner row of the SUBSET-
/// EMBEDDING column at ONE peer const-fn helper per element-type.
/// Contract-orthogonal peer to [`assert_str_array_pairwise_distinct`]
/// and [`assert_str_arrays_disjoint`] on the (INJECTIVITY,
/// DISJOINTNESS, SUBSET-EMBEDDING) axis of the (contract-shape) column
/// on the SAME (`&'static str`) row: where the pairwise-distinctness
/// sibling binds INTRA-array `∀ i ≠ j : arr[i] ≠ arr[j]` and the
/// disjointness sibling binds INTER-array `∀ i, j : a[i] ≠ b[j]`, this
/// SUBSET-EMBEDDING sibling binds ORIENTED-INTER-array `∀ i : ∃ j :
/// arr[i] = set[j]` — the three together give every `&'static str`
/// sub-vocabulary on the substrate INTRA-array injectivity AND INTER-
/// array disjointness (symmetric) AND INTER-array subset embedding
/// (oriented) at compile time.
///
/// The invariant is load-bearing for every consumer that carves a
/// SUB-vocabulary of a shared OUTER `&'static str` vocabulary:
/// [`AtomKind::LABELS`] (`[&; 6]`, `["symbol", "keyword", "string",
/// "int", "float", "bool"]` — the atomic-payload kind labels) is an
/// intentionally-closed proper subset of
/// [`crate::error::SexpShape::LABELS`] (`[&; 12]`, the twelve outer-
/// `Sexp` shape labels; six atomic + two structural + four quote-
/// family) so every atom-kind diagnostic label stays inside the outer-
/// shape label vocabulary; [`QuoteForm::LABELS`] (`[&; 4]`, `["quote",
/// "quasiquote", "unquote", "unquote-splice"]` — the quote-family
/// labels) is likewise a proper subset of the SAME
/// [`crate::error::SexpShape::LABELS`] so every quote-family diagnostic
/// stays inside the outer-shape vocabulary; and
/// [`crate::error::StructuralKind::LABELS`] (`[&; 2]`, `["nil",
/// "list"]` — the structural-shape labels) is the third proper subset
/// of the SAME twelve-arm superset. Union together `AtomKind::LABELS`
/// (6) + `QuoteForm::LABELS` (4) + `StructuralKind::LABELS` (2) = 12
/// = `SexpShape::LABELS.len()` closes a NON-CONTIGUOUS PARTITION of
/// the outer twelve-arm vocabulary at compile time; the three
/// SUBSET-EMBEDDING witnesses PLUS the pre-existing pairwise-
/// distinctness witnesses PLUS the pre-existing (AtomKind, QuoteForm)
/// disjointness witness compose to a full partition proof. Every
/// future `[&'static str; N]` sub-vocabulary on the substrate whose
/// distinct-values set must remain embedded in a shared outer surface
/// (a new tokenizer keyword vocabulary embedded in an outer prefix
/// vocabulary; a new diagnostic label vocabulary embedded in an outer
/// error-family label vocabulary; a new algebra whose display / label
/// / prefix arrays must remain sub-vocabularies of the closed-set
/// outer algebras' `&'static str` surface) gets the subset-embedding
/// theorem at ONE `const _` line rather than a per-pair runtime
/// iterator sweep.
///
/// SET-side well-formedness is DELEGATED to the sibling ARRAY-side
/// pairwise-distinctness helper ([`assert_str_array_pairwise_distinct`])
/// via a co-located call at the TOP of the sweep — a malformed `set`
/// (e.g. `["a", "a", "b"]`) is NOT a well-formed finite set of
/// cardinality `M` and silently mis-verifies the intended subset
/// contract on any `arr` embedded in the DISTINCT-value subset. The
/// delegated arm routes drift on the CALLER'S TARGET-SET SPEC to the
/// SET-side well-formedness axis rather than to a downstream STR-
/// SUBSET-VIOLATION symptom on `arr`. A well-formed `set` passes this
/// arm as a no-op — the sweep is const-eval-elidable and costs zero
/// at rustc-time on the substrate call sites. The (str) row does NOT
/// carry a separate `assert_str_finite_set_pairwise_distinct` alias
/// (the (u8) row's `assert_u8_finite_set_pairwise_distinct` is the
/// only SET-side well-formedness peer on the substrate); the
/// delegation reuses the ARRAY-side helper directly, matching the
/// (char) row's (`assert_char_array_within_char_finite_set` →
/// `assert_char_array_pairwise_distinct`) delegation shape.
///
/// Pre-lift the three subset embeddings lived as prose in the parent
/// arrays' partition-rule docstrings (the `SexpShape::LABELS` twelve-
/// arm decomposition into atomic / structural / quote-family sub-
/// vocabularies) and as runtime `_pairwise_distinct` cross-checks on
/// the tests submodule — the ARRAY-LEVEL embedding was not itself
/// pinned. Post-lift the three witnesses bind at rustc time via one
/// `const _` line per pair; a regression that silently re-inlined
/// either the SUBSET side (dropping `AtomKind::SYMBOL_LABEL`'s
/// `SexpShape::SYMBOL_LABEL` alias to a fresh distinct byte spelling)
/// or the SUPERSET side (dropping `SexpShape::SYMBOL_LABEL` and
/// leaving `AtomKind::SYMBOL_LABEL` as a stale copy of `"symbol"`)
/// fails at `cargo check` BEFORE any test scheduler runs.
///
/// Adding a new family-wide `[&'static str; N]` sub-vocabulary whose
/// distinct-values set must remain embedded in another substrate
/// `[&'static str; M]` array's distinct-values set: pair the
/// declaration with `const _: () = assert_str_array_within_str_finite_
/// set::<N, M>(&Self::FOO_ARRAY, &Other::BAR_ARRAY);` co-located after
/// the array's declaration and the SUBSET-EMBEDDING contract binds at
/// compile time. The rustc-forced arities `[&'static str; N]` and
/// `[&'static str; M]` compose with this const-eval sweep so BOTH
/// cardinality-pair AND cross-array subset embedding are compile-time
/// theorems on the SAME (arr, set) str-array pair.
///
/// Delegates to the existing module-private [`str_bytes_equal`] const-
/// fn helper so a future toolchain stabilising `const fn str::eq`
/// collapses ALL THREE (str)-row helpers ((str, pairwise-distinct),
/// (str, disjointness), (str, subset-embedding)) through ONE edit.
///
/// Runtime callability: the function is a normal `pub const fn`, so
/// callers CAN also invoke it at runtime — pinned by
/// `assert_str_array_within_str_finite_set_panics_at_runtime_on_out_of_set_entry`
/// and
/// `assert_str_array_within_str_finite_set_panic_message_names_the_helper_and_str_subset_violation_axis`.
/// The panic site carries the `"STR-SUBSET-VIOLATION"` axis-provenance
/// string chosen DISTINCT from every sibling helper's axis vocabulary
/// (`"duplicate"` on the ARRAY-side pairwise-distinct sibling; `"STR-
/// DISJOINTNESS-VIOLATION"` on the (str) row DISJOINTNESS sibling;
/// `"CHAR-SUBSET-VIOLATION"` on the (char) row-dual SUBSET sibling;
/// `"SUBSET-VIOLATION"` on the (u8) row-dual finite-set SUBSET-only
/// sibling; `"RANGE-SUBSET-VIOLATION"` on the (u8) range SUBSET-only
/// sibling; `"CHAR-DISJOINTNESS-VIOLATION"` / `"U8-DISJOINTNESS-
/// VIOLATION"` on the (char) / (u8) row-dual DISJOINTNESS siblings;
/// `"OUT-OF-SET"` / `"SET-BYTE-MISSING"` on the (u8) covers-finite-
/// set sibling; `"OUT-OF-RANGE"` / `"MISSING"` on the (u8) covers-
/// inclusive-range sibling; `"ARITY-MISMATCH"` on both (u8)
/// `_permutes_*` compound helpers; `"SET-NOT-PAIRWISE-DISTINCT"` on
/// the (u8) SET-side well-formedness sibling) so a diagnostic that
/// names the failed axis routes UNAMBIGUOUSLY to (a) this specific
/// `&'static str` SUBSET-embedding helper, (b) the `arr` argument as
/// the drift site rather than the `set` argument specifying the
/// target superset. The `"STR-"` prefix disambiguates from the
/// (char) + (u8) row-dual peers; the shared `"-SUBSET-VIOLATION"`
/// suffix lets callers grep any row's SUBSET-embedding sibling by
/// the shared `"SUBSET-VIOLATION"` suffix alone.
///
/// Theory grounding:
/// - THEORY.md §V.1 — knowable platform; the family-wide cross-array
///   subset-embedding contract on `&'static str` sub-vocabularies
///   becomes a TYPE-LEVEL theorem the substrate carries per (arr,
///   set) str-array pair rather than a runtime test the developer
///   must remember to write per pair.
/// - THEORY.md §III — the typescape; the (element-type × contract-
///   shape) matrix now carries the SUBSET-EMBEDDING corner on THREE
///   rows at ONE peer const-fn helper per row. The (subset,
///   disjointness) 2-corner face on the (`&'static str`) row is now
///   closed at TWO peer const-fn helpers —
///   [`assert_str_arrays_disjoint`] on the DISJOINTNESS corner and
///   this helper on the SUBSET corner.
/// - THEORY.md §VI.1 — generation over composition; the const-eval
///   cross-array membership sweep IS the generative shape. Every new
///   closed-set `&'static str` sub-vocabulary array whose distinct-
///   values set is an intentionally-embedded proper subset of another
///   substrate `&'static str` array adds ONE `const _` line to get
///   the subset-embedding theorem rather than re-deriving a per-pair
///   runtime iterator sweep at each call site.
/// - THEORY.md §II.1 invariant 5 — composition preserves proofs; the
///   SUBSET-EMBEDDING proof at declaration site AND the outer-
///   algebra's twelve-arm shape-label partition contract regenerate
///   through the SAME `const _` witnesses at the ARRAY level.
///
/// Frontier inspiration: Lean 4's `Finset.subset_iff : s ⊆ t ↔ ∀ a ∈
/// s, a ∈ t` unfolded at the concrete `[&'static str; N] ⊆ [&'static
/// str; M]` monomorphic realisation — the substrate primitive here
/// embeds the same subset relation as a rustc const-eval-time proof
/// obligation at every `assert_str_array_within_str_finite_set` call
/// site rather than as a Lean tactic invocation deferred to
/// `elab_command`. The (char, u8, `&'static str`) row triple mirrors
/// Lean's polymorphic `[DecidableEq α] → Finset α → Finset α → Prop`
/// realised at three concrete element-type instantiations the
/// substrate closes at compile time.
pub const fn assert_str_array_within_str_finite_set<const N: usize, const M: usize>(
    arr: &[&'static str; N],
    set: &[&'static str; M],
) {
    // Delegate target-set well-formedness to the sibling ARRAY-side
    // pairwise-distinctness helper FIRST. Placed BEFORE the STR-
    // SUBSET-VIOLATION sweep below because a malformed `set` (e.g.
    // `["a", "a", "b"]`) is not a well-formed finite set of
    // cardinality `M` and silently mis-verifies the intended subset
    // contract on any `arr` embedded in the DISTINCT-value subset.
    // Routes drift on the CALLER'S TARGET-SET SPEC to the SET-side
    // well-formedness axis (via the sibling's own panic-name prefix)
    // rather than to a downstream STR-SUBSET-VIOLATION symptom on
    // `arr`. A well-formed `set` passes this arm as a no-op — the
    // sweep is const-eval-elidable and costs zero at rustc-time on
    // the substrate call sites.
    assert_str_array_pairwise_distinct(set);
    let mut i = 0;
    while i < N {
        let mut j = 0;
        let mut found = false;
        while j < M {
            if str_bytes_equal(arr[i], set[j]) {
                found = true;
                break;
            }
            j += 1;
        }
        if !found {
            panic!(
                "assert_str_array_within_str_finite_set: STR-SUBSET-\
                 VIOLATION — the family-wide &'static str array `arr` \
                 carries an entry at some position whose bytes are \
                 NOT a member of the target finite superset partition \
                 `set`. The substrate's SUBSET-EMBEDDING contract on \
                 the array is broken; every consumer that expects the \
                 array's distinct-value set to be a subset of the \
                 target finite partition (`AtomKind::LABELS ⊂ \
                 SexpShape::LABELS` on the atomic-payload sub-\
                 vocabulary carve of the twelve-arm outer-shape label \
                 vocabulary; `QuoteForm::LABELS ⊂ SexpShape::LABELS` \
                 on the quote-family sub-vocabulary carve of the SAME \
                 twelve-arm outer-shape label vocabulary; \
                 `StructuralKind::LABELS ⊂ SexpShape::LABELS` on the \
                 structural sub-vocabulary carve of the SAME twelve-\
                 arm outer-shape label vocabulary; any future typed-\
                 subset embedding on the substrate's `&'static str` \
                 sub-vocabularies) relies on every array entry \
                 staying within the target superset. Fix at the \
                 ARRAY-DECLARATION site (the `arr` under \
                 verification, NOT the `set` argument specifying the \
                 target superset) by dropping the offending entry OR \
                 by extending `set` to cover it — the choice depends \
                 on whether the drift is an unintended overshoot \
                 outside the parent superset or an intentional \
                 extension of the superset vocabulary"
            );
        }
        i += 1;
    }
}

/// Compile-time contract verifier — panics at const evaluation time if
/// any entry of the target finite `set` is NOT reached by at least one
/// entry across the three sub-vocabulary arrays `a`, `b`, `c`.
///
/// SURJECTIVITY dual of [`assert_str_array_within_str_finite_set`] at
/// the (`&'static str`) row of the (element-type × contract-shape)
/// matrix, extended to the 3-array-union carrier shape: where the
/// within-helper closes the SUBSET direction for a single array
/// (`arr ⊆ set`), this helper closes the COVERAGE direction for a
/// three-array partition triple (`set ⊆ a ∪ b ∪ c`) at compile time.
/// Composed with the three sibling SUBSET-EMBEDDING witnesses (each
/// sub-array's `_within_str_finite_set` pin) AND the three sibling
/// pairwise-DISJOINTNESS witnesses (each pair's
/// `assert_str_arrays_disjoint` pin) AND the parent's INJECTIVITY
/// witness (`_pairwise_distinct` on the parent set), the four
/// contract-shape corners jointly close the full DISJOINT-UNION
/// theorem `set ≡ a ⊕ b ⊕ c` at rustc const-eval time on the
/// substrate's twelve-arm `SexpShape::LABELS` outer-vocabulary
/// partition — the pre-existing runtime cross-check
/// `sexp_shape_labels_is_disjoint_union_of_three_sub_vocabularies`
/// becomes a defense-in-depth safety net for the SAME theorem the
/// compile-time witness triple now enforces at `cargo check` time,
/// one invocation stage earlier.
///
/// Delegates target-set well-formedness to the ARRAY-side
/// [`assert_str_array_pairwise_distinct`] helper via a co-located
/// call at the TOP of the sweep — a malformed `set` (e.g.
/// `["a", "a", "b"]`) is not a well-formed finite set of cardinality
/// `W` and silently mis-verifies the intended COVERAGE contract on
/// any `(a, b, c)` triple whose distinct-value union misses the
/// duplicated set byte (the duplicated byte still counts as covered
/// on the FIRST hit even if the second copy is absent from the
/// union). Routes drift on the CALLER'S TARGET-SET SPEC to the SET-
/// side well-formedness axis rather than to a downstream SET-STR-
/// MISSING symptom on `(a, b, c)`. The (str) row does NOT carry a
/// separate `assert_str_finite_set_pairwise_distinct` alias; the
/// delegation reuses the ARRAY-side helper directly, matching the
/// (str) row's sibling delegation shape at
/// [`assert_str_array_within_str_finite_set`].
///
/// The sub-vocabulary arities `N`, `M`, `K` and the parent
/// cardinality `W` are INDEPENDENT const generics: this helper
/// intentionally does NOT enforce `N + M + K == W` at const-eval
/// time. That arity sum is a CONSEQUENCE of the disjoint-union
/// theorem (COVERAGE here + pairwise DISJOINTNESS at the sibling
/// witnesses + parent INJECTIVITY) rather than a separate pre-
/// condition; a caller that binds all three peer witnesses AND this
/// COVERAGE witness AND finds a cardinality mismatch has necessarily
/// broken one of the four contract corners, and the diagnostic fires
/// on the specific corner that broke rather than on a synthetic
/// arity-sum pre-check. Under-covering triples (`N + M + K < W`) fire
/// the SET-STR-MISSING panic here; over-covering triples
/// (`N + M + K > W`) fire the STR-DISJOINTNESS-VIOLATION panic at the
/// sibling pairwise-disjointness witness or the STR-SUBSET-VIOLATION
/// panic at the sibling `_within_str_finite_set` witness. The four-
/// corner diagnostic partition stays sharp on the FAILURE mode rather
/// than collapsing every drift onto a single arity-sum axis.
///
/// Pre-lift the twelve-arm `SexpShape::LABELS` disjoint-union
/// theorem lived as a runtime cross-check
/// (`sexp_shape_labels_is_disjoint_union_of_three_sub_vocabularies`
/// at `error.rs` tests module) that iterated every parent label and
/// counted its multiplicity across the three sub-vocabularies. The
/// runtime sweep enforced BOTH directions (⊆) and (⊇) of the
/// disjoint-union at test time; the (⊇) direction was already lifted
/// to compile time via three `_within_str_finite_set` witnesses in
/// `error.rs` (each sub-vocab ⊂ parent) and three
/// `assert_str_arrays_disjoint` witnesses (pairwise disjoint), but
/// the (⊆) direction — every parent label appears in at least one
/// sub-vocabulary — remained runtime-only. Post-lift this helper
/// binds the (⊆) direction at `cargo check` time via ONE `const _`
/// line on the (`AtomKind::LABELS`, `QuoteForm::LABELS`,
/// `StructuralKind::LABELS`, `SexpShape::LABELS`) partition-triple-
/// with-parent quadruple; a regression that silently drops a variant
/// from one of the three sub-vocabularies (e.g. removing
/// `AtomKind::Bool` and its `AtomKind::BOOL_LABEL` alias while
/// leaving `SexpShape::Bool` and its `SexpShape::BOOL_LABEL` alias in
/// the parent vocabulary) fires the SET-STR-MISSING panic at
/// `cargo check` BEFORE any test scheduler runs.
///
/// Adding a new n-way partition proof on the substrate's `&'static
/// str` vocabularies (e.g. a hypothetical fourth sub-vocabulary
/// carving of a widened `SexpShape` parent, or an independent
/// partition on `Atom::ESCAPE_SOURCES` into printable / whitespace /
/// control sub-vocabularies): pair the parent+partition declaration
/// with `const _: () = assert_str_finite_set_covered_by_three_str_
/// arrays::<N, M, K, W>(&Foo::LABELS, &Bar::LABELS, &Baz::LABELS,
/// &Parent::LABELS);` co-located after the partition arrays'
/// declarations and the COVERAGE contract binds at compile time. The
/// rustc-forced arities `[&'static str; N]`, `[&'static str; M]`,
/// `[&'static str; K]`, `[&'static str; W]` compose with this const-
/// eval sweep so BOTH the four cardinalities AND the (⊆) disjoint-
/// union direction are compile-time theorems on the SAME partition
/// quadruple.
///
/// Delegates to the existing module-private [`str_bytes_equal`]
/// const-fn helper so a future toolchain stabilising `const fn
/// str::eq` collapses ALL FOUR (str)-row helpers ((str, pairwise-
/// distinct), (str, disjointness), (str, subset-embedding), (str,
/// three-array-coverage)) through ONE edit.
///
/// Runtime callability: the function is a normal `pub const fn`, so
/// callers CAN also invoke it at runtime — pinned by
/// `assert_str_finite_set_covered_by_three_str_arrays_panics_at_runtime_on_uncovered_parent_entry`
/// and
/// `assert_str_finite_set_covered_by_three_str_arrays_panic_message_names_the_helper_and_set_str_missing_axis`.
/// The panic site carries the `"SET-STR-MISSING"` axis-provenance
/// string chosen DISTINCT from every sibling helper's axis vocabulary
/// (`"duplicate"` on the ARRAY-side pairwise-distinct sibling; `"STR-
/// SUBSET-VIOLATION"` on the (str) row SUBSET sibling; `"STR-
/// DISJOINTNESS-VIOLATION"` on the (str) row DISJOINTNESS sibling;
/// `"CHAR-SUBSET-VIOLATION"` on the (char) row-dual SUBSET sibling;
/// `"SUBSET-VIOLATION"` on the (u8) row-dual finite-set SUBSET-only
/// sibling; `"OUT-OF-SET"` / `"SET-BYTE-MISSING"` on the (u8) covers-
/// finite-set sibling; `"OUT-OF-RANGE"` / `"MISSING"` on the (u8)
/// covers-inclusive-range sibling; `"ARITY-MISMATCH"` on both (u8)
/// `_permutes_*` compound helpers; `"SET-NOT-PAIRWISE-DISTINCT"` on
/// the (u8) SET-side well-formedness sibling) so a diagnostic that
/// names the failed axis routes UNAMBIGUOUSLY to (a) this specific
/// three-array COVERAGE helper on the `&'static str` element-type
/// row, (b) the `set` argument as the drift-target (the parent
/// element that no sub-array reaches) rather than any single sub-
/// vocabulary carrier. The `"SET-STR-MISSING"` axis distinguishes
/// from the (u8) row's `"SET-BYTE-MISSING"` peer via the element-
/// type infix — one substring search per element-type routes any
/// row's SET-side COVERAGE-VIOLATION back to its element-type peer.
///
/// Theory grounding:
/// - THEORY.md §V.1 — knowable platform; the DISJOINT-UNION theorem
///   on `&'static str` sub-vocabulary partitions becomes a TYPE-LEVEL
///   theorem the substrate carries per (partition-triple, parent)
///   quadruple rather than a runtime test the developer must
///   remember to write per partition.
/// - THEORY.md §III — the typescape; the (element-type × contract-
///   shape) matrix now carries the THREE-ARRAY-COVERAGE corner on
///   the (`&'static str`) row at ONE peer const-fn helper. Combined
///   with the pre-existing (str, pairwise-distinct), (str, subset-
///   embedding), and (str, disjointness) siblings, the four contract-
///   shape corners on the (`&'static str`) row jointly close the
///   full DISJOINT-UNION theorem on any n-way partition of a
///   `&'static str` vocabulary at compile time.
/// - THEORY.md §VI.1 — generation over composition; the const-eval
///   coverage sweep IS the generative shape. Every new n-way `&'static
///   str` vocabulary partition adds ONE `const _` line (plus the
///   sibling SUBSET-EMBEDDING and pairwise-DISJOINTNESS witnesses per
///   sub-array pair) to get the DISJOINT-UNION theorem rather than
///   re-deriving a per-partition runtime iterator sweep at each site.
/// - THEORY.md §II.1 invariant 5 — composition preserves proofs; the
///   COVERAGE proof at declaration site AND the outer-algebra's
///   twelve-arm shape-label partition regenerate through the SAME
///   `const _` witness at the ARRAY level.
///
/// Frontier inspiration: Lean 4's `Finset.disjUnion` /
/// `Finset.biUnion_eq_iff_forall_mem_exists` unfolded at the
/// concrete 3-arm `[&'static str; W] ⊆ [&'static str; N] ∪ [&'static
/// str; M] ∪ [&'static str; K]` monomorphic realisation — the
/// substrate primitive here embeds the same coverage relation as a
/// rustc const-eval-time proof obligation at every partition site
/// rather than as a Lean tactic invocation deferred to `elab_command`.
pub const fn assert_str_finite_set_covered_by_three_str_arrays<
    const N: usize,
    const M: usize,
    const K: usize,
    const W: usize,
>(
    a: &[&'static str; N],
    b: &[&'static str; M],
    c: &[&'static str; K],
    set: &[&'static str; W],
) {
    // Delegate target-set well-formedness to the sibling ARRAY-side
    // pairwise-distinctness helper FIRST. Placed BEFORE the SET-STR-
    // MISSING sweep below because a malformed `set` (e.g. `["a", "a",
    // "b"]`) is not a well-formed finite set of cardinality `W` and
    // silently mis-verifies the intended COVERAGE contract — the
    // duplicated byte still counts as covered on the FIRST hit even
    // if the second copy is absent from the union. Routes drift on
    // the CALLER'S TARGET-SET SPEC to the SET-side well-formedness
    // axis rather than to a downstream SET-STR-MISSING symptom on
    // `(a, b, c)`. A well-formed `set` passes this arm as a no-op —
    // the sweep is const-eval-elidable and costs zero at rustc-time
    // on the substrate call sites.
    assert_str_array_pairwise_distinct(set);
    let mut w = 0;
    while w < W {
        let target = set[w];
        let mut found = false;
        let mut i = 0;
        while i < N {
            if str_bytes_equal(target, a[i]) {
                found = true;
                break;
            }
            i += 1;
        }
        if !found {
            let mut j = 0;
            while j < M {
                if str_bytes_equal(target, b[j]) {
                    found = true;
                    break;
                }
                j += 1;
            }
        }
        if !found {
            let mut k = 0;
            while k < K {
                if str_bytes_equal(target, c[k]) {
                    found = true;
                    break;
                }
                k += 1;
            }
        }
        if !found {
            panic!(
                "assert_str_finite_set_covered_by_three_str_arrays: \
                 SET-STR-MISSING — the target finite `set` carries a \
                 parent entry at some position whose bytes are NOT \
                 reached by any of the three sub-vocabulary arrays \
                 `a`, `b`, `c`. The substrate's THREE-ARRAY COVERAGE \
                 contract on the partition-triple is broken; every \
                 consumer that expects the union `a ∪ b ∪ c` to span \
                 the parent finite vocabulary (`SexpShape::LABELS ≡ \
                 AtomKind::LABELS ⊕ QuoteForm::LABELS ⊕ \
                 StructuralKind::LABELS` on the twelve-arm outer-\
                 shape label partition; any future n-way disjoint-\
                 union theorem on the substrate's `&'static str` \
                 vocabularies) relies on every parent entry being \
                 reached by at least one sub-array. Fix at the SUB-\
                 VOCABULARY DECLARATION site (the missing entry is \
                 an intentional variant of the parent that ONE sub-\
                 vocabulary must carry) OR at the PARENT-VOCABULARY \
                 DECLARATION site (the missing entry was inadvertently \
                 added to the parent without extending any sub-\
                 vocabulary) — the choice depends on whether the \
                 drift is an unintended parent overshoot or an \
                 unintended sub-vocabulary shrinkage"
            );
        }
        w += 1;
    }
}

/// Compile-time contract verifier — panics at const evaluation time if
/// `arr` is NOT the concatenation of `K` byte-verbatim replicas of
/// `head` followed by `N - K` byte-verbatim replicas of `tail` on the
/// substrate's family-wide `[&'static str; N]` MANY-TO-ONE variant →
/// canonical-projection vocabulary. Binds ONE conjunct clause: BLOCK-
/// CONSTANCY-VIOLATION — every entry in the HEAD segment `arr[0..K)`
/// MUST byte-equal `head` and every entry in the TAIL segment
/// `arr[K..N)` MUST byte-equal `tail`.
///
/// Contract-orthogonal peer to [`assert_str_array_pairwise_distinct`]
/// on the (INJECTIVITY, MANY-TO-ONE-BLOCK-CONSTANCY) axis of the
/// (contract-shape) column on the SAME (`&'static str`) row: where the
/// pairwise-distinctness sibling binds `∀ i ≠ j : arr[i] ≠ arr[j]` at
/// compile time (INTRA-ARRAY INJECTIVITY on arrays whose per-index
/// projection is bijective with a per-index typed source), this BLOCK-
/// CONSTANCY sibling binds `arr = [head; K] ++ [tail; N - K]` at
/// compile time (INTRA-ARRAY BLOCK-CONSTANT MANY-TO-ONE PROJECTION on
/// arrays whose per-index projection is a MANY-TO-ONE pattern
/// collapsing multiple contiguous typed sources onto ONE canonical
/// scalar byte). The two helpers close the (INJECTIVITY, MANY-TO-ONE-
/// BLOCK-CONSTANCY) 2-corner face on the (`&'static str`) row at ONE
/// peer const-fn helper per structural shape; a single family-wide
/// `[&'static str; N]` array picks whichever helper matches its
/// per-index projection cardinality (a BIJECTIVE per-index projection
/// binds the pairwise-distinct sibling; a MANY-TO-ONE per-index
/// projection binds this block-constancy sibling).
///
/// The invariant is load-bearing for the substrate's typed variant →
/// canonical-projection MANY-TO-ONE closed-set surface at
/// [`crate::error::CompilerSpecIoStage::OPERATIONS`]: the four typed
/// variants of [`crate::error::CompilerSpecIoStage`] project through
/// [`crate::error::CompilerSpecIoStage::operation`] onto EXACTLY TWO
/// canonical `&'static str` operation labels
/// ([`crate::error::CompilerSpecIoStage::REALIZE_TO_DISK_OPERATION`]
/// (`"realize_to_disk"`) shared by
/// [`crate::error::CompilerSpecIoStage::RealizeToDiskSerialize`] and
/// [`crate::error::CompilerSpecIoStage::RealizeToDiskWrite`];
/// [`crate::error::CompilerSpecIoStage::LOAD_FROM_DISK_OPERATION`]
/// (`"load_from_disk"`) shared by
/// [`crate::error::CompilerSpecIoStage::LoadFromDiskRead`] and
/// [`crate::error::CompilerSpecIoStage::LoadFromDiskDeserialize`]).
/// The `[REALIZE, REALIZE, LOAD, LOAD]` block-constant shape encodes
/// the compound-key `"{operation}: {stage}"` surface's 2-of-2-to-2
/// partition — a regression that silently flip-flopped the projection
/// (e.g. reorder to `[REALIZE, LOAD, REALIZE, LOAD]`, drift a variant
/// slot to a distinct third operation label) would compile cleanly
/// past the sibling `_pairwise_distinct` exclusion (this array is
/// INTENTIONALLY non-injective, so the pairwise-distinct helper does
/// NOT bind it) and only fail at test time via the runtime pin
/// `compiler_spec_io_stage_operations_align_with_all_by_index`; post-
/// lift the ARRAY-LEVEL block-constancy binds at rustc time via ONE
/// `const _` line, one invocation stage earlier than the runtime pin.
///
/// The `K = 0` and `K = N` degenerate corners collapse the array into
/// a ONE-BLOCK replica: `K = 0` yields `arr = [tail; N]`; `K = N`
/// yields `arr = [head; N]`. Both corners pass through the same
/// sweep without a distinct code path, and both are covered by the
/// helper's test surface — a future substrate array whose per-index
/// projection collapses ALL positions onto ONE canonical byte binds
/// through either degenerate corner at ONE const-generic setting.
///
/// SET-side well-formedness: no SET-side arm here — the helper takes
/// TWO scalars, NOT a scalar plus a target-set spec, so there's no
/// SET well-formedness axis to gate on the CALLER'S input. The two
/// scalars `head` and `tail` MAY be byte-equal (in which case the
/// helper degenerates to a SINGLE-BLOCK replica-check that binds the
/// SAME constancy across all `N` positions with `head == tail`); the
/// two scalars MAY be byte-distinct (the intended TWO-BLOCK partition
/// shape). Either case is a well-formed BLOCK-CONSTANCY invariant.
///
/// CARDINALITY-MISMATCH gate: `K > N` fails FIRST at const-eval time
/// (before any per-position sweep begins) with a CARDINALITY-MISMATCH
/// arm so a caller-side turbofish arity slip on the `K` const-generic
/// routes to the CARDINALITY axis rather than silently degenerating
/// into a truncated head-only sweep. The `K == N` and `K == 0`
/// corners are LEGAL (the ONE-BLOCK degenerate shapes) so the gate
/// is `K > N`, not `K >= N`.
///
/// Adding a new family-wide `[&'static str; N]` MANY-TO-ONE variant →
/// canonical-projection array to the substrate: pair the declaration
/// with `const _: () = assert_str_array_is_concatenation_of_two_
/// scalar_replicas::<N, K>(&Self::FOO, HEAD_SCALAR, TAIL_SCALAR);`
/// co-located after the array's declaration and the block-constancy
/// contract binds at compile time. The rustc-forced arity
/// `[&'static str; N]` composes with this const-eval sweep so BOTH
/// cardinality AND per-index MANY-TO-ONE block-constancy are compile-
/// time theorems on the SAME array.
///
/// Runtime callability: the function is a normal `pub const fn`, so
/// callers CAN also invoke it at runtime — pinned by
/// `assert_str_array_is_concatenation_of_two_scalar_replicas_panics_
/// at_runtime_on_head_segment_drift`, `..._on_tail_segment_drift`,
/// `..._on_arity_slip`, and `..._panic_message_names_the_helper_and_
/// block_constancy_violation_axis`.
///
/// Theory grounding:
/// - THEORY.md §V.1 — knowable platform; the MANY-TO-ONE variant →
///   canonical-projection block-constant contract on the `&'static
///   str` per-index projection axis becomes a TYPE-LEVEL theorem the
///   substrate carries per (arr, head, tail, K) quadruple rather than
///   a runtime iterator sweep the developer must remember to write
///   per quadruple.
/// - THEORY.md §III — the typescape; the (element-type × contract-
///   shape) matrix now carries the MANY-TO-ONE-BLOCK-CONSTANCY corner
///   on the (`&'static str`) row at ONE peer const-fn helper. Combined
///   with the pre-existing (`_pairwise_distinct`) INJECTIVITY sibling
///   the two helpers close the (INJECTIVITY, MANY-TO-ONE-BLOCK-
///   CONSTANCY) 2-corner face on the (`&'static str`) row — every
///   family-wide `[&'static str; N]` per-index projection array picks
///   the corner that matches its per-index projection cardinality.
/// - THEORY.md §VI.1 — generation over composition; the const-eval
///   block-constant sweep IS the generative shape. Every new MANY-TO-
///   ONE variant → canonical-projection substrate closed set adds ONE
///   `const _` line to get the block-constant theorem rather than
///   re-deriving a per-projection runtime multiplicity check.
/// - THEORY.md §II.1 invariant 5 — composition preserves proofs; the
///   BLOCK-CONSTANCY proof at declaration site AND the compound-key
///   `"{operation}: {stage}"` surface's partition contract regenerate
///   through the SAME `const _` witness at the ARRAY level.
///
/// Frontier inspiration: Lean 4's `List.replicate` unfolded at the
/// concrete 2-block `List.replicate K head ++ List.replicate (N - K)
/// tail` monomorphic realisation — the substrate primitive embeds
/// the same run-length shape as a rustc const-eval-time proof
/// obligation at every block-constant projection site rather than as
/// a Lean tactic invocation deferred to `elab_command`. The MANY-TO-
/// ONE variant → canonical-projection shape mirrors GHC Core's
/// constant-folding of a `case` scrutinee whose match arms collapse a
/// wider sum type onto a narrower sum type via a per-arm literal
/// projection; where GHC folds this at Core compilation, the
/// substrate binds the projection identity at rustc const-eval time.
pub const fn assert_str_array_is_concatenation_of_two_scalar_replicas<
    const N: usize,
    const K: usize,
>(
    arr: &[&'static str; N],
    head: &'static str,
    tail: &'static str,
) {
    if K > N {
        panic!(
            "assert_str_array_is_concatenation_of_two_scalar_replicas: \
             CARDINALITY-MISMATCH — the two const parameters `N` and \
             `K` must satisfy `K <= N` so the HEAD segment of `arr` \
             (positions `[0..K)`) followed by the TAIL segment \
             (positions `[K..N)`) exactly cover `arr`'s `N` \
             positions. Fix at the `const _` witness's turbofish by \
             reconciling the two arities against the composite's \
             declared arity. The CARDINALITY-MISMATCH gate \
             distinguishes THIS failure from every content-drift arm \
             — a mistyped ARITY on the caller side fails HERE before \
             any per-position sweep begins, so a subtle arity slip \
             doesn't silently degenerate into a truncated head-only \
             sweep."
        );
    }
    let mut i = 0;
    while i < K {
        if !str_bytes_equal(arr[i], head) {
            panic!(
                "assert_str_array_is_concatenation_of_two_scalar_replicas: \
                 HEAD-SEGMENT-BLOCK-CONSTANCY-VIOLATION — the family-\
                 wide `&'static str` array `arr` carries an entry at \
                 some position in `[0, K)` (the HEAD segment) that \
                 does NOT byte-for-byte equal the peer `head` scalar. \
                 The substrate's HEAD-SEGMENT BLOCK-CONSTANCY \
                 contract on the array is broken; every consumer that \
                 reads `arr[0..K)` and the peer `head` scalar as \
                 INTERCHANGEABLE (any MANY-TO-ONE variant → \
                 canonical-projection consumer expecting the first \
                 `K` variant slots to share ONE canonical projection \
                 byte — e.g. the `zip(Self::ALL, Self::OPERATIONS)` \
                 compound-key `\"{{operation}}: {{stage}}\"` surface \
                 consumers on \
                 `crate::error::CompilerSpecIoStage::OPERATIONS` \
                 whose first two slots project through \
                 `crate::error::CompilerSpecIoStage::REALIZE_TO_DISK_\
                 OPERATION`) relies on this invariant. Fix at the \
                 ARRAY-DECLARATION site (the drifted `arr[i]` entry) \
                 OR at the per-role scalar constant that `head` \
                 re-exports — the choice depends on whether the drift \
                 is an unintended slot reorder inside the array or a \
                 rename of the canonical projection byte upstream."
            );
        }
        i += 1;
    }
    let mut j = K;
    while j < N {
        if !str_bytes_equal(arr[j], tail) {
            panic!(
                "assert_str_array_is_concatenation_of_two_scalar_replicas: \
                 TAIL-SEGMENT-BLOCK-CONSTANCY-VIOLATION — the family-\
                 wide `&'static str` array `arr` carries an entry at \
                 some position in `[K, N)` (the TAIL segment) that \
                 does NOT byte-for-byte equal the peer `tail` scalar. \
                 The substrate's TAIL-SEGMENT BLOCK-CONSTANCY \
                 contract on the array is broken; every consumer that \
                 reads `arr[K..N)` and the peer `tail` scalar as \
                 INTERCHANGEABLE (any MANY-TO-ONE variant → \
                 canonical-projection consumer expecting the last \
                 `N - K` variant slots to share ONE canonical \
                 projection byte — e.g. the `zip(Self::ALL, \
                 Self::OPERATIONS)` compound-key `\"{{operation}}: \
                 {{stage}}\"` surface consumers on \
                 `crate::error::CompilerSpecIoStage::OPERATIONS` whose \
                 last two slots project through \
                 `crate::error::CompilerSpecIoStage::LOAD_FROM_DISK_\
                 OPERATION`) relies on this invariant. Fix at the \
                 ARRAY-DECLARATION site (the drifted `arr[j]` entry) \
                 OR at the per-role scalar constant that `tail` \
                 re-exports — the choice depends on whether the drift \
                 is an unintended slot reorder inside the array or a \
                 rename of the canonical projection byte upstream."
            );
        }
        j += 1;
    }
}

/// Compile-time contract verifier — panics at const evaluation time if
/// the sub-slice `full[START..START + M)` does NOT byte-equal the peer
/// sub-array `sub[..]` positionwise (`&'static str`-by-`&'static str`).
///
/// Row-dual peer of [`assert_u8_array_slice_equals_u8_array`] and
/// [`assert_char_array_slice_equals_char_array`] on the (element-type)
/// axis: where the `u8` sibling closes the outer-`Sexp` cache-key
/// discriminator sub-carving vocabulary at compile time AND the `char`
/// sibling closes the substrate's reader-boundary `[char; N]` scalar-
/// composed vocabulary at compile time, this closes the substrate's
/// family-wide `[&'static str; N]` label / prefix / tag / literal
/// vocabulary at compile time. Opens the SUB-SLICE ARRAY-image column
/// on the (str) row of the (element-type × contract-shape) matrix peer
/// to the u8-row + char-row siblings' SUB-SLICE ARRAY-image column —
/// the three helpers together lift EVERY positionwise-composition
/// contract `arr[START..START + M) == sub[..]` on scalar-family-wide
/// substrate arrays into a COMPILE-TIME theorem, one per element-type
/// row of the matrix.
///
/// The MIDDLE-SLICE corner (`0 < START`, `START + M < N`) — the shape
/// the (str) row uniquely exercises against the substrate's twelve-arm
/// [`crate::error::SexpShape::LABELS`] vocabulary — pins that each
/// sub-carving's LABELS array occupies its CANONICAL SLOTS on the
/// parent superset's declaration order, one invocation stage stronger
/// than the pre-existing SET-level DISJOINT-UNION witnesses (three
/// `assert_str_array_within_str_finite_set::<sub, 12>` embeddings,
/// three `assert_str_arrays_disjoint::<a, b>` pairwise-disjointness
/// witnesses, one `assert_str_finite_set_covered_by_three_str_arrays::
/// <6, 4, 2, 12>` coverage witness, one `assert_str_array_pairwise_
/// distinct(&SexpShape::LABELS)` INJECTIVITY witness). The SET-level
/// theorem `SexpShape::LABELS ≡ AtomKind::LABELS ⊕ QuoteForm::LABELS ⊕
/// StructuralKind::LABELS` those seven witnesses close is SILENT on
/// which SLOTS each sub-vocabulary's arms occupy — a regression that
/// permuted `SexpShape::LABELS` from
/// `[NIL, SYMBOL, KEYWORD, STRING, INT, FLOAT, BOOL, LIST, QUOTE,
/// QUASIQUOTE, UNQUOTE, UNQUOTE_SPLICE]` (`StructuralKind` at slots
/// `{0, 7}`, `AtomKind` at slots `[1..7)`, `QuoteForm` at slots
/// `[8..12)` — the CANONICAL positional decomposition) to
/// `[SYMBOL, NIL, KEYWORD, STRING, INT, FLOAT, BOOL, LIST, QUOTE,
/// QUASIQUOTE, UNQUOTE, UNQUOTE_SPLICE]` (swapping slots `0` and `1`,
/// interleaving `AtomKind` into a slot the structural-residual carving
/// previously owned) preserves the SET-level disjoint-union theorem
/// (both sub-vocabularies still embed into the parent, still cover, still
/// disjoint, parent still injective) but silently misaligns every
/// consumer indexing `SexpShape::LABELS[0]` for the NIL diagnostic
/// literal. THIS helper binds each sub-slice's positionwise composition
/// against its sub-vocabulary's canonical array at rustc time —
/// strictly STRONGER on the (contract-strength) axis than the sibling
/// SET-level DISJOINT-UNION witnesses.
///
/// Consumer sites this helper closes at the MIDDLE-SLICE corner:
/// * [`crate::error::SexpShape::LABELS`] `[0..1) ==
///   [crate::error::StructuralKind::NIL_LABEL]` — the singleton left-
///   endpoint slot of the outer twelve-shape LABELS array binds the
///   structural-residual carving's NIL role at the CANONICAL slot `0`.
/// * [`crate::error::SexpShape::LABELS`] `[1..7) == AtomKind::LABELS` —
///   the six-slot atomic-payload middle slice binds the six atomic
///   variants' LABELS at the CANONICAL slots `[1..7)`, one invocation
///   stage stronger than the (u8)-row peer at the SAME slice range
///   `[1..7)` where the six slots collapse to a single scalar
///   [`AtomKind::OUTER_HASH_DISCRIMINATOR`] byte (`1u8`) — the (str)
///   row's per-slot LABELS listing distinguishes ALL SIX slots
///   individually, so a permutation of the six atomic arms inside the
///   parent's `[1..7)` slice fails HERE where the (u8)-row's SCALAR-
///   REPLICA sibling stays silent.
/// * [`crate::error::SexpShape::LABELS`] `[7..8) ==
///   [crate::error::StructuralKind::LIST_LABEL]` — the singleton mirror-
///   endpoint slot at the atomic-collapse right endpoint binds the
///   structural-residual carving's LIST role at the CANONICAL slot `7`.
/// * [`crate::error::SexpShape::LABELS`] `[8..12) == QuoteForm::LABELS`
///   — the four-slot quote-family tail slice binds the four quote-
///   family variants' LABELS at the CANONICAL slots `[8..12)`, peer to
///   the (u8)-row's `assert_u8_array_slice_equals_u8_array::<12, 4, 8>
///   (&SexpShape::HASH_DISCRIMINATORS, &QuoteForm::HASH_DISCRIMINATORS)`
///   witness.
///
/// Together the FOUR positional witnesses cover the ENTIRE twelve-slot
/// outer container's per-position LABEL sequence at rustc time — the
/// UNION of the four disjoint slice ranges `[0..1) ∪ [1..7) ∪ [7..8) ∪
/// [8..12)` exhausts the twelve-slot outer container's position space.
/// Sibling posture to the (u8)-row's FOUR positional witnesses on
/// `SexpShape::HASH_DISCRIMINATORS` (the two singleton
/// slice-equals-array witnesses on `[0..1)` and `[7..8)`, the six-slot
/// slice-is-scalar-replica witness on `[1..7)`, the four-slot
/// slice-equals-array witness on `[8..12)`) — both rows now carry the
/// FULL positional decomposition of the twelve-slot outer container at
/// rustc time on BOTH the (u8) discriminator axis AND the (str) label
/// axis. A regression that reordered `SexpShape::LABELS` fails at BOTH
/// the (u8)-row `HASH_DISCRIMINATORS` positional witnesses (through
/// the parallel outer twelve-slot ordering) AND the (str)-row LABELS
/// positional witnesses lifted here.
///
/// Pre-lift the twelve-arm `SexpShape::LABELS` positional decomposition
/// lived ONLY through the runtime cross-check
/// `sexp_shape_labels_align_with_sub_vocabularies_by_position` (in
/// `error.rs`, sweeping the twelve positions and routing each
/// `SexpShape::LABELS[i]` through its sub-vocabulary via
/// `SexpShape::ALL[i].as_atom_kind() / .as_quote_form()` composition);
/// post-lift the ARRAY-LEVEL positional decomposition binds at rustc
/// time via FOUR `const _` lines, one invocation stage earlier than
/// the runtime pin. A regression that reorders the outer
/// `SexpShape::LABELS` array's initializer (e.g. swapping slot `0`'s
/// `Self::NIL_LABEL` with slot `1`'s `Self::SYMBOL_LABEL`) while
/// leaving each sub-vocabulary in its canonical order fails at
/// `cargo check` BEFORE any test scheduler runs.
///
/// The three axis-partitioned panic messages (`START-OUT-OF-BOUNDS`,
/// `SLICE-LENGTH-OUT-OF-BOUNDS`, `STR-SLICE-EQUALS-ARRAY-VIOLATION`)
/// mirror the u8 sibling's message vocabulary with the `STR-` prefix
/// on the CONTENT-drift axis so callers grep either the (u8) row's
/// plain `SLICE-EQUALS-ARRAY-VIOLATION`, the (char) row's
/// `CHAR-SLICE-EQUALS-ARRAY-VIOLATION`, or the (str) row's
/// `STR-SLICE-EQUALS-ARRAY-VIOLATION` axis-prefix by element-type. The
/// shared `-SLICE-EQUALS-ARRAY-VIOLATION` infix lets callers grep any
/// element-type variant by the shared axis substring.
///
/// Adding a new family-wide `[&'static str; N]` array to the substrate
/// whose declaration is a positionwise composition against named per-
/// role `pub const *_LABEL` / `*_PREFIX` / `*_TAG` `&'static str`
/// constants: pair the declaration with `const _: () =
/// assert_str_array_slice_equals_str_array::<N, M, START>(&Self::FOO_
/// ARRAY, &Self::SUB_ARRAY);` co-located after the array's declaration
/// and the per-slot ORDER contract binds at compile time. The rustc-
/// forced arities `[&'static str; N]` + `[&'static str; M]` compose
/// with this const-eval sweep so BOTH cardinality AND per-slot
/// canonical-str-value are compile-time theorems on the SAME array.
///
/// Runtime callability: the function is a normal `pub const fn`, so
/// callers CAN also invoke it at runtime — e.g. a REPL / LSP surface
/// that constructs a `[&'static str; N]` at runtime from a user-
/// supplied vocabulary and wants to verify positionwise composition
/// against a peer sub-array before consuming it — and the panic
/// surfaces normally in that path (pinned by
/// `assert_str_array_slice_equals_str_array_panics_at_runtime_on_positionwise_drift`,
/// `assert_str_array_slice_equals_str_array_panics_at_runtime_on_start_out_of_bounds`,
/// `assert_str_array_slice_equals_str_array_panics_at_runtime_on_slice_length_out_of_bounds`,
/// AND
/// `assert_str_array_slice_equals_str_array_panic_message_names_the_helper_and_str_slice_equals_array_violation_axis`).
///
/// Theory grounding:
/// - THEORY.md §V.1 — knowable platform; the family-wide per-position
///   ORDER contract on the `&'static str`-typed vocabulary becomes a
///   TYPE-LEVEL theorem the substrate carries per array declaration
///   rather than a runtime iterator sweep the developer must remember
///   to write per array.
/// - THEORY.md §VI.1 — generation over composition; the const-eval
///   positionwise sweep IS the generative shape. Every new closed-set
///   string array declared as a positionwise composition against
///   named-per-role `pub const` labels adds ONE `const _` line to get
///   the per-slot ORDER theorem rather than re-deriving a runtime
///   index-by-index `assert_eq!` block per array.
/// - THEORY.md §II.1 invariant 5 — composition preserves proofs; the
///   per-slot-ORDER proof at declaration site AND the per-role
///   `pub const *_LABEL` alias-chain composition every consumer relies
///   on regenerate through the SAME `const _` witness.
pub const fn assert_str_array_slice_equals_str_array<
    const N: usize,
    const M: usize,
    const START: usize,
>(
    full: &[&'static str; N],
    sub: &[&'static str; M],
) {
    if START > N {
        panic!(
            "assert_str_array_slice_equals_str_array: START-OUT-OF-\
             BOUNDS — the const parameter `START` sits OUTSIDE the \
             outer array's valid position range `[0..N]` (inclusive \
             upper bound: `START == N` combined with `M == 0` is the \
             LEGAL empty-slice-at-right-endpoint corner). Fix at the \
             `const _` witness's turbofish by reconciling `START` \
             against the outer array's declared arity `N`. The \
             START-OUT-OF-BOUNDS gate fires FIRST — a mistyped \
             `START` on the caller side fails HERE before the peer \
             `SLICE-LENGTH-OUT-OF-BOUNDS` gate reads `N - START` \
             (which would underflow `usize` had this gate not caught \
             the slip), so a subtle bounds slip doesn't silently \
             degenerate into a subtraction wrap-around OR a panic \
             deeper in `full[START + i]` bounds-checking."
        );
    }
    if M > N - START {
        panic!(
            "assert_str_array_slice_equals_str_array: SLICE-LENGTH-\
             OUT-OF-BOUNDS — the peer sub-array's arity `M` exceeds \
             the outer array's tail cardinality `N - START`, so the \
             positionwise sweep `full[START + i]` for `i ∈ [0..M)` \
             would overrun the outer array's valid position range \
             `[0..N)` at some `i ∈ [N - START..M)`. Fix at the \
             `const _` witness's turbofish by reconciling `M` against \
             the outer array's tail cardinality `N - START` OR by \
             narrowing `START` to leave a longer tail. The peer \
             `START-OUT-OF-BOUNDS` gate above guarantees `START ≤ N` \
             so `N - START` never underflows `usize` at this gate. \
             The LEGAL exact-fit corner `M == N - START` (the sub-\
             array reaches EXACTLY to the outer array's right \
             endpoint) is accepted; the STRICT `M > N - START` slip \
             is what this gate rejects."
        );
    }
    let mut i = 0;
    while i < M {
        if !str_bytes_equal(full[START + i], sub[i]) {
            panic!(
                "assert_str_array_slice_equals_str_array: STR-SLICE-\
                 EQUALS-ARRAY-VIOLATION — the outer `[&'static str; \
                 N]` array `full` carries a str at some position \
                 `START + i` (for `i ∈ [0..M)`) that does NOT byte-\
                 equal the peer `[&'static str; M]` sub-array `sub` \
                 at the offset-matched position `i`. The substrate's \
                 SLICE-EQUALS-ARRAY positionwise-composition contract \
                 on the sub-slice `full[START..START + M) == sub[..]` \
                 is broken; every consumer that reads `full[START..\
                 START + M)` as a positionwise-aligned copy of a peer \
                 sub-vocabulary's canonical `[&'static str; M]` \
                 listing (the twelve-slot outer container \
                 `crate::error::SexpShape::LABELS` whose four \
                 canonical sub-slices `[0..1) == \
                 [crate::error::StructuralKind::NIL_LABEL]`, `[1..7) \
                 == AtomKind::LABELS`, `[7..8) == [crate::error::\
                 StructuralKind::LIST_LABEL]`, `[8..12) == \
                 QuoteForm::LABELS` compose the parent LABELS \
                 vocabulary from the three sub-carvings' LABELS \
                 arrays; any future container-array sub-slice byte-\
                 for-byte equal to a peer sub-carving's canonical \
                 `[&'static str; M]` listing) relies on this \
                 invariant. Fix at the ARRAY-DECLARATION site (the \
                 drifted `full[START + i]` entry inside the slice \
                 segment) OR at the peer sub-array's arm listing — \
                 the choice depends on whether the drift is an \
                 unintended slot reorder in the outer array's tail \
                 OR in the sub-carving's own listing."
            );
        }
        i += 1;
    }
}

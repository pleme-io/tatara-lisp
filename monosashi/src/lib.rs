//! 物差し — **the measuring stick.** One strictness reading, shared by every
//! pleme-io lisp.
//!
//! A program on a gradual substrate slides along a continuum — loose and
//! dynamic at one end, locked and Rust-like at the other — and it slides
//! *because of what the author wrote*. `blue` calls that continuum the
//! blueshift (`theory/BLUE.md` §V.20) and reports it as a rung plus the
//! factors that caused it. `tatara-lisp` has the same continuum, drawn by its
//! own gradual checker, and needs the same reading.
//!
//! Two questions, and the second is the one that matters:
//!
//! 1. **How far is this shifted?** — a [`Rung`] on a [`Ladder`].
//! 2. **What is shifting it?** — every [`Factor`], each pointing at the exact
//!    source that caused it. An aggregate without the second is a score, and a
//!    score nobody can act on is decoration.
//!
//! # Why this is its own crate, below both lisps
//!
//! `blue` depends on `tatara-lisp`. So `tatara-lisp` cannot depend on `blue`,
//! and copying blue's vocabulary into tatara would create a *parallel*
//! vocabulary — two readings free to disagree about what "checked" means,
//! which is the same failure the shared `binding_shapes` table exists to
//! prevent one layer down. The shared core therefore sits BELOW both, with no
//! dependency on either and no dependency at all.
//!
//! # The ladder is the consumer's, not this crate's
//!
//! Nothing here names a rung. `blue`'s ladder is
//! `dynamic → annotated → checked → restricted`; tatara's is drawn by what
//! *its* checker actually verifies. A fixed enum would force one lisp's model
//! onto the other, which is the parallel this crate exists to avoid. A
//! consumer declares a `static` [`Ladder`] and a [`Rung`] is only obtainable
//! *from* that ladder — so a position on one lisp's continuum cannot be
//! constructed for, or compared against, another's.
//!
//! # Measured, never estimated
//!
//! Every number in a [`Reading`] comes from an analysis that actually ran.
//! [`Measured`] carries the counts so a consumer of the reading can see the
//! denominator rather than trust the verdict. A factor a producer cannot
//! measure is *absent* from the reading rather than approximated.
//!
//! # Evidence is not `Option<ByteRange>`
//!
//! This is the load-bearing decision in the crate. [`Evidence`] is
//! [`Evidence::At`] or [`Evidence::Unlocated`], and the unlocated arm carries
//! a [`Blindspot`] saying *why*.
//!
//! `Option::None` loses the reason, and — worse — reads as "no evidence
//! needed". The two producers are asymmetric in exactly this way today:
//! `tatara-lisp`'s `TypeDiagnostic` carries a real `Span` and passes
//! [`Evidence::At`] on day one, while `blue`'s `Diagnostic` is a bare `String`
//! and its factors fall back to the whole document. With `Option` that
//! degradation is invisible and becomes the silent baseline; with
//! [`Blindspot::ProducerHasNoSpans`] it is a *stated* gap that a reader can
//! see, count ([`Reading::blind_spots`]) and close.
//!
//! Offsets are BYTES, half-open `[start, end)`, matching `tatara_lisp::Span`
//! exactly. Never LSP coordinates: a UTF-16 column is an editor-protocol
//! concern, and a shared core that knew about it would push that concern into
//! every consumer, including the ones with no editor.

use std::fmt;

// ── The ladder ────────────────────────────────────────────────────────────

/// One named position on a ladder, and what standing there actually means.
///
/// `meaning` is not decoration: a rung label alone ("annotated") tells an
/// author where they are but not what it bought them, and a reading nobody can
/// interpret is the same decoration as a bare score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// The word a status bar shows. Lowercase by convention.
    pub label: &'static str,
    /// What being here means, in one line, for a hover or status bar.
    pub meaning: &'static str,
}

impl Step {
    #[must_use]
    pub const fn new(label: &'static str, meaning: &'static str) -> Self {
        Self { label, meaning }
    }
}

/// An ordered set of [`Step`]s, loosest first, locked last.
///
/// Declared `static` by the consumer, so a [`Rung`] can borrow it for
/// `'static` and carry its whole vocabulary with it.
///
/// ```
/// use monosashi::{Ladder, Step};
///
/// static RIGOR: Ladder = Ladder::new(
///     "example",
///     &[
///         Step::new("loose", "nothing is checked"),
///         Step::new("locked", "everything is checked"),
///     ],
/// );
///
/// assert_eq!(RIGOR.height(), 2);
/// assert_eq!(RIGOR.bottom().label(), "loose");
/// assert!(RIGOR.bottom() < RIGOR.top());
/// ```
#[derive(Debug)]
pub struct Ladder {
    name: &'static str,
    steps: &'static [Step],
}

impl Ladder {
    /// # Panics
    ///
    /// If `steps` is empty. A ladder with no steps has no position to report,
    /// so every `rung()` would be `None` and the reading would be silently
    /// empty forever.
    ///
    /// In a `static` or `const` initialiser — the only place a `Ladder` can
    /// usefully be built, since [`Rung`] borrows it for `'static` — this is
    /// **const-eval-rejected**, not a runtime panic. Measured: `E0080`,
    /// *"evaluation panicked: a ladder with no steps has no position to
    /// report"*, and the crate does not compile.
    ///
    /// ```compile_fail
    /// use monosashi::Ladder;
    /// static EMPTY: Ladder = Ladder::new("empty", &[]);
    /// # let _ = EMPTY.height();
    /// ```
    #[must_use]
    pub const fn new(name: &'static str, steps: &'static [Step]) -> Self {
        assert!(
            !steps.is_empty(),
            "a ladder with no steps has no position to report"
        );
        Self { name, steps }
    }

    /// What this ladder measures — shown when a reading has to say which
    /// continuum it is on.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub const fn steps(&self) -> &'static [Step] {
        self.steps
    }

    /// How many positions this ladder has. Named `height` rather than `len`
    /// because a ladder is never empty and an `is_empty()` that can only
    /// answer `false` is a question worth not asking.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.steps.len()
    }

    /// The rung at `index`, or `None` if this ladder is not that tall. The
    /// ONLY way to obtain a [`Rung`]: a position is meaningless without the
    /// ladder it is a position on.
    #[must_use]
    pub fn rung(&'static self, index: usize) -> Option<Rung> {
        if index < self.steps.len() {
            Some(Rung {
                ladder: self,
                index,
            })
        } else {
            None
        }
    }

    /// The loosest position. Always exists — see [`Ladder::new`].
    #[must_use]
    pub fn bottom(&'static self) -> Rung {
        Rung {
            ladder: self,
            index: 0,
        }
    }

    /// The most locked position. Always exists — see [`Ladder::new`].
    #[must_use]
    pub fn top(&'static self) -> Rung {
        Rung {
            ladder: self,
            index: self.steps.len() - 1,
        }
    }
}

/// The cell of a [`Rung::ramp`] that has been reached.
pub const RAMP_FILLED: char = '█';
/// The cell of a [`Rung::ramp`] that has not.
pub const RAMP_EMPTY: char = '░';

/// A position ON a particular [`Ladder`].
///
/// Constructible only via [`Ladder::rung`] / [`Ladder::bottom`] /
/// [`Ladder::top`], so a rung always knows its own vocabulary and a bare
/// integer can never be mistaken for one.
///
/// Ordering is **partial on purpose**: two rungs compare only when they came
/// from the same ladder. "tatara is more locked than blue" is not a claim
/// either ladder can support, so it has no answer rather than a wrong one.
#[derive(Clone, Copy, Debug)]
pub struct Rung {
    ladder: &'static Ladder,
    index: usize,
}

impl Rung {
    #[must_use]
    pub const fn ladder(self) -> &'static Ladder {
        self.ladder
    }

    /// Position from the loose end, `0`-based.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    #[must_use]
    pub const fn step(self) -> Step {
        self.ladder.steps[self.index]
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        self.step().label
    }

    #[must_use]
    pub const fn meaning(self) -> &'static str {
        self.step().meaning
    }

    /// Are these two rungs positions on the same continuum? The precondition
    /// for comparing them at all.
    #[must_use]
    pub fn same_ladder(self, other: Rung) -> bool {
        std::ptr::eq(self.ladder, other.ladder)
    }

    /// The looser of two rungs, or `None` if they are not on the same ladder.
    ///
    /// The whole-document rung is the LOWEST its subjects justify: one
    /// unannotated declaration means the checker genuinely did not check it,
    /// and taking the maximum would let a single annotated helper report a
    /// file as checked — the reading an author would most regret trusting.
    #[must_use]
    pub fn looser_of(self, other: Rung) -> Option<Rung> {
        if self.same_ladder(other) {
            Some(if self.index <= other.index {
                self
            } else {
                other
            })
        } else {
            None
        }
    }

    /// The rung rendered as a progress ramp, one cell per step of its ladder,
    /// filled to here. A four-step ladder produces `█░░░` … `████`.
    #[must_use]
    pub fn ramp(self) -> String {
        let height = self.ladder.steps.len();
        let mut out = String::with_capacity(height * RAMP_FILLED.len_utf8());
        for i in 0..height {
            out.push(if i <= self.index {
                RAMP_FILLED
            } else {
                RAMP_EMPTY
            });
        }
        out
    }
}

impl PartialEq for Rung {
    fn eq(&self, other: &Self) -> bool {
        self.same_ladder(*other) && self.index == other.index
    }
}

impl Eq for Rung {}

impl PartialOrd for Rung {
    /// `None` across ladders — see the type docs.
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.same_ladder(*other) {
            Some(self.index.cmp(&other.index))
        } else {
            None
        }
    }
}

impl fmt::Display for Rung {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ── Evidence ──────────────────────────────────────────────────────────────

/// A half-open byte range `[start, end)` into the source a producer read.
///
/// Byte offsets, matching `tatara_lisp::Span` exactly. Ranges are meaningful
/// only against the source string that produced them; holding onto that string
/// is the caller's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Width in bytes. Saturating, so a reversed range reads `0` rather than
    /// wrapping to a colossal length that a renderer would try to highlight.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }
}

impl fmt::Display for ByteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// Why a factor has no source position.
///
/// Every variant is a *stated* gap. Adding one is how a new class of
/// unlocatable evidence becomes visible instead of collapsing into the same
/// anonymous `None` as every other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Blindspot {
    /// The analysis that produced this factor does not carry source positions
    /// at all — the gap is in the producer, not in this particular factor.
    /// `blue`'s checker is here today: its diagnostic is a bare `String`.
    ProducerHasNoSpans,
    /// The factor is about a node that macro expansion produced, so no source
    /// position exists to point at. `tatara_lisp::Span::synthetic()` maps
    /// here.
    Synthetic,
}

impl Blindspot {
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Blindspot::ProducerHasNoSpans => {
                "the analysis that found this does not record source positions"
            }
            Blindspot::Synthetic => {
                "macro expansion produced this — there is no source position to point at"
            }
        }
    }
}

impl fmt::Display for Blindspot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

/// Where to look — or a stated reason there is nowhere to look.
///
/// Deliberately not `Option<ByteRange>`; see the crate docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Evidence {
    At(ByteRange),
    Unlocated(Blindspot),
}

impl Evidence {
    #[must_use]
    pub const fn range(self) -> Option<ByteRange> {
        match self {
            Evidence::At(r) => Some(r),
            Evidence::Unlocated(_) => None,
        }
    }

    #[must_use]
    pub const fn blindspot(self) -> Option<Blindspot> {
        match self {
            Evidence::At(_) => None,
            Evidence::Unlocated(b) => Some(b),
        }
    }

    #[must_use]
    pub const fn is_located(self) -> bool {
        matches!(self, Evidence::At(_))
    }
}

impl fmt::Display for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Evidence::At(r) => write!(f, "{r}"),
            Evidence::Unlocated(b) => write!(f, "<unlocated: {b}>"),
        }
    }
}

// ── Factors ───────────────────────────────────────────────────────────────

/// What kind of thing moved (or held) the reading.
///
/// A trait, not an enum: the kinds are the consumer's own vocabulary. blue has
/// capability declarations and seams; tatara has arity diagnostics and
/// malformed type specs. A shared enum would have to be the union of every
/// lisp's kinds, so every consumer would carry variants it can never emit.
pub trait FactorKind: Copy + fmt::Debug {
    /// The words a reader sees for this kind.
    fn label(self) -> &'static str;

    /// Does this push toward locked, or hold at loose?
    ///
    /// An author reading a list has to tell "this is what shifted me" from
    /// "this is what is holding me back" at a glance — the second list is the
    /// actionable one ([`Reading::holding_back`]).
    fn shifts_forward(self) -> bool;
}

/// One thing that moved (or held) the reading, and where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Factor<K> {
    pub kind: K,
    /// What it is — a declaration name, an input name, a call site.
    pub subject: String,
    /// Where to look. The reason a reading is not a bare score.
    pub evidence: Evidence,
    /// One line a status bar or hover can show verbatim.
    pub detail: String,
}

impl<K: FactorKind> Factor<K> {
    #[must_use]
    pub fn new(
        kind: K,
        subject: impl Into<String>,
        evidence: Evidence,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            subject: subject.into(),
            evidence,
            detail: detail.into(),
        }
    }
}

// ── The reading ───────────────────────────────────────────────────────────

/// The counts behind a reading — its denominator.
///
/// A rung without these is a verdict a reader has to take on trust. With them,
/// "checked" is `3/3`, and "annotated" is `2/7` with five named subjects to go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    /// Work the analysis actually did to produce this reading. The *cost* of
    /// where you are. Its meaning is the producer's to document — what it must
    /// not be is an estimate.
    pub analysed: usize,
    /// Subjects that met the criterion the top of the ladder describes.
    pub qualified: usize,
    /// Subjects examined. `qualified <= considered` always.
    pub considered: usize,
}

impl Measured {
    /// Every examined subject qualified — and at least one was examined. An
    /// empty program must not read as "fully qualified".
    #[must_use]
    pub const fn all_qualified(self) -> bool {
        self.considered > 0 && self.qualified == self.considered
    }

    #[must_use]
    pub const fn none_qualified(self) -> bool {
        self.qualified == 0
    }
}

/// Where a program sits, what put it there, and what that cost.
///
/// `rung` is `Option` because a program with nothing to measure has no
/// position: reporting the bottom rung would be a claim about a file whose
/// author never made the choice it describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading<K> {
    pub rung: Option<Rung>,
    pub factors: Vec<Factor<K>>,
    pub measured: Measured,
}

impl<K> Default for Reading<K> {
    fn default() -> Self {
        Self {
            rung: None,
            factors: Vec::new(),
            measured: Measured::default(),
        }
    }
}

impl<K: FactorKind> Reading<K> {
    /// The factors holding the program back, in the order the producer found
    /// them. What an author would act on to shift further.
    #[must_use]
    pub fn holding_back(&self) -> Vec<&Factor<K>> {
        self.factors
            .iter()
            .filter(|f| !f.kind.shifts_forward())
            .collect()
    }

    /// The factors that got the program to where it is.
    #[must_use]
    pub fn shifting_forward(&self) -> Vec<&Factor<K>> {
        self.factors
            .iter()
            .filter(|f| f.kind.shifts_forward())
            .collect()
    }

    /// Factors the producer could not locate. A non-empty list is a *stated*
    /// gap in the reading, not a defect in the program being read — surface it
    /// rather than letting it pass as ordinary.
    #[must_use]
    pub fn blind_spots(&self) -> Vec<&Factor<K>> {
        self.factors
            .iter()
            .filter(|f| !f.evidence.is_located())
            .collect()
    }

    /// Every factor points at real source.
    #[must_use]
    pub fn is_fully_located(&self) -> bool {
        self.factors.iter().all(|f| f.evidence.is_located())
    }

    /// One line for a status bar: position, then the denominator behind it.
    ///
    /// Neutral wording on purpose — this crate does not know whether the
    /// subjects are declarations, modules or routes. A consumer wanting its
    /// own nouns builds its own line from the same fields.
    #[must_use]
    pub fn summary(&self) -> String {
        let Some(rung) = self.rung else {
            return "no reading".to_string();
        };
        let mut out = rung.ramp();
        out.push(' ');
        out.push_str(rung.label());
        out.push_str("  (");
        out.push_str(&self.measured.qualified.to_string());
        out.push('/');
        out.push_str(&self.measured.considered.to_string());
        out.push_str(" qualified, ");
        out.push_str(&self.measured.analysed.to_string());
        out.push_str(" analysed)");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static FOUR: Ladder = Ladder::new(
        "four",
        &[
            Step::new("dynamic", "nothing is checked"),
            Step::new("annotated", "some of it is"),
            Step::new("checked", "all of it is"),
            Step::new("restricted", "all of it is, and the reach is bounded"),
        ],
    );

    static TWO: Ladder = Ladder::new(
        "two",
        &[Step::new("loose", "no"), Step::new("locked", "yes")],
    );

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        Forward,
        Holding,
    }

    impl FactorKind for Kind {
        fn label(self) -> &'static str {
            match self {
                Kind::Forward => "forward",
                Kind::Holding => "holding",
            }
        }
        fn shifts_forward(self) -> bool {
            matches!(self, Kind::Forward)
        }
    }

    fn factor(kind: Kind, subject: &str, evidence: Evidence) -> Factor<Kind> {
        Factor::new(kind, subject, evidence, "because")
    }

    // ── ladder + rung ─────────────────────────────────────────────────────

    #[test]
    fn a_rung_carries_its_ladders_vocabulary() {
        let r = FOUR.rung(2).expect("four has an index 2");
        assert_eq!(r.label(), "checked");
        assert_eq!(r.meaning(), "all of it is");
        assert_eq!(r.index(), 2);
        assert_eq!(r.ladder().name(), "four");
    }

    #[test]
    fn a_ladder_has_no_rung_past_its_top() {
        assert!(FOUR.rung(3).is_some());
        assert!(FOUR.rung(4).is_none(), "four steps, indices 0..=3");
        assert_eq!(FOUR.top().index(), 3);
        assert_eq!(FOUR.bottom().index(), 0);
    }

    /// Rungs are ordinal WITHIN a ladder — which is what makes "lowest wins"
    /// expressible at all.
    #[test]
    fn rungs_are_ordered_loose_to_locked() {
        let d = FOUR.bottom();
        let a = FOUR.rung(1).unwrap();
        let c = FOUR.rung(2).unwrap();
        let r = FOUR.top();
        assert!(d < a);
        assert!(a < c);
        assert!(c < r);
    }

    /// **And NOT across ladders.** "tatara is more locked than blue" is not a
    /// claim either ladder supports, so it gets no answer rather than a wrong
    /// one. This is the reason `Rung` is not a bare integer.
    #[test]
    fn rungs_from_different_ladders_do_not_compare() {
        let a = FOUR.rung(1).unwrap();
        let b = TWO.rung(1).unwrap();
        assert!(!a.same_ladder(b));
        assert_eq!(a.partial_cmp(&b), None);
        assert_ne!(a, b, "same index, different continuum — not the same rung");
        assert!(a.looser_of(b).is_none());
    }

    #[test]
    fn looser_of_takes_the_lowest_within_one_ladder() {
        let a = FOUR.rung(1).unwrap();
        let c = FOUR.rung(2).unwrap();
        assert_eq!(a.looser_of(c), Some(a));
        assert_eq!(c.looser_of(a), Some(a));
        assert_eq!(a.looser_of(a), Some(a));
    }

    /// The ramp fills with the rung and is sized by the ladder — a four-step
    /// ladder reproduces blue's mark exactly, which is what makes this core a
    /// generalisation of blue's reading rather than an approximation of it.
    #[test]
    fn the_ramp_fills_with_the_rung_and_is_sized_by_the_ladder() {
        assert_eq!(FOUR.rung(0).unwrap().ramp(), "█░░░");
        assert_eq!(FOUR.rung(1).unwrap().ramp(), "██░░");
        assert_eq!(FOUR.rung(2).unwrap().ramp(), "███░");
        assert_eq!(FOUR.rung(3).unwrap().ramp(), "████");
        assert_eq!(TWO.rung(0).unwrap().ramp(), "█░");
        assert_eq!(TWO.rung(1).unwrap().ramp(), "██");
    }

    #[test]
    fn a_rung_displays_as_its_label() {
        assert_eq!(FOUR.top().to_string(), "restricted");
    }

    // ── evidence ──────────────────────────────────────────────────────────

    /// **The load-bearing decision.** An unlocated factor states WHY, and the
    /// two reasons are different facts a reader must be able to tell apart.
    #[test]
    fn an_unlocated_factor_states_which_blind_spot_it_is() {
        let producer = Evidence::Unlocated(Blindspot::ProducerHasNoSpans);
        let synthetic = Evidence::Unlocated(Blindspot::Synthetic);
        assert_ne!(producer, synthetic);
        assert_eq!(producer.blindspot(), Some(Blindspot::ProducerHasNoSpans));
        assert_eq!(synthetic.blindspot(), Some(Blindspot::Synthetic));
        assert!(!producer.is_located());
        assert_eq!(producer.range(), None);
        assert!(producer
            .to_string()
            .contains("does not record source positions"));
        assert!(synthetic.to_string().contains("macro expansion"));
    }

    #[test]
    fn a_located_factor_hands_back_its_range() {
        let e = Evidence::At(ByteRange::new(4, 9));
        assert!(e.is_located());
        assert_eq!(e.range(), Some(ByteRange::new(4, 9)));
        assert_eq!(e.blindspot(), None);
        assert_eq!(e.to_string(), "4..9");
    }

    /// Half-open `[start, end)`, byte offsets — matching `tatara_lisp::Span`.
    #[test]
    fn a_byte_range_is_half_open_and_saturating() {
        assert_eq!(ByteRange::new(4, 9).len(), 5);
        assert!(ByteRange::new(4, 4).is_empty());
        assert_eq!(
            ByteRange::new(9, 4).len(),
            0,
            "a reversed range must not wrap into a colossal highlight"
        );
    }

    // ── reading ───────────────────────────────────────────────────────────

    fn mixed() -> Reading<Kind> {
        Reading {
            rung: FOUR.rung(1),
            factors: vec![
                factor(Kind::Forward, "a", Evidence::At(ByteRange::new(0, 3))),
                factor(Kind::Holding, "b", Evidence::At(ByteRange::new(4, 7))),
                factor(
                    Kind::Holding,
                    "c",
                    Evidence::Unlocated(Blindspot::ProducerHasNoSpans),
                ),
            ],
            measured: Measured {
                analysed: 12,
                qualified: 1,
                considered: 2,
            },
        }
    }

    /// The reading NAMES what is holding it back. An aggregate without this is
    /// a score, and a score nobody can act on is decoration.
    #[test]
    fn the_reading_names_what_is_holding_it_back() {
        let r = mixed();
        let held: Vec<&str> = r
            .holding_back()
            .iter()
            .map(|f| f.subject.as_str())
            .collect();
        assert_eq!(held, vec!["b", "c"]);
        let fwd: Vec<&str> = r
            .shifting_forward()
            .iter()
            .map(|f| f.subject.as_str())
            .collect();
        assert_eq!(fwd, vec!["a"]);
    }

    /// And it makes its own blind spots countable, which is the whole reason
    /// `Evidence` is not `Option`.
    #[test]
    fn the_reading_surfaces_its_own_blind_spots() {
        let r = mixed();
        assert!(!r.is_fully_located());
        let blind: Vec<&str> = r.blind_spots().iter().map(|f| f.subject.as_str()).collect();
        assert_eq!(blind, vec!["c"]);
    }

    #[test]
    fn a_fully_located_reading_reports_no_blind_spots() {
        let mut r = mixed();
        r.factors.pop();
        assert!(r.is_fully_located());
        assert!(r.blind_spots().is_empty());
    }

    #[test]
    fn the_summary_carries_the_position_and_its_denominator() {
        let line = mixed().summary();
        assert!(line.starts_with("██░░"), "carries the ramp: {line}");
        assert!(line.contains("annotated"), "{line}");
        assert!(line.contains("1/2 qualified"), "{line}");
        assert!(line.contains("12 analysed"), "{line}");
    }

    /// Nothing to measure is NOT the bottom rung — that would be a claim about
    /// a choice the author never made.
    #[test]
    fn a_reading_with_nothing_to_measure_has_no_rung() {
        let r = Reading::<Kind>::default();
        assert_eq!(r.rung, None);
        assert_eq!(r.summary(), "no reading");
        assert!(r.factors.is_empty());
    }

    #[test]
    fn an_empty_denominator_is_not_fully_qualified() {
        assert!(!Measured::default().all_qualified());
        assert!(Measured::default().none_qualified());
        assert!(Measured {
            analysed: 1,
            qualified: 3,
            considered: 3
        }
        .all_qualified());
    }
}

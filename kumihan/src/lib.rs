//! 組版 — **typesetting: the act of composing type onto the page.**
//!
//! One pretty-printing algebra, lisp-agnostic, shared by every pleme-io
//! emitter that has to decide where a line breaks.
//!
//! # Why this is its own crate
//!
//! Because it is the **second** consumer, which is where the PRIME DIRECTIVE
//! says to extract rather than copy. The algebra was written for
//! `blue-lang-fmt` (`src/doc.rs`), whose own header records the survey that
//! motivated it: *"Measured absent fleet-wide before this landed: no `pretty`
//! crate, no `Doc::group`, no Oppen implementation anywhere in pleme-io. Every
//! emitter in the fleet either concatenates strings or hand-rolls a shape
//! classifier, which is why `caixa-fmt` grew six ad-hoc `FormShape`s and still
//! could not generalize."*
//!
//! That prediction held. Counted 2026-08-17, four things print s-expressions
//! and no two share a line-breaking decision:
//!
//! | printer | how it breaks lines |
//! |---|---|
//! | `tatara_lisp::Sexp`'s `Display` | it does not — one flat line, always |
//! | `caixa-fmt` 0.1.157 `printer.rs` | `enum FormShape` at `:627`, Wadler's rule inlined by hand |
//! | `caixa-fmt` **=0.1.5** | what `tatara-kanmon`'s compile-time gate actually enforces |
//! | `blue-lang-fmt` | this algebra |
//!
//! A fifth would have been the wrong answer, so this is the fourth's algebra
//! lifted to where the other three can reach it.
//!
//! # Why it depends on nothing
//!
//! See `Cargo.toml`. The short version: `blue → tatara-lisp` and
//! `caixa-fmt → tatara-lisp`, so this algebra is a cycle from at least one
//! direction unless it depends on neither. Same structural position as
//! [`monosashi`], different subject — that one measures, this one composes.
//!
//! # The algebra
//!
//! The standard one (Wadler, *A prettier printer*, JFP 1998; Lindig, *Strictly
//! Pretty*, 2000) in its strict, linear-time form:
//!
//! - [`Doc::text`] — an atom, never broken
//! - [`Doc::line`] — a space when flat, a newline + indent when broken
//! - [`Doc::softline`] — nothing when flat, a newline + indent when broken
//! - [`Doc::concat`] — sequence
//! - [`Doc::nest`] — increase indentation for everything inside
//! - [`Doc::group`] — **the only decision point**: render flat if it fits the
//!   remaining width, otherwise break every `line` directly inside it
//!
//! # Determinism is the load-bearing property
//!
//! Given a document and a width, **exactly one output exists**. That is not an
//! aesthetic preference: a content-addressed identity over composed text needs
//! text and tree in bijection, and two renderings of one tree collapse it. It
//! is also what makes a formatter's idempotence and round-trip laws provable
//! rather than merely observed.
//!
//! [`monosashi`]: https://crates.io/crates/monosashi

use std::rc::Rc;

/// A document: a tree of text and break-candidates, not yet laid out.
///
/// Layout is deferred to [`pretty`], which is what separates *what* is being
/// composed from *how wide the page is* — the separation a string-concatenating
/// emitter cannot make, because it has already chosen its newlines.
#[derive(Clone, Debug)]
pub enum Doc {
    Nil,
    Text(Rc<str>),
    /// Break candidate. `flat` is what it renders as when the enclosing group
    /// fits on one line.
    Line {
        flat: &'static str,
    },
    Concat(Rc<Doc>, Rc<Doc>),
    Nest(isize, Rc<Doc>),
    Group(Rc<Doc>),
}

impl Doc {
    #[must_use]
    pub fn nil() -> Self {
        Doc::Nil
    }

    #[must_use]
    pub fn text(s: impl Into<Rc<str>>) -> Self {
        Doc::Text(s.into())
    }

    /// A space when flat; a newline when broken.
    #[must_use]
    pub fn line() -> Self {
        Doc::Line { flat: " " }
    }

    /// Nothing when flat; a newline when broken.
    #[must_use]
    pub fn softline() -> Self {
        Doc::Line { flat: "" }
    }

    /// Sequence two documents.
    ///
    /// `Nil` is absorbed on both sides, so building a document by folding over
    /// a possibly-empty sequence does not leave empty nodes behind for
    /// [`pretty`] to walk.
    #[must_use]
    pub fn concat(self, other: Doc) -> Self {
        match (&self, &other) {
            (Doc::Nil, _) => other,
            (_, Doc::Nil) => self,
            _ => Doc::Concat(Rc::new(self), Rc::new(other)),
        }
    }

    /// Indent everything inside by `indent` columns when it breaks.
    #[must_use]
    pub fn nest(self, indent: isize) -> Self {
        Doc::Nest(indent, Rc::new(self))
    }

    /// Mark a break decision point: flat if it fits, otherwise broken.
    #[must_use]
    pub fn group(self) -> Self {
        Doc::Group(Rc::new(self))
    }

    /// Join `docs` with `sep` between each pair.
    ///
    /// `sep` is borrowed rather than owned: it is cloned once per gap, so taking
    /// it by value would ask every caller to hand over a value the function then
    /// clones anyway. (blue-lang-fmt's original took it by value and clippy's
    /// `needless_pass_by_value` was right about it.)
    #[must_use]
    pub fn join(docs: impl IntoIterator<Item = Doc>, sep: &Doc) -> Doc {
        let mut out = Doc::Nil;
        let mut first = true;
        for d in docs {
            if !first {
                out = out.concat(sep.clone());
            }
            out = out.concat(d);
            first = false;
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

/// Render `doc` at `width` columns.
///
/// Linear in the size of the document: [`fits`] scans only far enough to decide
/// the current group, and never re-walks a decided one.
#[must_use]
pub fn pretty(doc: &Doc, width: usize) -> String {
    let mut out = String::new();
    // Work stack of (indent, mode, doc).
    let mut stack: Vec<(isize, Mode, Doc)> = vec![(0, Mode::Break, doc.clone())];
    let mut col: usize = 0;

    while let Some((indent, mode, d)) = stack.pop() {
        match d {
            Doc::Nil => {}
            Doc::Text(s) => {
                out.push_str(&s);
                col += s.chars().count();
            }
            Doc::Line { flat } => match mode {
                Mode::Flat => {
                    out.push_str(flat);
                    col += flat.chars().count();
                }
                Mode::Break => {
                    out.push('\n');
                    let pad = usize::try_from(indent.max(0)).unwrap_or(0);
                    for _ in 0..pad {
                        out.push(' ');
                    }
                    col = pad;
                }
            },
            Doc::Concat(a, b) => {
                stack.push((indent, mode, (*b).clone()));
                stack.push((indent, mode, (*a).clone()));
            }
            Doc::Nest(n, inner) => {
                stack.push((indent + n, mode, (*inner).clone()));
            }
            Doc::Group(inner) => {
                // The single decision: does this group fit flat?
                let m = if fits(width.saturating_sub(col), &inner, &stack) {
                    Mode::Flat
                } else {
                    Mode::Break
                };
                stack.push((indent, m, (*inner).clone()));
            }
        }
    }
    out
}

/// Would `doc`, rendered flat, fit in `space` columns — accounting for whatever
/// already-queued work follows it up to the next break?
fn fits(space: usize, doc: &Doc, rest: &[(isize, Mode, Doc)]) -> bool {
    let mut remaining = isize::try_from(space).unwrap_or(isize::MAX);
    let mut local: Vec<(Mode, Doc)> = vec![(Mode::Flat, doc.clone())];
    // Trailing work, innermost first.
    let mut tail_idx = rest.len();

    loop {
        let (mode, d) = if let Some(x) = local.pop() {
            x
        } else {
            // Exit paths must re-check the budget: the last item popped may
            // have overrun it, and returning `true` here without checking
            // was a real bug the group tests caught.
            if tail_idx == 0 {
                return remaining >= 0;
            }
            tail_idx -= 1;
            let (_, m, d) = &rest[tail_idx];
            (*m, d.clone())
        };
        if remaining < 0 {
            return false;
        }
        match d {
            Doc::Nil => {}
            Doc::Text(s) => remaining -= isize::try_from(s.chars().count()).unwrap_or(isize::MAX),
            Doc::Line { flat } => match mode {
                // A break in the trailing context ends the line, so everything
                // up to here fits — provided it actually did.
                Mode::Break => return remaining >= 0,
                Mode::Flat => {
                    remaining -= isize::try_from(flat.chars().count()).unwrap_or(isize::MAX);
                }
            },
            Doc::Concat(a, b) => {
                local.push((mode, (*b).clone()));
                local.push((mode, (*a).clone()));
            }
            Doc::Nest(_, inner) => local.push((mode, (*inner).clone())),
            // A nested group is measured flat, which is what makes this
            // Oppen's linear algorithm rather than Wadler's exponential one.
            Doc::Group(inner) => local.push((Mode::Flat, (*inner).clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Doc {
        Doc::text(s.to_string())
    }

    #[test]
    fn text_renders_verbatim() {
        assert_eq!(pretty(&t("hello"), 80), "hello");
    }

    #[test]
    fn a_group_that_fits_stays_flat() {
        let d = t("a").concat(Doc::line()).concat(t("b")).group();
        assert_eq!(pretty(&d, 80), "a b");
    }

    #[test]
    fn a_group_that_does_not_fit_breaks_every_line_inside_it() {
        // The all-or-nothing rule: a group does not break *some* of its lines.
        // Partial breaking is what produces the ragged output a hand-rolled
        // shape classifier gives you.
        let d = t("aaaa").concat(Doc::line()).concat(t("bbbb")).group();
        assert_eq!(pretty(&d, 5), "aaaa\nbbbb");
    }

    #[test]
    fn nest_indents_only_what_is_inside_it() {
        let inner = Doc::line().concat(t("x")).nest(2);
        let d = t("(").concat(inner).concat(Doc::line()).concat(t(")")).group();
        // Width 3 forces the break; `x` carries the nest, `)` does not.
        assert_eq!(pretty(&d, 3), "(\n  x\n)");
    }

    #[test]
    fn softline_disappears_when_flat_and_breaks_when_not() {
        let d = t("a").concat(Doc::softline()).concat(t("b")).group();
        assert_eq!(pretty(&d, 80), "ab", "flat softline contributes nothing");
        assert_eq!(pretty(&d, 1), "a\nb", "broken softline is a newline");
    }

    #[test]
    fn a_nested_group_may_stay_flat_inside_a_broken_parent() {
        // The property that makes this an algebra rather than a switch: each
        // group decides independently, so an outer break does not force inner
        // ones.
        let inner = t("c").concat(Doc::line()).concat(t("d")).group();
        let d = t("aaaaaaaa")
            .concat(Doc::line())
            .concat(inner)
            .group();
        assert_eq!(pretty(&d, 10), "aaaaaaaa\nc d");
    }

    #[test]
    fn the_trailing_context_is_counted_when_deciding_a_group() {
        // The bug the `fits` exit-path comment records. A group is measured
        // together with the queued work that follows it up to the next break,
        // so a group that would fit ALONE still breaks when what trails it
        // would overrun the line. Without this, output exceeds the width.
        let g = t("ab").concat(Doc::line()).concat(t("cd")).group();
        let d = g.concat(t("!!!!!!!!"));
        let rendered = pretty(&d, 10);
        assert!(
            rendered.lines().all(|l| l.chars().count() <= 10),
            "no line may exceed the width, got {rendered:?}"
        );
    }

    #[test]
    fn nil_is_absorbed_on_both_sides() {
        assert_eq!(pretty(&Doc::nil().concat(t("x")), 80), "x");
        assert_eq!(pretty(&t("x").concat(Doc::nil()), 80), "x");
        assert_eq!(pretty(&Doc::nil(), 80), "");
    }

    #[test]
    fn join_separates_only_between_pairs() {
        let d = Doc::join([t("a"), t("b"), t("c")], &t(", "));
        assert_eq!(pretty(&d, 80), "a, b, c");
        // Empty and single sequences must not emit a stray separator.
        assert_eq!(pretty(&Doc::join([], &t(", ")), 80), "");
        assert_eq!(pretty(&Doc::join([t("a")], &t(", ")), 80), "a");
    }

    #[test]
    fn rendering_is_deterministic_for_one_document_and_width() {
        // The load-bearing property: a content-addressed identity over composed
        // text requires text and tree in bijection. Two renderings of one tree
        // at one width would collapse that.
        let d = t("(")
            .concat(Doc::join([t("x"), t("y"), t("z")], &Doc::line()).nest(2))
            .concat(t(")"))
            .group();
        let first = pretty(&d, 7);
        for _ in 0..8 {
            assert_eq!(pretty(&d, 7), first);
        }
    }

    #[test]
    fn width_is_measured_in_chars_not_bytes() {
        // A multi-byte glyph occupies one column here. Measuring bytes would
        // break lines early on any non-ASCII source — and this corpus is full
        // of CJK, ★ and §.
        let d = t("交換").concat(Doc::line()).concat(t("ab")).group();
        assert_eq!(pretty(&d, 5), "交換 ab", "2 chars + space + 2 chars fits in 5");
    }

    #[test]
    fn a_zero_width_page_breaks_every_group_without_panicking() {
        // Degenerate input must degrade, not abort: `saturating_sub` on the
        // column budget is what keeps this from underflowing.
        let d = t("a").concat(Doc::line()).concat(t("b")).group();
        assert_eq!(pretty(&d, 0), "a\nb");
    }
}

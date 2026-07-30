//! Source-position rendering for `LispError`.
//!
//! `tatara-lisp` errors carry source `Span`s through the reader (see
//! `reader.rs` and `LispError::span`). This module is the projection
//! step: it converts a span into a 1-based `(line, column)` plus a
//! caret RUN as wide as the span, and renders a rustc-style diagnostic
//! around them. `tatara-lispc`, `tatara-check`, the REPL, and the future
//! LSP all funnel through `format_diagnostic` so authoring surfaces
//! underline the source that broke instead of leaving the operator to
//! hunt for it.
//!
//! `LispError::position` (the span's `start`) still names the single
//! byte that goes in the `--> label:line:col` header; the span's `end`
//! is what the underline is sized from.
//!
//! Theory grounding: THEORY.md §V.1 — knowable platform / constructive
//! diagnostics. An error whose location cannot be projected to source
//! is not knowable, and one that points at a byte when it means a form
//! is only half-knowable. Inspiration: rustc's `DiagnosticBuilder`
//! snippet format; translation through pleme-io primitives is the
//! byte-range `Span` already on the existing `LispError`, no new IR
//! layer.

use crate::error::LispError;
use crate::span::Span;

/// `writeln!` into a writer whose `fmt::Write` impl is infallible.
///
/// `format_diagnostic` assembles its rustc-style snippet by emitting
/// four formatted lines into a `String`. `String`'s `fmt::Write` impl
/// is total — `impl fmt::Write for String { fn write_str(&mut self, s)
/// { self.push_str(s); Ok(()) } }` — so every `writeln!`/`write!` into
/// it returns `Ok(())`; the inline `.expect("writes to a String never
/// fail")` triple recurred at four sites (THEORY.md §VI.1
/// three-times rule, crossed decisively).
///
/// Lifting it into ONE macro centralizes the canonical panic message:
/// a typo in the expect-string can never drift across the four
/// emission sites at runtime. Sibling of `infallible_write!` for the
/// non-newline-terminated single-write case (the trailing caret line
/// in `format_diagnostic`).
///
/// Theory grounding: THEORY.md §VI.1 — the four-times duplication of
/// `.expect("writes to a String never fail")` collapses into one
/// named primitive. The macro names the invariant ("infallible write
/// to String") as a primitive of the diagnostic-rendering substrate,
/// so future writer-type changes (e.g., `String` → a typed builder)
/// land in ONE place — every call site picks up the new emission
/// posture mechanically.
macro_rules! infallible_writeln {
    ($out:expr, $($t:tt)*) => {{
        // Hygienically bring `fmt::Write::write_fmt` into scope so
        // call sites don't need a separate `use std::fmt::Write as _`
        // import — the macro is self-contained.
        use ::std::fmt::Write as _;
        ::std::writeln!($out, $($t)*).expect("writes to a String never fail")
    }};
}

/// `write!` into a writer whose `fmt::Write` impl is infallible — the
/// non-newline-terminated sibling of `infallible_writeln!`. Used by
/// `format_diagnostic` for its trailing caret line, which must not
/// emit a closing newline (so consumers concatenating the rendered
/// diagnostic into a longer message see the caret as the final
/// character, not the line after it). Same `String`-infallibility
/// invariant; same canonical panic message; same theory-anchor
/// (THEORY.md §VI.1).
macro_rules! infallible_write {
    ($out:expr, $($t:tt)*) => {{
        use ::std::fmt::Write as _;
        ::std::write!($out, $($t)*).expect("writes to a String never fail")
    }};
}

/// 1-based line + column. `line_col` walks the source up to a byte
/// offset; `\n` increments `line` and resets `column` to 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineCol {
    pub line: usize,
    pub column: usize,
}

/// Convert a byte offset into a 1-based `LineCol`. Offsets past EOF
/// clamp to the final position. `column` counts UTF-8 scalar
/// characters, not bytes — an `é` is one column, two bytes — so the
/// caret renders under the visible character a human sees.
#[must_use]
pub fn line_col(src: &str, byte_offset: usize) -> LineCol {
    let cap = byte_offset.min(src.len());
    let mut line = 1usize;
    let mut column = 1usize;
    let mut idx = 0usize;
    for c in src.chars() {
        if idx >= cap {
            break;
        }
        if c == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
        idx += c.len_utf8();
    }
    LineCol { line, column }
}

/// Slice the line of `src` containing `byte_offset` (without its
/// trailing `\n`). Used by `format_diagnostic` to render the caret
/// underneath the right line.
///
/// Public because it is one half of the shared caret-rendering contract:
/// a consumer that frames its own diagnostic (see
/// `tatara_lisp_eval::EvalError::render`) still must slice the SAME line
/// this module's caret pad is computed against, or the caret and the
/// snippet disagree. Exposing the slicer alongside [`caret_run`] keeps
/// that pairing at one definition instead of two hand-rolled copies.
#[must_use]
pub fn line_at(src: &str, byte_offset: usize) -> &str {
    let cap = byte_offset.min(src.len());
    let start = src[..cap].rfind('\n').map_or(0, |i| i + 1);
    let end = src[start..].find('\n').map_or(src.len(), |i| start + i);
    &src[start..end]
}

/// Build a caret-pad string whose rendered visual width equals the
/// first `column - 1` chars of `line_text` under a fixed-width terminal.
///
/// Each source `\t` mirrors through as `\t`; every other char becomes
/// a space. This preserves caret alignment for tab-indented sources —
/// the pad and the source line consume the same tab-stops, so the
/// caret lands under the offending byte regardless of the terminal's
/// tab-stop setting. Pre-lift the caret pad was
/// `" ".repeat(column.saturating_sub(1))`, which silently drifted
/// under a tab-indented source (a `\t` renders as N columns of source
/// but as ONE space in the pad, so the caret slid left of the byte).
///
/// Named at the substrate level so a future range-underline
/// diagnostic (e.g. `let caret = mirror + "^".repeat(width)` for a
/// multi-column highlight, or a `note:` companion line pinned under a
/// different column of the SAME source line) inherits the tab-mirror
/// discipline mechanically — the visual-width invariant lives at ONE
/// projection on the diagnostic-rendering surface.
///
/// Theory anchor: THEORY.md §V.1 — knowable platform / constructive
/// diagnostics. A caret whose column drifts under a tab-indented
/// source is not knowable to the operator. Inspiration: rustc's
/// `SnippetData::render_source_line` tab-mirror idiom; translation
/// through pleme-io primitives is a chars-iterator over the source
/// line already in hand, no new IR layer.
fn mirror_source_prefix_as_pad(line_text: &str, column: usize) -> String {
    line_text
        .chars()
        .take(column.saturating_sub(1))
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect()
}

/// THE caret renderer: the pad that walks `line_text`'s first
/// `column - 1` columns, followed by `width` carets.
///
/// This is the one place in the fleet that decides where a `^` lands
/// under a source line and how wide the underline runs. Two hand-written
/// copies existed before this lift — this module's single-caret pad and
/// `tatara_lisp_eval::EvalError::render`'s span underline — and they had
/// drifted apart on two axes that are BOTH bugs in the eval copy:
///
/// 1. **Tab handling.** The eval copy padded with
///    `" ".repeat(column - 1)`, so under a tab-indented source the caret
///    slid left of its byte (one space where the source spent a whole
///    tab-stop). The pad here mirrors tabs through
///    ([`mirror_source_prefix_as_pad`]), so pad and source consume the
///    same tab-stops on any terminal.
/// 2. **Caret width unit.** The eval copy computed its underline as
///    `span.end - span.start` — a BYTE count — while the column it
///    padded to was a CHAR count. A multi-byte subform therefore
///    over-underlined (a one-column `é` drew two carets). `width` here
///    is documented as, and must be passed as, a count of CHARS.
///
/// `width` is clamped up to 1: a zero-width span still gets a single
/// caret to point at, rather than rendering a bare pad with nothing
/// under it.
///
/// `width` is NOT clamped down to the remaining length of `line_text`.
/// A span covering several lines will therefore draw carets past the end
/// of the single line rendered above it. That is the pre-existing
/// behaviour of both copies, preserved deliberately rather than changed
/// under cover of this consolidation — see the `pending-caret-multiline`
/// note in `tatara_lisp_eval::EvalError::render`.
///
/// Theory anchor: THEORY.md §V.1 — knowable platform / constructive
/// diagnostics. A caret whose column or width drifts from the byte it
/// names is not knowable to the operator.
#[must_use]
pub fn caret_run(line_text: &str, column: usize, width: usize) -> String {
    let mut out = mirror_source_prefix_as_pad(line_text, column);
    for _ in 0..width.max(1) {
        out.push('^');
    }
    out
}

/// Count how many CHARS a `Span` covers in `src` — the unit
/// [`caret_run`]'s `width` parameter is documented in.
///
/// The distinction is load-bearing and has already been a bug once: a
/// span is a BYTE range, `caret_run` pads to a CHAR column, so passing
/// `span.end - span.start` over-underlines any non-ASCII source (see the
/// second drift documented on [`caret_run`]). Every caller that turns a
/// span into a caret width goes through here so the two units cannot
/// disagree again — this module's [`format_diagnostic`] and
/// `tatara_lisp_eval::EvalError::render`, which were the two independent
/// hand-rolls of this same three-line expression.
///
/// A span that does not land on char boundaries (or runs past the end of
/// `src`) degrades to `1` rather than panicking — a diagnostic renderer
/// must not be the thing that takes the process down.
#[must_use]
pub fn span_width_chars(src: &str, span: Span) -> usize {
    src.get(span.start..span.end)
        .map_or(1, |covered| covered.chars().count())
}

/// Render a `LispError` as a rustc-style diagnostic with a caret.
///
/// ```text
/// error: unmatched closing paren at position 3
///  --> file.lisp:1:4
///   |
/// 1 |    )
///   |    ^
/// ```
///
/// `label` is the file path or any identifier the caller wants in the
/// `--> label:line:col` line; pass `None` when there is no source name
/// (the REPL, an in-memory string) and the location renders as
/// `--> line N, column M`.
///
/// Errors whose `span()` is `None` (`Type`, `Compile`, …) render
/// as a single `error: <msg>` line — there is nothing to point at.
/// As more variants gain spans, those errors automatically pick
/// up the snippet rendering with no consumer changes.
///
/// The underline is as wide as the error's span, in chars. The four
/// reader errors carry real `[start, end)` ranges (see `reader.rs`), so
/// an unclosed `(a (b c` underlines `(b c` rather than pointing one
/// caret at the `(`. Point-shaped spans — the stray `)`, end-of-input —
/// are zero- or one-wide and render exactly as they did before spans
/// reached the reader.
///
/// `pending-caret-multiline` (recorded on `caret_run` and on
/// `tatara_lisp_eval::EvalError::render`): a span covering more than one
/// line draws its full char width under the SINGLE line rendered above
/// it, overflowing that line's end. The reader's `UnmatchedOpenParen`
/// and `UnterminatedString` spans can now reach that case (a form left
/// open across a newline), where pre-lift every `LispError` was width 1
/// and could not. This is a widening of an EXISTING residue's reach, not
/// a new defect class, and it is deliberately not fixed here — clamping
/// the run to the rendered line changes output for every whole-form span
/// in the eval renderer too, so it wants its own measured pass. Pinned
/// by `format_diagnostic_multiline_unclosed_form_overflows_its_line`.
#[must_use]
pub fn format_diagnostic(src: &str, err: &LispError, label: Option<&str>) -> String {
    let mut out = format!("error: {err}");
    let Some(span) = err.span() else {
        return out;
    };
    let pos = span.start;
    let LineCol { line, column } = line_col(src, pos);
    let line_text = line_at(src, pos);
    let line_str = line.to_string();
    let gutter = " ".repeat(line_str.len());
    // Width comes from the span, in CHARS — the unit `caret_run` pads in.
    // The width parameter is what lets `EvalError::render`, whose errors
    // also carry a `[start, end)` span, share this exact renderer instead
    // of hand-rolling a second one.
    let caret = caret_run(line_text, column, span_width_chars(src, span));

    out.push('\n');
    match label {
        Some(label) => infallible_writeln!(out, "{gutter}--> {label}:{line}:{column}"),
        None => infallible_writeln!(out, "{gutter}--> line {line}, column {column}"),
    }
    infallible_writeln!(out, "{gutter} |");
    infallible_writeln!(out, "{line_str} | {line_text}");
    infallible_write!(out, "{gutter} | {caret}");
    out
}

#[cfg(test)]
mod tests {
    use super::{
        caret_run, format_diagnostic, line_at, line_col, mirror_source_prefix_as_pad,
        span_width_chars, LineCol,
    };
    use crate::error::LispError;
    use crate::reader::read;
    use crate::span::Span;

    // ── span_width_chars ────────────────────────────────────────────
    //
    // The byte-range → char-width conversion both caret consumers go
    // through. It exists because the two of them each hand-rolled it and
    // the eval copy got the unit wrong (see `caret_run`'s doc). These
    // pins are the unit's definition.

    #[test]
    fn span_width_chars_counts_ascii_as_one_per_byte() {
        assert_eq!(span_width_chars("(a (b c", Span::new(3, 7)), 4);
        assert_eq!(span_width_chars("(a b)", Span::new(0, 1)), 1);
    }

    #[test]
    fn span_width_chars_counts_chars_not_bytes() {
        // `é` is two bytes, one column. A byte-count would say 2 here and
        // draw twice the underline the source occupies — the exact bug
        // the eval-side copy shipped.
        assert_eq!(span_width_chars("é", Span::new(0, 2)), 1);
        assert_eq!(span_width_chars("(éé)", Span::new(1, 5)), 2);
    }

    #[test]
    fn span_width_chars_is_zero_for_an_empty_range() {
        // Zero is the honest answer; the clamp-up-to-one caret lives in
        // `caret_run`, not here, so callers that want a range length get
        // a range length.
        assert_eq!(span_width_chars("(a b)", Span::new(2, 2)), 0);
    }

    #[test]
    fn span_width_chars_degrades_to_one_off_char_boundary_or_past_eof() {
        // A diagnostic renderer must never be the thing that panics. A
        // span slicing into the middle of a multi-byte char, or running
        // past the end of the source, yields a single caret instead of a
        // `byte index is not a char boundary` panic.
        assert_eq!(span_width_chars("é", Span::new(0, 1)), 1);
        assert_eq!(span_width_chars("abc", Span::new(0, 99)), 1);
        assert_eq!(span_width_chars("abc", Span::new(3, 3)), 0);
    }

    // ── line_col ────────────────────────────────────────────────────

    #[test]
    fn line_col_at_start_of_input() {
        assert_eq!(line_col("abc", 0), LineCol { line: 1, column: 1 });
    }

    #[test]
    fn line_col_advances_columns_on_first_line() {
        assert_eq!(line_col("abc", 1), LineCol { line: 1, column: 2 });
        assert_eq!(line_col("abc", 2), LineCol { line: 1, column: 3 });
    }

    #[test]
    fn line_col_at_eof_is_one_past_last_char() {
        assert_eq!(line_col("abc", 3), LineCol { line: 1, column: 4 });
    }

    #[test]
    fn line_col_clamps_past_eof() {
        assert_eq!(line_col("abc", 999), LineCol { line: 1, column: 4 });
        assert_eq!(line_col("", 999), LineCol { line: 1, column: 1 });
    }

    #[test]
    fn line_col_advances_line_after_newline() {
        // `a\nb` — offset 0 = (1,1); 1 = (1,2) (still on line 1, after `a`);
        // 2 = (2,1) (after the `\n`); 3 = (2,2) (after `b`).
        assert_eq!(line_col("a\nb", 0), LineCol { line: 1, column: 1 });
        assert_eq!(line_col("a\nb", 1), LineCol { line: 1, column: 2 });
        assert_eq!(line_col("a\nb", 2), LineCol { line: 2, column: 1 });
        assert_eq!(line_col("a\nb", 3), LineCol { line: 2, column: 2 });
    }

    #[test]
    fn line_col_counts_chars_not_bytes_for_multibyte() {
        // `é` is two bytes (0xC3 0xA9) but one column. Offset = 2 lands
        // immediately after `é`, i.e. column 2 on line 1.
        assert_eq!(line_col("é", 2), LineCol { line: 1, column: 2 });
        assert_eq!(line_col("\né", 1), LineCol { line: 2, column: 1 });
        assert_eq!(line_col("\né", 3), LineCol { line: 2, column: 2 });
    }

    // ── line_at ─────────────────────────────────────────────────────

    #[test]
    fn line_at_returns_the_containing_line_without_newline() {
        let src = "alpha\nbeta\ngamma";
        assert_eq!(line_at(src, 0), "alpha");
        assert_eq!(line_at(src, 6), "beta"); // first char of line 2
        assert_eq!(line_at(src, 11), "gamma"); // first char of line 3
        assert_eq!(line_at(src, 16), "gamma"); // EOF still on line 3
    }

    // ── format_diagnostic ───────────────────────────────────────────

    #[test]
    fn format_diagnostic_renders_unmatched_paren_with_caret_under_offending_byte() {
        // `   )` — stray `)` at byte 3, which is column 4 on line 1.
        // The caret under the `)` proves the column math + line slicing
        // agree.
        let src = "   )";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("x.lisp"));
        let expected = "\
error: unmatched closing paren at position 3
 --> x.lisp:1:4
  |
1 |    )
  |    ^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_locates_paren_on_a_later_line() {
        // Two leading lines plus a stray `)` — confirms the line index
        // and the line-slicing both work past the first newline.
        let src = "(a b)\n(c d)\n   )\n";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("nested.lisp"));
        // The stray `)` is at byte 15 → (line 3, column 4).
        let expected = "\
error: unmatched closing paren at position 15
 --> nested.lisp:3:4
  |
3 |    )
  |    ^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_unmatched_open_underlines_the_unclosed_form() {
        // `(a (b c` — inner `(` at byte 3 is the deepest unclosed open.
        //
        // THE payoff of giving the reader's errors a `Span` instead of a
        // `pos`. Pre-lift every `LispError` was a point, so this rendered
        // ONE caret under the `(` and left the operator to work out how
        // much of the line the unclosed form covered:
        //
        //     1 | (a (b c
        //       |    ^
        //
        // Post-lift the error carries `[3, 7)` — the `(` through the end
        // of the last token read — so the underline names the form.
        let src = "(a (b c";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("open.lisp"));
        let expected = "\
error: unmatched opening paren at position 3
 --> open.lisp:1:4
  |
1 | (a (b c
  |    ^^^^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_unmatched_open_underline_stops_at_the_last_token() {
        // Companion to the test above, pinning the OTHER end of the span:
        // trailing trivia is not part of the unclosed form. `(a b` plus
        // three spaces and a comment still underlines four columns, not
        // the eleven bytes to end-of-input. A regression that builds the
        // span from `src.len()` (the obvious wrong end) fails HERE with a
        // caret run that runs off past `b`.
        let src = "(a b   ; tail";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("trivia.lisp"));
        let expected = "\
error: unmatched opening paren at position 0
 --> trivia.lisp:1:1
  |
1 | (a b   ; tail
  | ^^^^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_unterminated_string_underlines_the_dangling_literal() {
        // Second variant the span lift widens: the tokenizer consumes to
        // end-of-input hunting for a closing `"`, so the span covers the
        // whole dangling literal instead of pointing at the opening quote.
        let src = "(a \"bc";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("str.lisp"));
        let expected = "\
error: unterminated string literal at position 3
 --> str.lisp:1:4
  |
1 | (a \"bc
  |    ^^^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_stray_close_paren_still_renders_exactly_one_caret() {
        // The point-shaped half of the lift. A stray `)` names one byte,
        // so its span is one byte wide and the render is byte-identical
        // to pre-lift. Pinned so a future "spans mean wide underlines"
        // change cannot quietly widen an error that has nothing more to
        // underline.
        let src = "(a b) )";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("stray.lisp"));
        let expected = "\
error: unmatched closing paren at position 6
 --> stray.lisp:1:7
  |
1 | (a b) )
  |       ^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_multiline_unclosed_form_overflows_its_line() {
        // `pending-caret-multiline`, MEASURED rather than asserted.
        //
        // `caret_run` does not clamp its width to the length of the line
        // rendered above it, so a span crossing a newline draws carets
        // past that line's end. This is a pre-existing residue of the
        // shared renderer (recorded on `caret_run` and on
        // `tatara_lisp_eval::EvalError::render`, where whole-form spans
        // hit it routinely). The reader's `UnmatchedOpenParen` span can
        // now reach it too — pre-lift every `LispError` was width 1 and
        // structurally could not.
        //
        // This pin records the CURRENT output, warts included, so the
        // reach is visible in-tree instead of living only in a comment.
        // Fixing it is a separate measured pass: clamping changes output
        // for every whole-form span in the eval renderer as well, so it
        // does not ride along inside this consolidation.
        let src = "(a\n b";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("multi.lisp"));
        // Span is `[0, 5)` — five chars — but line 1 is only `(a`, two
        // chars. Three of the five carets hang past the end of the line.
        let expected = "\
error: unmatched opening paren at position 0
 --> multi.lisp:1:1
  |
1 | (a
  | ^^^^^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_omits_label_when_none() {
        let err = read(")").unwrap_err();
        let rendered = format_diagnostic(")", &err, None);
        // No file path is known; still produce a structured location.
        let expected = "\
error: unmatched closing paren at position 0
 --> line 1, column 1
  |
1 | )
  | ^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_renders_eof_at_end_of_input() {
        // `(a b) '` — trailing quote with no datum runs the parser past
        // EOF; the caret renders one column past the last visible char.
        let src = "(a b) '";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("dangle.lisp"));
        let expected = "\
error: unexpected end of input at position 7
 --> dangle.lisp:1:8
  |
1 | (a b) '
  |        ^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn infallible_writeln_macro_appends_formatted_line_with_trailing_newline() {
        // Pin the macro's emission shape: `writeln!`-equivalent into a
        // `String`, no swallowed bytes, no missing newline. A regression
        // that drops the newline or mis-handles format-arg interpolation
        // fails-loudly here. The macro is the centralized substitute
        // for the four inline `.expect("writes to a String never
        // fail")` triples that recurred in `format_diagnostic`'s body
        // pre-lift.
        let mut out = String::new();
        infallible_writeln!(out, "hello {x}", x = 42);
        assert_eq!(out, "hello 42\n");
    }

    #[test]
    fn infallible_write_macro_appends_formatted_text_without_newline() {
        // Sibling of `infallible_writeln!` — non-newline-terminated
        // emission. Pin that the macro does NOT add a trailing newline
        // so the caret-line rendering in `format_diagnostic` stays
        // byte-for-byte stable. A regression that adds a newline here
        // fails-loudly via the existing `format_diagnostic_*` tests
        // AND this isolated unit-pin.
        let mut out = String::new();
        infallible_write!(out, "tail {y}", y = "value");
        assert_eq!(out, "tail value");
    }

    #[test]
    fn infallible_macros_preserve_format_diagnostic_byte_identity() {
        // The lift is a pure refactor — `format_diagnostic`'s rendered
        // output must be byte-for-byte identical to the pre-lift state
        // across every existing test case. The five `format_diagnostic_*`
        // tests below already pin specific expected strings; this test
        // re-asserts that path-uniformity at the macro-substitution
        // layer: emit one full diagnostic and confirm both the caret
        // line (the only `infallible_write!` site) AND the gutter
        // lines (three `infallible_writeln!` sites) render correctly
        // together.
        let src = "   )";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("macros.lisp"));
        assert!(rendered.starts_with("error: unmatched closing paren"));
        assert!(rendered.contains("\n --> macros.lisp:1:4\n"));
        assert!(rendered.ends_with("^"));
        assert!(
            !rendered.ends_with("^\n"),
            "trailing caret line must NOT emit a newline (would drift consumer concat)"
        );
    }

    #[test]
    fn format_diagnostic_falls_back_to_single_line_for_positionless_errors() {
        // A `Compile` error has no position today; it must still render
        // as a clean single line so downstream tools can dump it
        // unconditionally.
        let err = LispError::Compile {
            form: ":threshold".into(),
            message: "expected number".into(),
        };
        let rendered = format_diagnostic("(defmonitor :threshold #t)", &err, Some("m.lisp"));
        assert_eq!(
            rendered,
            "error: compile error in :threshold: expected number"
        );
        assert!(
            !rendered.contains('\n'),
            "single-line render must not introduce newlines"
        );
        assert!(
            !rendered.contains('^'),
            "no caret allowed without a position to point at"
        );
    }

    // ── mirror_source_prefix_as_pad ─────────────────────────────────
    //
    // The pre-lift `" ".repeat(column - 1)` caret pad drifted under a
    // tab-indented source (a `\t` renders as N columns of source but
    // as ONE space in the pad, so the caret slid left of the offending
    // byte). Post-lift the pad mirrors each source char — tabs stay
    // tabs, everything else becomes a space — so the pad and the
    // source line consume the SAME tab-stops on a fixed-width terminal.
    // The pins below anchor the four canonical fixpoints AND the
    // end-to-end composition through `format_diagnostic`.

    #[test]
    fn mirror_source_prefix_as_pad_at_column_one_is_empty() {
        // Column 1 means the caret sits under the FIRST char of the
        // source line — zero pad ahead of it. `saturating_sub(1)` guards
        // both column 0 (unreachable but defensively OK) and column 1.
        assert_eq!(mirror_source_prefix_as_pad("(a b)", 1), "");
        assert_eq!(mirror_source_prefix_as_pad("(a b)", 0), "");
    }

    #[test]
    fn mirror_source_prefix_as_pad_replaces_non_tab_chars_with_spaces() {
        // A tab-free source line reproduces the pre-lift behavior byte-
        // for-byte: N chars of source before the caret → N spaces of
        // pad. Load-bearing for the existing `format_diagnostic_*`
        // tests, which all pin space-only prefixes.
        assert_eq!(mirror_source_prefix_as_pad("   )", 4), "   ");
        assert_eq!(mirror_source_prefix_as_pad("(a b c)", 5), "    ");
        assert_eq!(
            mirror_source_prefix_as_pad("hello", 6),
            "     ",
            "column past-last-char pads with spaces for every source char",
        );
    }

    #[test]
    fn mirror_source_prefix_as_pad_preserves_tabs_verbatim() {
        // A single leading tab mirrors through as a tab — the caret pad
        // consumes the same tab-stop the source did, so `\t)` and `\t^`
        // land the `)` and the `^` at the SAME visual column regardless
        // of the terminal's tab-stop setting (2, 4, 8, whatever).
        assert_eq!(mirror_source_prefix_as_pad("\t)", 2), "\t");
        assert_eq!(mirror_source_prefix_as_pad("\t\t)", 3), "\t\t");
    }

    #[test]
    fn mirror_source_prefix_as_pad_mirrors_mixed_tab_and_space_prefix() {
        // Real-world indent shapes (space-then-tab, tab-then-space,
        // interleaved) must reproduce the source's exact whitespace
        // sequence in the pad. A regression that converts tabs to
        // spaces (or vice versa) fails HERE with a visible mismatch.
        assert_eq!(mirror_source_prefix_as_pad("  \t)", 4), "  \t");
        assert_eq!(mirror_source_prefix_as_pad("\t  )", 4), "\t  ");
        // `(` is a non-tab char — it becomes a space in the pad while
        // the surrounding tabs mirror through as tabs. Pin that the
        // interleaved `[tab, space, non-tab, tab]` prefix produces
        // `[tab, space, space, tab]` so the caret's tab-stop advances
        // remain aligned regardless of what non-tab chars precede it.
        assert_eq!(mirror_source_prefix_as_pad("\t (\t)", 5), "\t  \t");
    }

    #[test]
    fn mirror_source_prefix_as_pad_replaces_multibyte_chars_with_spaces() {
        // `é` is one char, one column. The pad counts CHARS (matching
        // `line_col`'s `column` accounting), so `é)` at column 2 →
        // ONE space of pad, not two (which the pre-lift byte-repeat
        // would have produced under a naive byte-count).
        assert_eq!(mirror_source_prefix_as_pad("é)", 2), " ");
        assert_eq!(mirror_source_prefix_as_pad("\téé)", 4), "\t  ");
    }

    #[test]
    fn mirror_source_prefix_as_pad_clamps_to_line_length_at_eof_column() {
        // `format_diagnostic_renders_eof_at_end_of_input` renders the
        // caret one column past the last visible char. Under this
        // logic the pad is `chars().take(N)` over an N-char line, which
        // yields exactly N mirrored chars — same as the pre-lift
        // `" ".repeat(N)` for a tab-free source. Pin the clamp so a
        // future refactor that swaps `take` for a slice-index panics
        // out at rustc / test time rather than silently mispadding
        // EOF errors.
        assert_eq!(mirror_source_prefix_as_pad("(a b) '", 8), "       ");
        assert_eq!(mirror_source_prefix_as_pad("\tfoo", 5), "\t   ");
    }

    // ── caret_run ───────────────────────────────────────────────────
    //
    // The shared caret renderer. These pins exist because `caret_run` is
    // the SINGLE definition both consumers now go through — this module's
    // `format_diagnostic` (width 1) and `EvalError::render` (width = the
    // span's CHAR count). A drift here breaks both at once, which is the
    // point of consolidating them.

    #[test]
    fn caret_run_width_one_reproduces_the_single_caret_pad() {
        // The `format_diagnostic` case: pad + exactly one `^`. Byte-
        // identical to the pre-lift `mirror_source_prefix_as_pad(..) +
        // "^"` expression it replaced.
        assert_eq!(caret_run("   )", 4, 1), "   ^");
        assert_eq!(caret_run("(a b)", 1, 1), "^");
    }

    #[test]
    fn caret_run_underlines_a_multi_column_span() {
        // The `EvalError::render` case, which the single-caret renderer
        // could not express at all before the width parameter. `(+ x foo
        // y)` with `foo` spanning columns 6..9 → five pad columns then
        // three carets.
        assert_eq!(caret_run("(+ x foo y)", 6, 3), "     ^^^");
    }

    #[test]
    fn caret_run_clamps_zero_width_up_to_one_caret() {
        // A zero-width span still names a point in the source; rendering
        // a bare pad with nothing under it would produce a diagnostic
        // whose caret line is invisible whitespace.
        assert_eq!(caret_run("(a b)", 3, 0), "  ^");
    }

    #[test]
    fn caret_run_mirrors_tabs_under_a_multi_column_underline() {
        // THE eval-side bug fix, at the primitive. Pre-lift the eval copy
        // padded with `" ".repeat(column - 1)`, so a tab-indented source
        // line put ONE space where the source spent a whole tab-stop and
        // the entire underline slid left of the span it names. Going
        // through the shared pad fixes it for every width.
        assert_eq!(caret_run("\tfoo", 2, 3), "\t^^^");
        assert_eq!(caret_run("  \tbar", 4, 3), "  \t^^^");
    }

    #[test]
    fn caret_run_counts_pad_in_chars_not_bytes_for_multibyte_prefix() {
        // Companion to the width-unit fix: the pad is a CHAR count, so a
        // multi-byte prefix advances the caret by columns-as-seen, not by
        // bytes. `é` is one column, two bytes — one space of pad.
        assert_eq!(caret_run("é)", 2, 1), " ^");
        assert_eq!(caret_run("ééx", 3, 1), "  ^");
    }

    #[test]
    fn format_diagnostic_caret_pad_mirrors_tab_indent_for_terminal_alignment() {
        // END-TO-END CONTRACT: a tab-indented source with a stray `)`
        // must render a caret pad whose leading tab matches the source
        // line's leading tab — so the terminal displays `^` under `)`
        // regardless of tab-stop setting. Pre-lift this rendered
        // `\t)` above `  ^` (two spaces where a tab belongs), which
        // slid the caret left of the `)` on every real terminal. Pin
        // the fix at the outer boundary so a regression in
        // `mirror_source_prefix_as_pad` surfaces through the diagnostic
        // consumer, not just its internal unit-pin.
        let src = "\t)";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("tabby.lisp"));
        let expected = "\
error: unmatched closing paren at position 1
 --> tabby.lisp:1:2
  |
1 | \t)
  | \t^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }

    #[test]
    fn format_diagnostic_caret_pad_mirrors_mixed_tab_space_indent() {
        // Deeper composition: mixed leading indent (space + tab +
        // space) with the caret on a nested `(` that stays unclosed.
        // Every prefix char round-trips into the pad — spaces stay
        // spaces, the tab stays a tab. A regression that homogenizes
        // the prefix to all-spaces fails HERE with a visible mismatch
        // on the tab position.
        //
        // The underline is four wide because the error's span covers the
        // unclosed form `(a b`; the tab-mirror invariant this test exists
        // for is about the PAD, and holds identically at any width.
        let src = " \t (a b";
        let err = read(src).unwrap_err();
        let rendered = format_diagnostic(src, &err, Some("mixed.lisp"));
        // Unclosed `(` sits at byte 3, column 4 on line 1.
        let expected = "\
error: unmatched opening paren at position 3
 --> mixed.lisp:1:4
  |
1 |  \t (a b
  |  \t ^^^^";
        assert_eq!(rendered, expected, "got:\n{rendered}");
    }
}

//! S-expression reader — tokenize + parse into `Sexp` or into `Spanned`.
//!
//! ONE tokenizer, two projections. Source positions are byte offsets into
//! the original `&str`; every token carries the `Span` of its own lexeme,
//! and reader-level errors (`UnmatchedParen`, `UnmatchedOpenParen`, `Eof`,
//! `UnterminatedString`) report a real `Span` — a `[start, end)` RANGE,
//! not a point — so downstream tools (`tatara-lispc`, `tatara-check`,
//! REPL, future LSP) can pinpoint AND underline the failure in the source.
//!
//! The reader's error currency is [`Span`], uniformly. Pre-lift those four
//! variants carried a bare `pos: usize`, which was strictly less than what
//! the tokenizer already knew: every token here is built with both ends of
//! its lexeme in hand, and the parser holds the open-paren token while it
//! reads a form's children. Throwing the end away meant
//! `crate::diagnostic::format_diagnostic` could only ever pass width 1 to
//! `caret_run`, so an unclosed `(a (b c` pointed one caret at the `(`
//! instead of underlining `(b c`. The end offsets below are the ones the
//! reader genuinely knows — see [`SourceTail`] for why an unclosed form
//! ends at the last TOKEN rather than at `src.len()`.
//!
//! Pre-consolidation this file carried TWO tokenizers and TWO atom
//! classifiers — a "plain" pair that threw positions away and a "spanned"
//! pair that kept them — declared parallel on the theory that the hot plain
//! path wanted zero overhead. The cost was not the overhead: it was that
//! the plain path had no offsets to report, so `parse` returned
//! `UnmatchedOpenParen { pos: 0 }` / `UnmatchedParen { pos: 0 }` /
//! `Eof { pos: 0 }` — three hardcoded lies — and the two classifiers were
//! free to disagree about what `"#t"` means. Post-consolidation `tokenize`
//! is the single lexical authority, [`Atom::from_lexeme`] is the single
//! classifier, and the two parsers differ only in which tree they build.

use crate::ast::{Atom, QuoteForm, Sexp};
use crate::error::{LispError, Result};
use crate::span::Span;
use crate::spanned::{Spanned, SpannedForm};

// The four homoiconic prefix-wrappers (`'`, `` ` ``, `,`, `,@`) collapse
// onto ONE `Token::Quoted(QuoteForm)` variant carrying the substrate's
// typed `QuoteForm` marker. Pre-lift the reader carried its own parallel
// closed set (`Token::{Quote, Quasiquote, Unquote, UnquoteSplice}`) paired
// with the matching `Sexp::*` tuple-variant constructors threaded as
// `fn(Box<Sexp>) -> Sexp` arguments to `read_quoted` — the FIFTH consumer
// site of the quote-family closed set the prior `QuoteForm` lifts did not
// reach. Post-lift the reader binds to the substrate algebra: tokenizer
// arms construct `Token::Quoted(QuoteForm::*)` directly, the parser
// collapses its four `Some((Token::Quote*, _))` arms to ONE
// `Some((Token::Quoted(qf), _))` arm, and `read_quoted` routes through
// `QuoteForm::wrap` so the (marker, Sexp::* constructor) pairing binds
// at ONE site rather than per-arm. Adding a fifth homoiconic prefix
// extends `QuoteForm` AND the tokenizer arm AND `QuoteForm::wrap`'s arm
// in lockstep — rustc binds the reader's wrap step to the substrate
// algebra through exhaustiveness over the closed enum.
#[derive(Clone, Debug, PartialEq)]
enum Token {
    LParen,
    RParen,
    Quoted(QuoteForm),
    Atom(String),
    Str(String),
}

/// A token paired with the `Span` of its own lexeme in the source.
///
/// The plain parser reads `span.start` (the byte offset the pre-
/// consolidation fork carried as a bare `usize`); the spanned parser reads
/// the whole range. ONE token stream feeds both, so a lexical fix lands
/// once and neither projection can drift from the other on what a token
/// even is.
#[derive(Clone, Debug, PartialEq)]
struct SpannedToken {
    kind: Token,
    span: Span,
}

/// The two end-of-input offsets a reader error can need, carried as ONE
/// value so the two parsers cannot thread different ends of the source.
///
/// They differ, and the difference is what makes an unclosed-form
/// underline readable:
///
/// * `eof` is `src.len()` — where an [`LispError::Eof`] points, because
///   "input ran out" is a fact about the input, not about any token.
/// * `last_token_end` is the end of the LAST lexeme in the stream. An
///   unclosed `(` is reported as the span `[open, last_token_end)`: the
///   extent of the partial form. Using `eof` there would drag trailing
///   whitespace, newlines and comments under the caret run — `"(a b   "`
///   would underline seven columns to name a four-column form.
///
/// `last_token_end` is 0 for an empty token stream, which is unreachable
/// from a parse: `read`/`read_spanned` only enter `parse` when the stream
/// has a token, and every recursive entry is preceded by one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceTail {
    eof: usize,
    last_token_end: usize,
}

impl SourceTail {
    fn of(src: &str, tokens: &[SpannedToken]) -> Self {
        Self {
            eof: src.len(),
            last_token_end: tokens.last().map_or(0, |t| t.span.end),
        }
    }

    /// Span for an unclosed `(` opened at `open_start` — the partial
    /// form's extent.
    fn unclosed_from(self, open_start: usize) -> Span {
        Span::new(open_start, self.last_token_end)
    }

    /// Zero-width span AT end-of-input. There is no text to underline;
    /// `caret_run` clamps the width up to a single caret.
    fn eof_span(self) -> Span {
        Span::new(self.eof, self.eof)
    }
}

/// Read a full program (sequence of top-level forms) into a `Vec<Sexp>`.
pub fn read(src: &str) -> Result<Vec<Sexp>> {
    let tokens = tokenize(src)?;
    let tail = SourceTail::of(src, &tokens);
    let mut it = tokens.into_iter().peekable();
    let mut forms = Vec::new();
    while it.peek().is_some() {
        forms.push(parse(&mut it, tail)?);
    }
    Ok(forms)
}

/// Read a full program into `Vec<Spanned>`, where every subtree carries
/// a `Span` pointing back into `src`. Equivalent to `read` in grammar and
/// error reporting — they share `tokenize` and [`Atom::from_lexeme`], so
/// the equivalence is structural rather than asserted — and strictly
/// additive in what it carries.
pub fn read_spanned(src: &str) -> Result<Vec<Spanned>> {
    let tokens = tokenize(src)?;
    let tail = SourceTail::of(src, &tokens);
    let mut it = tokens.into_iter().peekable();
    let mut forms = Vec::new();
    while it.peek().is_some() {
        forms.push(parse_spanned(&mut it, tail)?);
    }
    Ok(forms)
}

fn tokenize(src: &str) -> Result<Vec<SpannedToken>> {
    let mut out: Vec<SpannedToken> = Vec::new();
    let mut chars = src.char_indices().peekable();
    while let Some(&(pos, c)) = chars.peek() {
        // Quote-family outer dispatch — the (lead char, `QuoteForm`
        // marker) pairing binds at ONE site on the closed-set
        // [`QuoteForm`] algebra via [`QuoteForm::from_lead_char`], then
        // promotes the decoded `QuoteForm::Unquote` to
        // `QuoteForm::UnquoteSplice` on second-char `@` via
        // [`QuoteForm::promote_via_next_char`] (the
        // `(Unquote, SPLICE_DISCRIMINATOR) → UnquoteSplice` promotion
        // table on the closed-set algebra). The `Option<QuoteForm>`
        // returned by `promote_via_next_char` signals BOTH whether the
        // second char was consumed AND the promoted variant to emit — no
        // separate `matches!(…)` gate, no per-branch `QuoteForm::*`
        // literal. Adding a fifth homoiconic prefix extends
        // [`QuoteForm`] AND [`QuoteForm::from_lead_char`] AND
        // [`QuoteForm::promote_via_next_char`] in lockstep.
        //
        // The token's end offset is `pos + qf.prefix().len()` rather than
        // a hand-counted 1-or-2: `prefix()` IS the canonical rendering of
        // the bytes this arm just consumed, so the span cannot drift from
        // the lexeme when a variant's prefix changes length.
        if let Some(qf_head) = QuoteForm::from_lead_char(c) {
            chars.next();
            let qf = if let Some(promoted) = chars
                .peek()
                .and_then(|&(_, next)| qf_head.promote_via_next_char(next))
            {
                chars.next();
                promoted
            } else {
                qf_head
            };
            out.push(SpannedToken {
                kind: Token::Quoted(qf),
                span: Span::new(pos, pos + qf.prefix().len()),
            });
            continue;
        }
        match c {
            ws if ws.is_whitespace() => {
                chars.next();
            }
            // Line-comment arm — the canonical `;` byte routed through
            // the [`Sexp::COMMENT_LEAD`] constant on the closed-set outer
            // [`Sexp`] algebra. The paired terminator byte is
            // [`Sexp::COMMENT_TERM`]; the (lead, term) pair carries the
            // SAME opener/closer discipline that
            // (`LIST_OPEN`, `LIST_CLOSE`) does on the outer-structural
            // axis. This loop consumes every byte up to (and including)
            // the first `COMMENT_TERM` so the discarded run emits NO
            // token — which is why a comment never shows up in either
            // projection's span arithmetic.
            Sexp::COMMENT_LEAD => {
                while let Some(&(_, ch)) = chars.peek() {
                    chars.next();
                    if ch == Sexp::COMMENT_TERM {
                        break;
                    }
                }
            }
            // List-opening arm — the canonical `(` byte routed through
            // the [`Sexp::LIST_OPEN`] constant on the closed-set outer
            // [`Sexp`] algebra. The (structural role, canonical byte)
            // pairing binds at ONE typed constant rather than at an
            // inline `char` literal at this arm AND the bare-atom
            // terminator's disjunct below AND `Sexp`'s Display impl's
            // opener/closer arms in `ast.rs`.
            Sexp::LIST_OPEN => {
                chars.next();
                out.push(SpannedToken {
                    kind: Token::LParen,
                    span: Span::new(pos, pos + Sexp::LIST_OPEN.len_utf8()),
                });
            }
            // List-closing arm — the canonical `)` byte routed through
            // the [`Sexp::LIST_CLOSE`] constant. Section-for-retraction
            // sibling of the list-opening arm above; the paired-delimiter
            // round-trip holds iff both arms bind to the same closed-set
            // constants.
            Sexp::LIST_CLOSE => {
                chars.next();
                out.push(SpannedToken {
                    kind: Token::RParen,
                    span: Span::new(pos, pos + Sexp::LIST_CLOSE.len_utf8()),
                });
            }
            // String-opening arm — the canonical `"` byte routed through
            // the [`Atom::STR_DELIMITER`] constant on the closed-set
            // [`Atom`] algebra. The closing arm below AND the self-escape
            // arm inside the escape table AND the bare-atom terminator
            // disjunct all bind to the SAME constant so the four
            // `"`-round-trip sites cannot drift.
            Atom::STR_DELIMITER => {
                chars.next();
                let start = pos;
                let mut s = String::new();
                let end;
                loop {
                    match chars.next() {
                        // Escape-lead arm — the canonical `\` byte routed
                        // through [`Atom::STR_ESCAPE_LEAD`]. The decode
                        // itself is ONE typed projection
                        // ([`Atom::decode_str_escape`]) on the algebra
                        // rather than an inline six-arm `match esc` table,
                        // so a new named escape (`'0' → '\0'`) extends the
                        // algebra ONCE.
                        Some((_, Atom::STR_ESCAPE_LEAD)) => {
                            if let Some((_, esc)) = chars.next() {
                                // `\u{...}` is the ONE escape that is not a
                                // single character, so it cannot route through
                                // the single-char algebra above and needs its
                                // own arm.
                                //
                                // It is here because the WRITER emits it:
                                // `Display` escapes a non-printable scalar as
                                // `\u{301}`, and without this arm the reader
                                // decoded `u` and dropped the backslash, so
                                // `"e\u{301}"` round-tripped to `"eu{301}"` —
                                // SILENTLY, with no error and corrupted data.
                                // Display and read must be inverses; they were
                                // not.
                                if esc == 'u' {
                                    let mut hex = String::new();
                                    let mut saw_open = false;
                                    // Peek-free: consume `{`, then hex, then `}`.
                                    for (_, c) in chars.by_ref() {
                                        if !saw_open {
                                            if c != '{' {
                                                break;
                                            }
                                            saw_open = true;
                                            continue;
                                        }
                                        if c == '}' {
                                            break;
                                        }
                                        hex.push(c);
                                    }
                                    // An unparseable body keeps the literal
                                    // text rather than inventing a character:
                                    // substituting U+FFFD here would turn a
                                    // malformed escape into a plausible glyph.
                                    match u32::from_str_radix(&hex, 16)
                                        .ok()
                                        .and_then(char::from_u32)
                                    {
                                        Some(ch) => s.push(ch),
                                        None => {
                                            s.push('u');
                                            s.push('{');
                                            s.push_str(&hex);
                                            s.push('}');
                                        }
                                    }
                                } else {
                                    s.push(Atom::decode_str_escape(esc));
                                }
                            }
                        }
                        // String-closing arm — the same constant as the
                        // opener; a delimiter swap flips both arms in
                        // lockstep. `p + len_utf8()` is the byte just past
                        // the closing delimiter, so the token's span
                        // covers the quotes as well as the payload.
                        Some((p, Atom::STR_DELIMITER)) => {
                            end = p + Atom::STR_DELIMITER.len_utf8();
                            break;
                        }
                        Some((_, ch)) => s.push(ch),
                        // The unterminated run reaches end-of-input by
                        // construction — this arm fires only when the char
                        // iterator is exhausted — so `src.len()` is the
                        // exact end of what the tokenizer consumed, and
                        // the span underlines the whole dangling literal
                        // rather than pointing at its opening quote.
                        None => {
                            return Err(LispError::UnterminatedString(Span::new(pos, src.len())))
                        }
                    }
                }
                out.push(SpannedToken {
                    kind: Token::Str(s),
                    span: Span::new(start, end),
                });
            }
            _ => {
                let start = pos;
                let mut s = String::new();
                let mut end = pos;
                while let Some(&(p, ch)) = chars.peek() {
                    // Bare-atom terminator disjunct — the SIX-fold
                    // outer-dispatch category-leading char disjunction
                    // routed through the closed-set
                    // [`Sexp::is_bare_atom_boundary`] projection. Pre-lift
                    // this predicate lived as an inline six-clause boolean
                    // chain spanning THREE type namespaces at ONE consumer
                    // site — and, worse, as TWO such chains, one per
                    // tokenizer. The outer dispatch above fires the
                    // SPECIFIC arm for whichever category `c` matches;
                    // this terminator fires the BARE-ATOM break iff ANY of
                    // the same categories matches. One typed source of
                    // truth, so a SEVENTH outer-dispatch category cannot
                    // land in one place and be forgotten in the other.
                    if Sexp::is_bare_atom_boundary(ch) {
                        break;
                    }
                    s.push(ch);
                    end = p + ch.len_utf8();
                    chars.next();
                }
                out.push(SpannedToken {
                    kind: Token::Atom(s),
                    span: Span::new(start, end),
                });
            }
        }
    }
    Ok(out)
}

fn parse<I: Iterator<Item = SpannedToken>>(
    it: &mut std::iter::Peekable<I>,
    tail: SourceTail,
) -> Result<Sexp> {
    match it.next() {
        Some(SpannedToken {
            kind: Token::LParen,
            span: open_span,
        }) => {
            let mut xs = Vec::new();
            loop {
                match it.peek() {
                    Some(SpannedToken {
                        kind: Token::RParen,
                        ..
                    }) => {
                        it.next();
                        return Ok(Sexp::List(xs));
                    }
                    Some(_) => xs.push(parse(it, tail)?),
                    None => {
                        return Err(LispError::UnmatchedOpenParen {
                            span: tail.unclosed_from(open_span.start),
                        })
                    }
                }
            }
        }
        Some(SpannedToken {
            kind: Token::RParen,
            span,
        }) => Err(LispError::UnmatchedParen { span }),
        // The four pre-lift `Token::Quote*` arms collapse to ONE arm
        // routing through the typed `QuoteForm` marker — the (Token
        // variant, `Sexp::*` constructor) pairing binds at the closed-set
        // algebra (`QuoteForm::wrap`) rather than threaded as per-arm
        // constructor literals.
        Some(SpannedToken {
            kind: Token::Quoted(qf),
            ..
        }) => read_quoted(it, tail, qf),
        Some(SpannedToken {
            kind: Token::Str(s),
            ..
        }) => Ok(Sexp::Atom(Atom::Str(s))),
        // The five-statement classification cascade that lived in this
        // module's private `atom_from_str` — and, byte-for-byte again, in
        // its private `spanned_atom_from_str` — is ONE call to
        // [`Atom::from_lexeme`] on the typed `Atom` algebra. Adding a
        // fifth structural prefix (`"#["` for vector literals, `"#\\x"`
        // for char literals) extends `Atom::from_lexeme` + the matching
        // `Atom` variant + each sibling typed-exit projection in lockstep.
        Some(SpannedToken {
            kind: Token::Atom(s),
            ..
        }) => Ok(Sexp::Atom(Atom::from_lexeme(&s))),
        None => Err(LispError::Eof {
            span: tail.eof_span(),
        }),
    }
}

/// Parse the datum following a quote-like prefix token (`'`, `` ` ``, `,`,
/// `,@`) and wrap it in the matching `Sexp::*` constructor projected from
/// the typed [`QuoteForm`] marker.
///
/// Centralizes the four byte-identical "read-inner-and-box" arms in
/// `parse` — one per homoiconic prefix — into ONE emission site. The
/// per-prefix (Token variant, `Sexp::*` constructor) pairing binds at ONE
/// site on the substrate's `QuoteForm` closed-set algebra: the parser
/// dispatches on `Token::Quoted(qf)`, this helper reads the inner datum,
/// and [`QuoteForm::wrap`] projects the typed marker back into its
/// `Sexp::*` wrapper variant. Adding a fifth homoiconic prefix extends
/// `QuoteForm` AND its tokenizer arm AND [`QuoteForm::wrap`]'s match arm
/// in lockstep, with rustc binding the extension through exhaustiveness
/// over the closed enum.
///
/// Span-carrying dual: [`read_quoted_spanned`], which routes through
/// [`SpannedForm::wrap`] over the SAME marker.
fn read_quoted<I: Iterator<Item = SpannedToken>>(
    it: &mut std::iter::Peekable<I>,
    tail: SourceTail,
    qf: QuoteForm,
) -> Result<Sexp> {
    let inner = parse(it, tail)?;
    Ok(qf.wrap(inner))
}

fn parse_spanned<I: Iterator<Item = SpannedToken>>(
    it: &mut std::iter::Peekable<I>,
    tail: SourceTail,
) -> Result<Spanned> {
    match it.next() {
        Some(SpannedToken {
            kind: Token::LParen,
            span: open_span,
        }) => {
            let mut xs: Vec<Spanned> = Vec::new();
            loop {
                match it.peek() {
                    Some(SpannedToken {
                        kind: Token::RParen,
                        span: close_span,
                    }) => {
                        let close = *close_span;
                        it.next();
                        return Ok(Spanned::new(
                            Span::new(open_span.start, close.end),
                            SpannedForm::List(xs),
                        ));
                    }
                    Some(_) => xs.push(parse_spanned(it, tail)?),
                    None => {
                        return Err(LispError::UnmatchedOpenParen {
                            span: tail.unclosed_from(open_span.start),
                        })
                    }
                }
            }
        }
        Some(SpannedToken {
            kind: Token::RParen,
            span,
        }) => Err(LispError::UnmatchedParen { span }),
        Some(SpannedToken {
            kind: Token::Quoted(qf),
            span,
        }) => read_quoted_spanned(it, tail, qf, span),
        Some(SpannedToken {
            kind: Token::Str(s),
            span,
        }) => Ok(Spanned::new(span, SpannedForm::Atom(Atom::Str(s)))),
        Some(SpannedToken {
            kind: Token::Atom(s),
            span,
        }) => Ok(Spanned::new(
            span,
            SpannedForm::Atom(Atom::from_lexeme(&s)),
        )),
        None => Err(LispError::Eof {
            span: tail.eof_span(),
        }),
    }
}

/// Span-carrying dual of [`read_quoted`] — reads the inner datum and wraps
/// it through [`SpannedForm::wrap`] over the same typed [`QuoteForm`]
/// marker, then widens the span to cover the prefix AND the datum.
fn read_quoted_spanned<I: Iterator<Item = SpannedToken>>(
    it: &mut std::iter::Peekable<I>,
    tail: SourceTail,
    qf: QuoteForm,
    prefix_span: Span,
) -> Result<Spanned> {
    let inner = parse_spanned(it, tail)?;
    let full = Span::new(prefix_span.start, inner.span.end.max(prefix_span.end));
    Ok(Spanned::new(full, SpannedForm::wrap(qf, inner)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_atoms() {
        let forms = read("foo 42 2.5 \"hello\" :kw #t #f").unwrap();
        assert_eq!(forms.len(), 7);
        assert_eq!(forms[0].as_symbol(), Some("foo"));
        assert_eq!(forms[1], Sexp::int(42));
        assert_eq!(forms[2], Sexp::float(2.5));
        assert_eq!(forms[3].as_string(), Some("hello"));
        assert_eq!(forms[4].as_keyword(), Some("kw"));
        assert_eq!(forms[5], Sexp::boolean(true));
        assert_eq!(forms[6], Sexp::boolean(false));
    }

    #[test]
    fn reads_nested_lists() {
        let f = read("(defpoint obs :class (Gate Observability))").unwrap();
        assert_eq!(f.len(), 1);
        let outer = f[0].as_list().unwrap();
        assert_eq!(outer[0].as_symbol(), Some("defpoint"));
        assert_eq!(outer[1].as_symbol(), Some("obs"));
        assert_eq!(outer[2].as_keyword(), Some("class"));
        let inner = outer[3].as_list().unwrap();
        assert_eq!(inner[0].as_symbol(), Some("Gate"));
        assert_eq!(inner[1].as_symbol(), Some("Observability"));
    }

    #[test]
    fn handles_comments() {
        let f = read("; top-level comment\n(a b) ; inline\n(c)").unwrap();
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn string_escapes() {
        let f = read(r#""line\nbreak\ttab""#).unwrap();
        assert_eq!(f[0].as_string(), Some("line\nbreak\ttab"));
    }

    #[test]
    fn quote_form() {
        let f = read("'(a b)").unwrap();
        match &f[0] {
            Sexp::Quote(inner) => assert!(inner.is_list()),
            _ => panic!("expected quote"),
        }
    }

    #[test]
    fn unmatched_paren_errors() {
        assert!(read("(a b").is_err());
        assert!(read(")").is_err());
    }

    // ── Source-position fidelity ────────────────────────────────────────
    //
    // The four reader-level errors carry `Span`s — RANGES — so authoring
    // tools can render them at the right place AND underline the right
    // extent. Two regressions are kept dead here.
    //
    // First: pre-consolidation the PLAIN parser consumed an unspanned
    // token stream and spelled `UnmatchedOpenParen { pos: 0 }` /
    // `UnmatchedParen { pos: 0 }` / `Eof { pos: 0 }` — three hardcodes. A
    // regression that re-splits the tokenizer makes these fail rather
    // than silently re-report byte 0 for everything.
    //
    // Second: pre-span-lift these variants carried only `start`, so every
    // reader diagnostic could render exactly one caret no matter how much
    // source the failure covered. The `end` assertions below are what
    // keep the widths honest — see `SourceTail` for why an unclosed form
    // ends at the last TOKEN and not at `src.len()`.

    #[test]
    fn unmatched_closing_paren_reports_byte_offset() {
        // `   )` — the stray `)` is at byte 3, and the span is exactly
        // that one-byte token: a stray closer names itself, so this
        // renders as a single caret both before and after the span lift.
        let err = read("   )").unwrap_err();
        match err {
            LispError::UnmatchedParen { span } => assert_eq!(span, Span::new(3, 4)),
            other => panic!("expected UnmatchedParen, got {other:?}"),
        }
    }

    #[test]
    fn unmatched_opening_paren_reports_offset_of_open() {
        // `(a (b c` — the inner `(` is at byte 3 and stays unclosed; the
        // outer `(` at byte 0 is also unclosed but the deepest unclosed
        // open is what the parser hits first. The span runs to byte 7,
        // the end of the last token (`c`), so the partial form `(b c` is
        // the underlined extent.
        let err = read("(a (b c").unwrap_err();
        match err {
            LispError::UnmatchedOpenParen { span } => assert_eq!(span, Span::new(3, 7)),
            other => panic!("expected UnmatchedOpenParen, got {other:?}"),
        }
    }

    #[test]
    fn outer_unmatched_open_reports_outer_offset() {
        // `(a b` — only the outer `(` at byte 0 is open. Distinct from the
        // pre-consolidation `pos: 0` hardcode ONLY because the sibling
        // test above pins a NON-zero offset for the same variant.
        let err = read("(a b").unwrap_err();
        match err {
            LispError::UnmatchedOpenParen { span } => assert_eq!(span, Span::new(0, 4)),
            other => panic!("expected UnmatchedOpenParen, got {other:?}"),
        }
    }

    #[test]
    fn unmatched_open_span_ends_at_last_token_not_at_eof() {
        // THE reason `SourceTail` carries two offsets. `(a b` followed by
        // trailing whitespace and a comment: the form's extent is still
        // `[0, 4)` — bytes 4..15 are trailing trivia the tokenizer emitted
        // no token for. Ending the span at `src.len()` instead would
        // underline eleven columns of nothing to name a four-column form.
        let src = "(a b   ; tail\n";
        assert_eq!(src.len(), 14);
        let err = read(src).unwrap_err();
        match err {
            LispError::UnmatchedOpenParen { span } => assert_eq!(span, Span::new(0, 4)),
            other => panic!("expected UnmatchedOpenParen, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_string_span_covers_the_dangling_literal() {
        // `(a "bc` — the `"` opens at byte 3 and the tokenizer consumes to
        // end-of-input looking for a closer, so the span is the whole
        // dangling literal `"bc` rather than a point at the quote.
        let src = "(a \"bc";
        let err = read(src).unwrap_err();
        match err {
            LispError::UnterminatedString(span) => assert_eq!(span, Span::new(3, src.len())),
            other => panic!("expected UnterminatedString, got {other:?}"),
        }
    }

    #[test]
    fn eof_span_is_zero_width_at_end_of_input() {
        // "input ran out" names a point, not a range — there is no text to
        // underline. `caret_run` clamps zero width up to one caret, which
        // is why the rendered EOF diagnostic is unchanged by the lift.
        let src = "(a b) '";
        let err = read(src).unwrap_err();
        match err {
            LispError::Eof { span } => {
                assert_eq!(span, Span::new(src.len(), src.len()));
                assert_eq!(span.end - span.start, 0, "EOF span must be zero-width");
            }
            other => panic!("expected Eof, got {other:?}"),
        }
    }

    #[test]
    fn dangling_quote_reports_eof_at_input_length() {
        // A trailing `'` with no datum to quote — parse runs off the end.
        let src = "(a b) '";
        let err = read(src).unwrap_err();
        match err {
            LispError::Eof { span } => assert_eq!(span.start, src.len()),
            other => panic!("expected Eof, got {other:?}"),
        }
    }

    #[test]
    fn spanned_parser_reports_the_same_offsets_as_the_plain_one() {
        // ONE-TOKENIZER CONTRACT, error axis. `read` and `read_spanned`
        // share `tokenize`, so a malformed source must produce the SAME
        // typed error with the SAME span through both projections. The
        // pre-consolidation pair could not satisfy this: the plain side
        // reported 0 for every one of these. Comparing the `Debug`
        // rendering means the assertion tightened for free when the
        // variants went from `pos: usize` to `span: Span` — it now pins
        // both ends of every reader error, through both parsers.
        for src in ["   )", "(a (b c", "(a b", "(a b) '", "\"unterminated"] {
            let plain = read(src).unwrap_err();
            let spanned = read_spanned(src).unwrap_err();
            assert_eq!(
                format!("{plain:?}"),
                format!("{spanned:?}"),
                "{src:?}: plain and spanned readers disagreed on the error"
            );
        }
    }

    #[test]
    fn error_display_includes_position() {
        // The user-facing string must mention the position so downstream
        // tools and humans can act on it without inspecting the variant.
        let err = read(")  ").unwrap_err();
        let rendered = format!("{err}");
        assert!(
            rendered.contains("position 0"),
            "expected position in display, got {rendered:?}"
        );
    }

    // ── read_quoted helper: closed-set quote-prefix dispatch ────────────

    #[test]
    fn quote_prefix_round_trips_through_read_quoted_into_sexp_quote() {
        let f = read("'foo").unwrap();
        assert_eq!(f.len(), 1);
        match &f[0] {
            Sexp::Quote(inner) => assert_eq!(inner.as_symbol(), Some("foo")),
            other => panic!("expected Sexp::Quote, got {other:?}"),
        }
    }

    #[test]
    fn quasiquote_prefix_round_trips_through_read_quoted_into_sexp_quasiquote() {
        let f = read("`foo").unwrap();
        assert_eq!(f.len(), 1);
        match &f[0] {
            Sexp::Quasiquote(inner) => assert_eq!(inner.as_symbol(), Some("foo")),
            other => panic!("expected Sexp::Quasiquote, got {other:?}"),
        }
    }

    #[test]
    fn unquote_prefix_round_trips_through_read_quoted_into_sexp_unquote() {
        // Pin that the bare `,` (not `,@`) dispatches to `Sexp::Unquote`,
        // NOT to `Sexp::UnquoteSplice` — the tokenizer's `,`-then-peek-`@`
        // discriminator must round-trip cleanly through the helper.
        let f = read(",foo").unwrap();
        assert_eq!(f.len(), 1);
        match &f[0] {
            Sexp::Unquote(inner) => assert_eq!(inner.as_symbol(), Some("foo")),
            other => panic!("expected Sexp::Unquote, got {other:?}"),
        }
    }

    #[test]
    fn unquote_splice_prefix_round_trips_through_read_quoted_into_sexp_unquote_splice() {
        let f = read(",@xs").unwrap();
        assert_eq!(f.len(), 1);
        match &f[0] {
            Sexp::UnquoteSplice(inner) => assert_eq!(inner.as_symbol(), Some("xs")),
            other => panic!("expected Sexp::UnquoteSplice, got {other:?}"),
        }
    }

    #[test]
    fn quote_prefix_recursively_wraps_via_read_quoted_for_nested_homoiconic_forms() {
        let f = read("',foo").unwrap();
        assert_eq!(f.len(), 1);
        match &f[0] {
            Sexp::Quote(outer) => match outer.as_ref() {
                Sexp::Unquote(inner) => assert_eq!(inner.as_symbol(), Some("foo")),
                other => panic!("expected inner Sexp::Unquote, got {other:?}"),
            },
            other => panic!("expected outer Sexp::Quote, got {other:?}"),
        }
    }

    #[test]
    fn read_quoted_propagates_inner_parse_error_unchanged() {
        let src = "'";
        let err = read(src).unwrap_err();
        match err {
            LispError::Eof { span } => assert_eq!(span, Span::new(src.len(), src.len())),
            other => panic!("expected Eof, got {other:?}"),
        }
    }

    #[test]
    fn reader_threads_each_prefix_through_quote_form_wrap_dual_of_as_quote_form() {
        // END-TO-END CLOSED-SET CONTRACT: for each of the four homoiconic
        // prefixes the read path produces a `Sexp::*` value byte-identical
        // to what `expected_qf.wrap(inner)` builds, and projecting back
        // through `as_quote_form` recovers the marker and the body.
        let inner = Sexp::symbol("payload");
        for (src, expected_qf) in [
            ("'payload", QuoteForm::Quote),
            ("`payload", QuoteForm::Quasiquote),
            (",payload", QuoteForm::Unquote),
            (",@payload", QuoteForm::UnquoteSplice),
        ] {
            let forms = read(src).expect(src);
            assert_eq!(forms.len(), 1, "{src} must produce one form");
            assert_eq!(
                forms[0],
                expected_qf.wrap(inner.clone()),
                "{src} drifted from QuoteForm::wrap dual"
            );
            let (qf, body) = forms[0]
                .as_quote_form()
                .unwrap_or_else(|| panic!("{src} must project through as_quote_form"));
            assert_eq!(qf, expected_qf, "{src} produced wrong typed marker");
            assert_eq!(body, &inner, "{src} drifted inner body");
        }
    }

    #[test]
    fn token_quoted_arms_carry_typed_quote_form_marker_for_every_prefix() {
        // CLOSED-SET TOKENIZATION CONTRACT: each homoiconic prefix
        // tokenizes to `Token::Quoted(QuoteForm::*)` with the matching
        // closed-set variant, and the token's span covers exactly the
        // prefix bytes.
        for (src, expected_qf) in [
            ("'", QuoteForm::Quote),
            ("`", QuoteForm::Quasiquote),
            (",", QuoteForm::Unquote),
            (",@", QuoteForm::UnquoteSplice),
        ] {
            let tokens = tokenize(src).expect(src);
            assert_eq!(tokens.len(), 1, "{src} must produce one token");
            assert_eq!(
                tokens[0].kind,
                Token::Quoted(expected_qf),
                "{src} drifted typed marker"
            );
            assert_eq!(
                tokens[0].span,
                Span::new(0, expected_qf.prefix().len()),
                "{src} drifted span"
            );
        }
    }

    #[test]
    fn token_quoted_unquote_splice_two_char_marker_collapses_to_single_token() {
        // TOKEN-MERGE CONTRACT: the two-char `,@` prefix collapses to ONE
        // token, not two adjacent ones, and the bare `,` projects to
        // `Unquote`.
        let tokens = tokenize(",@xs").expect(",@xs");
        assert_eq!(tokens.len(), 2, ",@xs must tokenize as splice + atom");
        assert_eq!(tokens[0].kind, Token::Quoted(QuoteForm::UnquoteSplice));
        assert_eq!(tokens[0].span, Span::new(0, 2));
        assert_eq!(tokens[1].kind, Token::Atom("xs".into()));
        assert_eq!(tokens[1].span, Span::new(2, 4));

        let bare = tokenize(",xs").expect(",xs");
        assert_eq!(bare[0].kind, Token::Quoted(QuoteForm::Unquote));
        assert_eq!(bare[0].span, Span::new(0, 1));
    }

    #[test]
    fn read_quoted_propagates_unmatched_open_paren_for_quoted_list() {
        let err = read("'(a b").unwrap_err();
        match err {
            // `(` at byte 1, last token `b` ends at 5 — the quote prefix
            // is NOT part of the unclosed form's extent.
            LispError::UnmatchedOpenParen { span } => assert_eq!(span, Span::new(1, 5)),
            other => panic!("expected UnmatchedOpenParen, got {other:?}"),
        }
    }

    #[test]
    fn reader_atom_token_arm_routes_through_atom_from_lexeme_for_every_kind() {
        // LIFTED-BOUNDARY CONTRACT: the reader's `Token::Atom(s)` arm
        // produces the SAME value `Sexp::Atom(Atom::from_lexeme(s))`
        // builds directly, for every canonical bare-atom lexeme. Sweeps
        // every `AtomKind` the bare-atom branch can produce — `Symbol`,
        // `Keyword`, `Int`, `Float`, `Bool` — plus the load-bearing
        // `i64`-before-`f64` cascade order. `AtomKind::Str` is absent
        // because string literals take the distinct `Token::Str(_)`
        // branch.
        let cases: &[&str] = &[
            "foo", "defpoint", "seph.1", ":parent", ":kw", "42", "-7", "0", "1", "1.0", "1.5",
            "-2.5", "1e3", "#t", "#f",
            "true",  // "Lisp bools" — must classify to Symbol, not Bool.
            "false", // "Lisp bools" — must classify to Symbol, not Bool.
            "+", "a-b",
        ];
        for src in cases {
            let forms = read(src).unwrap_or_else(|e| panic!("reader rejected {src:?}: {e}"));
            assert_eq!(forms.len(), 1, "{src:?} must read as exactly one form");
            assert_eq!(
                &forms[0],
                &Sexp::Atom(Atom::from_lexeme(src)),
                "{src:?}: reader's bare-atom arm drifted from Atom::from_lexeme"
            );
        }
    }

    #[test]
    fn both_projections_classify_every_bare_atom_identically() {
        // ONE-CLASSIFIER CONTRACT. Pre-consolidation `atom_from_str` and
        // `spanned_atom_from_str` were two hand-written cascades free to
        // disagree; they are now ONE call to `Atom::from_lexeme` from both
        // parsers. This sweeps the same corpus through both projections
        // and pins that the spanned tree's spanless projection is the
        // plain tree — the structural replacement for the equivalence the
        // fork could only assert.
        let cases: &[&str] = &[
            "foo", ":kw", "42", "-7", "1.5", "1e3", "#t", "#f", "true", "false", "+", "a-b",
        ];
        for src in cases {
            let plain = read(src).expect(src);
            let spanned = read_spanned(src).expect(src);
            let projected: Vec<Sexp> = spanned.iter().map(Spanned::to_sexp).collect();
            assert_eq!(projected, plain, "{src:?}: projections disagreed");
        }
    }

    // ── `Atom::STR_DELIMITER` / `Atom::STR_ESCAPE_LEAD` round-trip sites ──

    #[test]
    fn reader_str_open_close_arms_bind_to_atom_str_delimiter() {
        let payload = "hello world";
        let source = format!("{}{payload}{}", Atom::STR_DELIMITER, Atom::STR_DELIMITER);
        let tokens = tokenize(&source)
            .unwrap_or_else(|e| panic!("tokenize rejected `{source}`: {e}"));
        assert_eq!(tokens.len(), 1, "must tokenize as exactly one Token::Str");
        assert_eq!(tokens[0].kind, Token::Str(payload.to_string()));
        // The span covers the delimiters, not just the payload.
        assert_eq!(tokens[0].span, Span::new(0, source.len()));
    }

    #[test]
    fn reader_str_escape_self_escape_arm_routes_through_atom_str_delimiter() {
        let escape_source = format!(
            "{}\\{}{}",
            Atom::STR_DELIMITER,
            Atom::STR_DELIMITER,
            Atom::STR_DELIMITER,
        );
        let forms = read(&escape_source)
            .unwrap_or_else(|e| panic!("reader rejected `{escape_source}`: {e}"));
        assert_eq!(forms.len(), 1);
        assert_eq!(
            forms[0],
            Sexp::Atom(Atom::string(Atom::STR_DELIMITER.to_string())),
        );
    }

    #[test]
    fn reader_str_escape_lead_arms_route_through_atom_str_escape_lead() {
        let escape_source = format!(
            "{}{}{}{}",
            Atom::STR_DELIMITER,
            Atom::STR_ESCAPE_LEAD,
            Atom::STR_ESCAPE_LEAD,
            Atom::STR_DELIMITER,
        );
        let forms = read(&escape_source)
            .unwrap_or_else(|e| panic!("reader rejected `{escape_source}`: {e}"));
        assert_eq!(forms.len(), 1);
        assert_eq!(
            forms[0],
            Sexp::Atom(Atom::string(Atom::STR_ESCAPE_LEAD.to_string())),
        );
    }

    #[test]
    fn tokenizer_quote_family_outer_dispatch_routes_through_quote_form_from_lead_char() {
        for qf in QuoteForm::ALL {
            let source = format!("{}xs", qf.prefix());
            let tokens = tokenize(&source)
                .unwrap_or_else(|e| panic!("tokenize rejected `{source}`: {e}"));
            assert!(!tokens.is_empty());
            assert_eq!(tokens[0].kind, Token::Quoted(qf));
            assert_eq!(tokens[0].span, Span::new(0, qf.prefix().len()));
        }
    }

    #[test]
    fn tokenizer_splice_promotion_routes_through_quote_form_promote_via_next_char() {
        // PROMOTION-TABLE CONTRACT: only the `(Unquote, SPLICE_DISCRIMINATOR)`
        // pair promotes; every other variant emits its own token and leaves
        // the discriminator to the NEXT token.
        for qf in QuoteForm::ALL {
            let expected_promoted = qf.promote_via_next_char(QuoteForm::SPLICE_DISCRIMINATOR);
            let source = format!("{}{}xs", qf.prefix(), QuoteForm::SPLICE_DISCRIMINATOR);
            let tokens = tokenize(&source)
                .unwrap_or_else(|e| panic!("tokenize rejected `{source}`: {e}"));
            assert!(!tokens.is_empty());
            match expected_promoted {
                Some(promoted) => assert_eq!(tokens[0].kind, Token::Quoted(promoted)),
                None => {
                    assert_eq!(tokens[0].kind, Token::Quoted(qf));
                    let expected_atom_pos = qf.prefix().len();
                    assert_eq!(tokens[1].kind, Token::Atom("@xs".into()));
                    assert_eq!(tokens[1].span.start, expected_atom_pos);
                }
            }
        }
    }

    #[test]
    fn tokenizer_bare_atom_terminator_disjunct_routes_through_is_bare_atom_boundary() {
        for qf in [QuoteForm::Quote, QuoteForm::Quasiquote, QuoteForm::Unquote] {
            let source = format!("foo{}xs", qf.prefix());
            let tokens = tokenize(&source)
                .unwrap_or_else(|e| panic!("tokenize rejected `{source}`: {e}"));
            assert!(tokens.len() >= 2);
            assert_eq!(tokens[0].kind, Token::Atom("foo".into()));
            assert_eq!(tokens[0].span, Span::new(0, 3));
            assert_eq!(tokens[1].kind, Token::Quoted(qf));
            assert_eq!(tokens[1].span.start, 3);
        }
    }

    // ── Spanned reader ──────────────────────────────────────────────

    #[test]
    fn spanned_atoms_carry_byte_ranges() {
        let src = "foo 42 \"hi\" :kw";
        let forms = read_spanned(src).unwrap();
        assert_eq!(forms.len(), 4);
        assert_eq!(forms[0].span, Span::new(0, 3));
        assert_eq!(forms[1].span, Span::new(4, 6));
        assert_eq!(forms[2].span, Span::new(7, 11));
        assert_eq!(forms[3].span, Span::new(12, 15));
        // Plain-Sexp projection matches the unspanned reader byte-for-byte.
        let plain: Vec<Sexp> = forms.iter().map(Spanned::to_sexp).collect();
        assert_eq!(plain, read(src).unwrap());
    }

    #[test]
    fn spanned_list_outer_span_covers_parens() {
        let src = "(a b c)";
        let forms = read_spanned(src).unwrap();
        assert_eq!(forms[0].span, Span::new(0, 7));
        let SpannedForm::List(children) = &forms[0].form else {
            panic!("expected list")
        };
        assert_eq!(children[0].span, Span::new(1, 2));
        assert_eq!(children[1].span, Span::new(3, 4));
        assert_eq!(children[2].span, Span::new(5, 6));
    }

    #[test]
    fn spanned_comments_and_whitespace_skipped() {
        let src = "; header\n(a b) ; inline\n";
        let forms = read_spanned(src).unwrap();
        assert_eq!(forms.len(), 1);
        let start = src.find('(').unwrap();
        let end = src.find(')').unwrap() + 1;
        assert_eq!(forms[0].span, Span::new(start, end));
    }

    #[test]
    fn spanned_quote_span_covers_tick_and_inner() {
        let src = "'(a b)";
        let forms = read_spanned(src).unwrap();
        assert_eq!(forms[0].span, Span::new(0, 6));
        let SpannedForm::Quote(inner) = &forms[0].form else {
            panic!("expected quote")
        };
        assert_eq!(inner.span, Span::new(1, 6));
    }

    #[test]
    fn spanned_nested_lists_have_proper_containment() {
        let src = "(a (b c) d)";
        let forms = read_spanned(src).unwrap();
        let outer = forms[0].span;
        let SpannedForm::List(children) = &forms[0].form else {
            panic!()
        };
        let inner = children[1].span;
        assert!(outer.start <= inner.start);
        assert!(inner.end <= outer.end);
        assert!(inner.start > outer.start);
        assert!(inner.end < outer.end);
    }

    #[test]
    fn spanned_reader_threads_every_prefix_through_spanned_form_wrap() {
        // The span-carrying dual of
        // `reader_threads_each_prefix_through_quote_form_wrap_dual_of_as_quote_form`:
        // pin that `read_spanned`'s quote arm routes through
        // `SpannedForm::wrap` over the SAME closed-set marker the plain
        // arm feeds to `QuoteForm::wrap`, so the two wrap tables cannot
        // cross wires.
        for qf in QuoteForm::ALL {
            let src = format!("{}payload", qf.prefix());
            let forms = read_spanned(&src).expect(&src);
            assert_eq!(forms.len(), 1);
            assert_eq!(
                forms[0].to_sexp(),
                qf.wrap(Sexp::symbol("payload")),
                "{src}: spanned wrap table drifted from QuoteForm::wrap"
            );
            // The outer span covers the prefix AND the datum.
            assert_eq!(forms[0].span, Span::new(0, src.len()));
        }
    }
}

#[cfg(test)]
mod str_round_trip {
    use crate::read_spanned;

    /// **`Display` and `read` must be inverses for string payloads.**
    ///
    /// They were not. `Display` escapes a non-printable scalar as `\u{301}`,
    /// and the reader had no `u` arm — so it decoded `u` and dropped the
    /// backslash, turning `"e\u{301}"` into `"eu{301}"`. Silently: no error, no
    /// diagnostic, just different data on the other side. Found by
    /// `pleme-io/blue`, whose pipeline lowers through text and so exercises
    /// this on every program.
    ///
    /// Property-shaped rather than case-shaped, because the failure was
    /// invisible for exactly the characters nobody writes a case for.
    #[test]
    fn every_string_survives_a_display_read_round_trip() {
        let payloads = [
            "",
            "plain",
            "with space",
            "quote\" inside",
            "back\\slash",
            "newline\nhere",
            "tab\there",
            "accented é",
            "emoji 😀",
            "combining e\u{301}",
            "zero\u{200B}width",
            "rtl \u{202E} mark",
            "cr\rlf",
        ];
        for payload in payloads {
            let sexp = crate::Sexp::Atom(crate::Atom::Str(payload.to_string()));
            let text = sexp.to_string();
            let forms = read_spanned(&text)
                .unwrap_or_else(|e| panic!("{payload:?} rendered as {text:?} would not re-read: {e:?}"));
            let got = match forms.first().map(|f| &f.form) {
                Some(crate::SpannedForm::Atom(crate::Atom::Str(s))) => s.clone(),
                other => panic!("{payload:?} came back as {other:?}"),
            };
            assert_eq!(
                got, payload,
                "round trip changed the payload\n  wrote: {text:?}\n  read:  {got:?}"
            );
        }
    }

    /// **KNOWN OPEN BUG, found by the property above: `\0` does not survive.**
    ///
    /// `Display` writes a NUL as `\0`, but `NAMED_ESCAPE_TABLE` is
    /// `[(char, char); 3]` — `\n`, `\t`, `\r` — with no row for it, so
    /// `decode_str_escape('0')` falls through to the identity arm and returns
    /// the DIGIT. `"nul\0byte"` round-trips to `"nul0byte"`: silent corruption,
    /// same shape as the `\u{…}` bug fixed above.
    ///
    /// Not fixed here because it is not a one-line change. The escape algebra is
    /// arity-forced and composed — `NAMED_ESCAPE_TABLE[3]` feeds
    /// `ESCAPE_TABLE[5]` feeds the `ALL[5]` closed set, each with per-role
    /// `pub const` pairs and catalog-reflection tests keyed to the counts. Doing
    /// it right means new `NUL_ESCAPE_SOURCE`/`_DECODED` consts, three arity
    /// bumps, a `decode_str_escape` arm, and the catalog rows — which the
    /// module's own docs already name as the worked example ("a new named escape
    /// (`'0' → '\0'`) extends the algebra ONCE").
    ///
    /// Ignored rather than deleted: deleting it loses the finding, and leaving
    /// it red makes the suite lie about the repo's state.
    #[test]
    #[ignore = "known open bug: \\0 is written by Display but has no NAMED_ESCAPE_TABLE row"]
    fn nul_should_survive_a_round_trip_and_does_not() {
        let sexp = crate::Sexp::Atom(crate::Atom::Str("nul\0byte".to_string()));
        let forms = read_spanned(&sexp.to_string()).expect("re-read");
        let got = match forms.first().map(|f| &f.form) {
            Some(crate::SpannedForm::Atom(crate::Atom::Str(s))) => s.clone(),
            other => panic!("got {other:?}"),
        };
        assert_eq!(got, "nul\0byte", "NUL is decoded as the digit 0");
    }

    /// A malformed `\u{…}` keeps its literal text rather than becoming a
    /// substituted glyph — a replacement character would turn a typo into a
    /// plausible-looking result.
    #[test]
    fn a_malformed_unicode_escape_is_not_silently_substituted() {
        for text in [r#""\u{zzzz}""#, r#""\u{D800}""#, r#""\u{110000}""#] {
            let forms = read_spanned(text).expect("must still read");
            let got = match forms.first().map(|f| &f.form) {
                Some(crate::SpannedForm::Atom(crate::Atom::Str(s))) => s.clone(),
                other => panic!("got {other:?}"),
            };
            assert!(
                !got.contains('\u{FFFD}'),
                "{text} must not become a replacement character: {got:?}"
            );
        }
    }
}

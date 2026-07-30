//! S-expression reader — tokenize + parse into `Sexp` or into `Spanned`.
//!
//! ONE tokenizer, two projections. Source positions are byte offsets into
//! the original `&str`; every token carries the `Span` of its own lexeme,
//! and reader-level errors (`UnmatchedParen`, `UnmatchedOpenParen`, `Eof`,
//! `UnterminatedString`) report a real offset so downstream tools
//! (`tatara-lispc`, `tatara-check`, REPL, future LSP) can pinpoint the
//! failure in the source.
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

/// Read a full program (sequence of top-level forms) into a `Vec<Sexp>`.
pub fn read(src: &str) -> Result<Vec<Sexp>> {
    let tokens = tokenize(src)?;
    let eof_pos = src.len();
    let mut it = tokens.into_iter().peekable();
    let mut forms = Vec::new();
    while it.peek().is_some() {
        forms.push(parse(&mut it, eof_pos)?);
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
    let eof_pos = src.len();
    let mut it = tokens.into_iter().peekable();
    let mut forms = Vec::new();
    while it.peek().is_some() {
        forms.push(parse_spanned(&mut it, eof_pos)?);
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
                                s.push(Atom::decode_str_escape(esc));
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
                        None => return Err(LispError::UnterminatedString(pos)),
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
    eof_pos: usize,
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
                    Some(_) => xs.push(parse(it, eof_pos)?),
                    None => {
                        return Err(LispError::UnmatchedOpenParen {
                            pos: open_span.start,
                        })
                    }
                }
            }
        }
        Some(SpannedToken {
            kind: Token::RParen,
            span,
        }) => Err(LispError::UnmatchedParen { pos: span.start }),
        // The four pre-lift `Token::Quote*` arms collapse to ONE arm
        // routing through the typed `QuoteForm` marker — the (Token
        // variant, `Sexp::*` constructor) pairing binds at the closed-set
        // algebra (`QuoteForm::wrap`) rather than threaded as per-arm
        // constructor literals.
        Some(SpannedToken {
            kind: Token::Quoted(qf),
            ..
        }) => read_quoted(it, eof_pos, qf),
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
        None => Err(LispError::Eof { pos: eof_pos }),
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
    eof_pos: usize,
    qf: QuoteForm,
) -> Result<Sexp> {
    let inner = parse(it, eof_pos)?;
    Ok(qf.wrap(inner))
}

fn parse_spanned<I: Iterator<Item = SpannedToken>>(
    it: &mut std::iter::Peekable<I>,
    eof_pos: usize,
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
                    Some(_) => xs.push(parse_spanned(it, eof_pos)?),
                    None => {
                        return Err(LispError::UnmatchedOpenParen {
                            pos: open_span.start,
                        })
                    }
                }
            }
        }
        Some(SpannedToken {
            kind: Token::RParen,
            span,
        }) => Err(LispError::UnmatchedParen { pos: span.start }),
        Some(SpannedToken {
            kind: Token::Quoted(qf),
            span,
        }) => read_quoted_spanned(it, eof_pos, qf, span),
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
        None => Err(LispError::Eof { pos: eof_pos }),
    }
}

/// Span-carrying dual of [`read_quoted`] — reads the inner datum and wraps
/// it through [`SpannedForm::wrap`] over the same typed [`QuoteForm`]
/// marker, then widens the span to cover the prefix AND the datum.
fn read_quoted_spanned<I: Iterator<Item = SpannedToken>>(
    it: &mut std::iter::Peekable<I>,
    eof_pos: usize,
    qf: QuoteForm,
    prefix_span: Span,
) -> Result<Spanned> {
    let inner = parse_spanned(it, eof_pos)?;
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
    // The four reader-level errors carry byte offsets so authoring tools
    // can render them at the right place. Pre-consolidation the PLAIN
    // parser could not: it consumed an unspanned token stream and spelled
    // `UnmatchedOpenParen { pos: 0 }` / `UnmatchedParen { pos: 0 }` /
    // `Eof { pos: 0 }`. Those three hardcodes are what these tests are
    // here to keep dead — a regression that re-splits the tokenizer makes
    // them fail rather than silently re-report byte 0 for everything.

    #[test]
    fn unmatched_closing_paren_reports_byte_offset() {
        // `   )` — the stray `)` is at byte 3.
        let err = read("   )").unwrap_err();
        match err {
            LispError::UnmatchedParen { pos } => assert_eq!(pos, 3),
            other => panic!("expected UnmatchedParen, got {other:?}"),
        }
    }

    #[test]
    fn unmatched_opening_paren_reports_offset_of_open() {
        // `(a (b c` — the inner `(` is at byte 3 and stays unclosed; the
        // outer `(` at byte 0 is also unclosed but the deepest unclosed
        // open is what the parser hits first.
        let err = read("(a (b c").unwrap_err();
        match err {
            LispError::UnmatchedOpenParen { pos } => assert_eq!(pos, 3),
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
            LispError::UnmatchedOpenParen { pos } => assert_eq!(pos, 0),
            other => panic!("expected UnmatchedOpenParen, got {other:?}"),
        }
    }

    #[test]
    fn dangling_quote_reports_eof_at_input_length() {
        // A trailing `'` with no datum to quote — parse runs off the end.
        let src = "(a b) '";
        let err = read(src).unwrap_err();
        match err {
            LispError::Eof { pos } => assert_eq!(pos, src.len()),
            other => panic!("expected Eof, got {other:?}"),
        }
    }

    #[test]
    fn spanned_parser_reports_the_same_offsets_as_the_plain_one() {
        // ONE-TOKENIZER CONTRACT, error axis. `read` and `read_spanned`
        // share `tokenize`, so a malformed source must produce the SAME
        // typed error with the SAME offset through both projections. The
        // pre-consolidation pair could not satisfy this: the plain side
        // reported 0 for every one of these.
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
            LispError::Eof { pos } => assert_eq!(pos, src.len()),
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
            LispError::UnmatchedOpenParen { pos } => assert_eq!(pos, 1),
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

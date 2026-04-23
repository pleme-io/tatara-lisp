//! S-expression reader — tokenize + parse into `Sexp`.

use crate::ast::{Atom, Sexp};
use crate::error::{LispError, Result};
use crate::span::Span;
use crate::spanned::{Spanned, SpannedForm};

#[derive(Clone, Debug, PartialEq)]
enum Token {
    LParen,
    RParen,
    Quote,
    Quasiquote,
    Unquote,
    UnquoteSplice,
    Atom(String),
    Str(String),
}

/// A token with its source span. Produced by `tokenize_spanned` and
/// consumed by `parse_spanned` to build `Spanned` trees.
#[derive(Clone, Debug, PartialEq)]
struct SpannedToken {
    kind: Token,
    span: Span,
}

/// Read a full program (sequence of top-level forms) into a `Vec<Sexp>`.
pub fn read(src: &str) -> Result<Vec<Sexp>> {
    let tokens = tokenize(src)?;
    let mut it = tokens.into_iter().peekable();
    let mut forms = Vec::new();
    while it.peek().is_some() {
        forms.push(parse(&mut it)?);
    }
    Ok(forms)
}

/// Read a full program into `Vec<Spanned>`, where every subtree carries
/// a `Span` pointing back into `src`. Equivalent to `read` in grammar and
/// error reporting; strictly additive.
pub fn read_spanned(src: &str) -> Result<Vec<Spanned>> {
    let tokens = tokenize_spanned(src)?;
    let mut it = tokens.into_iter().peekable();
    let mut forms = Vec::new();
    while it.peek().is_some() {
        forms.push(parse_spanned(&mut it)?);
    }
    Ok(forms)
}

fn tokenize(src: &str) -> Result<Vec<Token>> {
    let mut out = Vec::new();
    let mut chars = src.char_indices().peekable();
    while let Some(&(pos, c)) = chars.peek() {
        match c {
            ws if ws.is_whitespace() => {
                chars.next();
            }
            ';' => {
                while let Some(&(_, ch)) = chars.peek() {
                    chars.next();
                    if ch == '\n' {
                        break;
                    }
                }
            }
            '(' => {
                chars.next();
                out.push(Token::LParen);
            }
            ')' => {
                chars.next();
                out.push(Token::RParen);
            }
            '\'' => {
                chars.next();
                out.push(Token::Quote);
            }
            '`' => {
                chars.next();
                out.push(Token::Quasiquote);
            }
            ',' => {
                chars.next();
                // `,@` is splicing unquote; bare `,` is unquote.
                if chars.peek().map(|&(_, c)| c) == Some('@') {
                    chars.next();
                    out.push(Token::UnquoteSplice);
                } else {
                    out.push(Token::Unquote);
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some((_, '\\')) => {
                            if let Some((_, esc)) = chars.next() {
                                s.push(match esc {
                                    'n' => '\n',
                                    't' => '\t',
                                    'r' => '\r',
                                    '"' => '"',
                                    '\\' => '\\',
                                    other => other,
                                });
                            }
                        }
                        Some((_, '"')) => break,
                        Some((_, ch)) => s.push(ch),
                        None => return Err(LispError::UnterminatedString(pos)),
                    }
                }
                out.push(Token::Str(s));
            }
            _ => {
                let mut s = String::new();
                while let Some(&(_, ch)) = chars.peek() {
                    if ch.is_whitespace()
                        || ch == '('
                        || ch == ')'
                        || ch == '\''
                        || ch == '`'
                        || ch == ','
                        || ch == '"'
                        || ch == ';'
                    {
                        break;
                    }
                    s.push(ch);
                    chars.next();
                }
                out.push(Token::Atom(s));
            }
        }
    }
    Ok(out)
}

fn parse<I: Iterator<Item = Token>>(it: &mut std::iter::Peekable<I>) -> Result<Sexp> {
    match it.next() {
        Some(Token::LParen) => {
            let mut xs = Vec::new();
            loop {
                match it.peek() {
                    Some(Token::RParen) => {
                        it.next();
                        return Ok(Sexp::List(xs));
                    }
                    Some(_) => xs.push(parse(it)?),
                    None => return Err(LispError::UnmatchedOpenParen),
                }
            }
        }
        Some(Token::RParen) => Err(LispError::UnmatchedParen(0)),
        Some(Token::Quote) => {
            let inner = parse(it)?;
            Ok(Sexp::Quote(Box::new(inner)))
        }
        Some(Token::Quasiquote) => {
            let inner = parse(it)?;
            Ok(Sexp::Quasiquote(Box::new(inner)))
        }
        Some(Token::Unquote) => {
            let inner = parse(it)?;
            Ok(Sexp::Unquote(Box::new(inner)))
        }
        Some(Token::UnquoteSplice) => {
            let inner = parse(it)?;
            Ok(Sexp::UnquoteSplice(Box::new(inner)))
        }
        Some(Token::Str(s)) => Ok(Sexp::Atom(Atom::Str(s))),
        Some(Token::Atom(s)) => Ok(atom_from_str(&s)),
        None => Err(LispError::Eof),
    }
}

fn atom_from_str(s: &str) -> Sexp {
    if s == "#t" {
        return Sexp::boolean(true);
    }
    if s == "#f" {
        return Sexp::boolean(false);
    }
    if let Some(rest) = s.strip_prefix(':') {
        return Sexp::keyword(rest);
    }
    if let Ok(n) = s.parse::<i64>() {
        return Sexp::int(n);
    }
    if let Ok(n) = s.parse::<f64>() {
        return Sexp::float(n);
    }
    Sexp::symbol(s)
}

// ── Spanned tokenization + parsing ──────────────────────────────────────
//
// These mirror `tokenize` + `parse` exactly; they differ only in recording
// the byte span of each token/node. Kept parallel rather than unified so
// the hot plain path has zero overhead.

fn tokenize_spanned(src: &str) -> Result<Vec<SpannedToken>> {
    let mut out: Vec<SpannedToken> = Vec::new();
    let mut chars = src.char_indices().peekable();
    while let Some(&(pos, c)) = chars.peek() {
        match c {
            ws if ws.is_whitespace() => {
                chars.next();
            }
            ';' => {
                while let Some(&(_, ch)) = chars.peek() {
                    chars.next();
                    if ch == '\n' {
                        break;
                    }
                }
            }
            '(' => {
                chars.next();
                out.push(SpannedToken {
                    kind: Token::LParen,
                    span: Span::new(pos, pos + 1),
                });
            }
            ')' => {
                chars.next();
                out.push(SpannedToken {
                    kind: Token::RParen,
                    span: Span::new(pos, pos + 1),
                });
            }
            '\'' => {
                chars.next();
                out.push(SpannedToken {
                    kind: Token::Quote,
                    span: Span::new(pos, pos + 1),
                });
            }
            '`' => {
                chars.next();
                out.push(SpannedToken {
                    kind: Token::Quasiquote,
                    span: Span::new(pos, pos + 1),
                });
            }
            ',' => {
                chars.next();
                if chars.peek().map(|&(_, c)| c) == Some('@') {
                    chars.next();
                    out.push(SpannedToken {
                        kind: Token::UnquoteSplice,
                        span: Span::new(pos, pos + 2),
                    });
                } else {
                    out.push(SpannedToken {
                        kind: Token::Unquote,
                        span: Span::new(pos, pos + 1),
                    });
                }
            }
            '"' => {
                chars.next();
                let start = pos;
                let mut s = String::new();
                let end;
                loop {
                    match chars.next() {
                        Some((_, '\\')) => {
                            if let Some((_, esc)) = chars.next() {
                                s.push(match esc {
                                    'n' => '\n',
                                    't' => '\t',
                                    'r' => '\r',
                                    '"' => '"',
                                    '\\' => '\\',
                                    other => other,
                                });
                            }
                        }
                        Some((p, '"')) => {
                            end = p + 1;
                            break;
                        }
                        Some((_, ch)) => {
                            s.push(ch);
                        }
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
                    if ch.is_whitespace()
                        || ch == '('
                        || ch == ')'
                        || ch == '\''
                        || ch == '`'
                        || ch == ','
                        || ch == '"'
                        || ch == ';'
                    {
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

fn parse_spanned<I: Iterator<Item = SpannedToken>>(
    it: &mut std::iter::Peekable<I>,
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
                    Some(_) => xs.push(parse_spanned(it)?),
                    None => return Err(LispError::UnmatchedOpenParen),
                }
            }
        }
        Some(SpannedToken {
            kind: Token::RParen,
            ..
        }) => Err(LispError::UnmatchedParen(0)),
        Some(SpannedToken {
            kind: Token::Quote,
            span: q_span,
        }) => {
            let inner = parse_spanned(it)?;
            let full = Span::new(q_span.start, inner.span.end.max(q_span.end));
            Ok(Spanned::new(full, SpannedForm::Quote(Box::new(inner))))
        }
        Some(SpannedToken {
            kind: Token::Quasiquote,
            span: q_span,
        }) => {
            let inner = parse_spanned(it)?;
            let full = Span::new(q_span.start, inner.span.end.max(q_span.end));
            Ok(Spanned::new(full, SpannedForm::Quasiquote(Box::new(inner))))
        }
        Some(SpannedToken {
            kind: Token::Unquote,
            span: q_span,
        }) => {
            let inner = parse_spanned(it)?;
            let full = Span::new(q_span.start, inner.span.end.max(q_span.end));
            Ok(Spanned::new(full, SpannedForm::Unquote(Box::new(inner))))
        }
        Some(SpannedToken {
            kind: Token::UnquoteSplice,
            span: q_span,
        }) => {
            let inner = parse_spanned(it)?;
            let full = Span::new(q_span.start, inner.span.end.max(q_span.end));
            Ok(Spanned::new(
                full,
                SpannedForm::UnquoteSplice(Box::new(inner)),
            ))
        }
        Some(SpannedToken {
            kind: Token::Str(s),
            span,
        }) => Ok(Spanned::new(span, SpannedForm::Atom(Atom::Str(s)))),
        Some(SpannedToken {
            kind: Token::Atom(s),
            span,
        }) => Ok(Spanned::new(span, spanned_atom_from_str(&s))),
        None => Err(LispError::Eof),
    }
}

fn spanned_atom_from_str(s: &str) -> SpannedForm {
    if s == "#t" {
        return SpannedForm::Atom(Atom::Bool(true));
    }
    if s == "#f" {
        return SpannedForm::Atom(Atom::Bool(false));
    }
    if let Some(rest) = s.strip_prefix(':') {
        return SpannedForm::Atom(Atom::Keyword(rest.to_string()));
    }
    if let Ok(n) = s.parse::<i64>() {
        return SpannedForm::Atom(Atom::Int(n));
    }
    if let Ok(n) = s.parse::<f64>() {
        return SpannedForm::Atom(Atom::Float(n));
    }
    SpannedForm::Atom(Atom::Symbol(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_atoms() {
        let forms = read("foo 42 3.14 \"hello\" :kw #t #f").unwrap();
        assert_eq!(forms.len(), 7);
        assert_eq!(forms[0].as_symbol(), Some("foo"));
        assert_eq!(forms[1], Sexp::int(42));
        assert_eq!(forms[2], Sexp::float(3.14));
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
        // Inner span must be strictly contained in outer span.
        assert!(outer.start <= inner.start);
        assert!(inner.end <= outer.end);
        assert!(inner.start > outer.start);
        assert!(inner.end < outer.end);
    }
}

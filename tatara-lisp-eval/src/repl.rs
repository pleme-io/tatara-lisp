//! Streaming / REPL-style evaluation.
//!
//! Phase 2.2 scaffold: public type + signatures. The reader integration
//! + multi-line continuation detection land in Phase 2.5.

use crate::error::Result;
use crate::eval::Interpreter;
use crate::value::Value;

/// A persistent REPL session against an `Interpreter<H>`.
///
/// Multiple calls to `eval_str` share the same global env — bindings
/// introduced by one call are visible to subsequent calls. Errors in one
/// call do not poison the session.
pub struct ReplSession<'i, H> {
    interp: &'i mut Interpreter<H>,
}

impl<'i, H> ReplSession<'i, H> {
    pub fn new(interp: &'i mut Interpreter<H>) -> Self {
        Self { interp }
    }

    /// Evaluate one or more forms from `input` in the session's env,
    /// returning the value of the last form. Phase 2.5 fills this in.
    pub fn eval_str(&mut self, input: &str, host: &mut H) -> Result<Value> {
        let forms = tatara_lisp::read_spanned(input)?;
        let mut last = Value::Nil;
        for form in &forms {
            last = self.interp.eval_spanned(form, host)?;
        }
        Ok(last)
    }

    /// Paren-balance check — returns `true` when `input` is a complete set
    /// of top-level forms (ready to submit) or `false` when the user's
    /// client should keep collecting more input.
    ///
    /// Phase 2.2: minimal heuristic (open-paren count vs close-paren count,
    /// ignoring string literals). Phase 2.5 refines.
    pub fn is_complete(input: &str) -> bool {
        let mut depth: i32 = 0;
        let mut in_string = false;
        let mut chars = input.chars().peekable();
        while let Some(c) = chars.next() {
            if in_string {
                if c == '\\' {
                    chars.next();
                    continue;
                }
                if c == '"' {
                    in_string = false;
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '(' => depth += 1,
                ')' => depth -= 1,
                ';' => {
                    for nc in chars.by_ref() {
                        if nc == '\n' {
                            break;
                        }
                    }
                }
                _ => {}
            }
            if depth < 0 {
                return true; // unbalanced close — let the reader surface it
            }
        }
        depth == 0 && !in_string
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoHost;

    #[test]
    fn session_evaluates_last_form() {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        let mut s = ReplSession::new(&mut i);
        let mut host = NoHost;
        let v = s.eval_str("42", &mut host).unwrap();
        assert!(matches!(v, Value::Int(42)));
    }

    #[test]
    fn session_returns_last_of_multiple() {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        let mut s = ReplSession::new(&mut i);
        let mut host = NoHost;
        let v = s.eval_str("1 2 3", &mut host).unwrap();
        assert!(matches!(v, Value::Int(3)));
    }

    #[test]
    fn is_complete_balanced() {
        assert!(ReplSession::<()>::is_complete("42"));
        assert!(ReplSession::<()>::is_complete("(foo bar)"));
        assert!(ReplSession::<()>::is_complete("(a (b c) d)"));
        assert!(ReplSession::<()>::is_complete("()"));
    }

    #[test]
    fn is_complete_unbalanced_open() {
        assert!(!ReplSession::<()>::is_complete("(foo"));
        assert!(!ReplSession::<()>::is_complete("(a (b"));
    }

    #[test]
    fn is_complete_ignores_parens_in_strings() {
        assert!(ReplSession::<()>::is_complete("\"(\""));
        assert!(ReplSession::<()>::is_complete("(foo \"a (b c)\")"));
    }

    #[test]
    fn is_complete_ignores_comments() {
        assert!(ReplSession::<()>::is_complete("42 ; a ( comment"));
    }
}

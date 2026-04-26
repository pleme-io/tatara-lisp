//! Special-form dispatch.
//!
//! Phase 2.2 scaffold: enum placeholder for the forms the evaluator
//! recognizes. Implementation lands in Phase 2.3.

/// The set of special forms the evaluator handles directly (as opposed to
/// dispatching through function application).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecialForm {
    Quote,
    Quasiquote,
    If,
    Cond,
    When,
    Unless,
    Let,
    LetStar,
    LetRec,
    Lambda,
    Define,
    Set,
    Begin,
    And,
    Or,
    Not,
    /// `(try body... (catch (binding) handler...))` —
    /// runs body sequentially; if any form raises an `EvalError::User`
    /// (i.e., a Lisp-level `(throw ...)`), the carried Value is bound
    /// to `binding` and `handler...` runs. Bare Rust-side errors
    /// (TypeMismatch, ArityMismatch, etc.) are wrapped into a
    /// `Value::Error` with tag `:runtime` so they can be caught too.
    Try,
    /// `(macroexpand-1 'form)` — evaluate the argument to a code value,
    /// run ONE level of macro expansion if the head is a registered
    /// macro, and return the expanded code as a Value. Useful for
    /// debugging macros — see exactly what one expansion produces.
    MacroexpandOne,
    /// `(macroexpand 'form)` — like macroexpand-1, but fully expand
    /// until no macro calls remain.
    MacroexpandAll,
    /// `(delay expr)` — wrap `expr` as a memoizing thunk. The first
    /// `(force p)` triggers evaluation; subsequent forces return the
    /// cached value. Returns a `Value::Promise`.
    Delay,
}

impl SpecialForm {
    /// Match a head symbol to a special form. Returns `None` if the symbol
    /// is not a recognized special form (interpret as a function call).
    pub fn from_symbol(s: &str) -> Option<Self> {
        Some(match s {
            "quote" => Self::Quote,
            "quasiquote" => Self::Quasiquote,
            "if" => Self::If,
            "cond" => Self::Cond,
            "when" => Self::When,
            "unless" => Self::Unless,
            "let" => Self::Let,
            "let*" => Self::LetStar,
            "letrec" => Self::LetRec,
            "lambda" => Self::Lambda,
            "define" => Self::Define,
            "set!" => Self::Set,
            "begin" => Self::Begin,
            "and" => Self::And,
            "or" => Self::Or,
            "not" => Self::Not,
            "try" => Self::Try,
            "macroexpand-1" => Self::MacroexpandOne,
            "macroexpand" => Self::MacroexpandAll,
            "delay" => Self::Delay,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_core_forms() {
        assert_eq!(SpecialForm::from_symbol("if"), Some(SpecialForm::If));
        assert_eq!(
            SpecialForm::from_symbol("lambda"),
            Some(SpecialForm::Lambda)
        );
        assert_eq!(SpecialForm::from_symbol("let*"), Some(SpecialForm::LetStar));
        assert_eq!(SpecialForm::from_symbol("set!"), Some(SpecialForm::Set));
        assert_eq!(SpecialForm::from_symbol("foo"), None);
    }
}

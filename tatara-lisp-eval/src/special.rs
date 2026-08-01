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
    /// `(eval form)` — evaluate `form` (a runtime Value representing
    /// code, typically a quoted list) at top-level, returning the
    /// result. Unlocks runtime metaprogramming.
    Eval,
    /// `(provide name1 name2 ...)` — declare that the names are
    /// exported from the current module. Errors at top-level (i.e.
    /// outside a `(require)` load).
    Provide,
    /// `(require "path")` — load and evaluate the module at `path`,
    /// register it. With `:as alias`, every exported name becomes
    /// reachable as `alias/name`. With `:refer (a b)`, only the listed
    /// names are imported as bare unqualified bindings.
    Require,
}

/// How the bytecode VM handles a given special form.
///
/// This exists so VM coverage is an **enumerable property** rather than a
/// shape buried in a `match`. Before this type, a form that the VM neither
/// compiled nor listed in `VM_FALLBACK_FORMS` fell through the wildcard in
/// `vm/compile.rs` to `compile_call` and became a call to an unbound global
/// — a *runtime* `VmError::Unbound`, raised only if the branch executed, and
/// carrying a synthetic span so the diagnostic could not point at the code.
///
/// Six forms were in that state when this was added (`quasiquote`, `cond`,
/// `when`, `unless`, `let*`, `letrec`), and the parity harness could not see
/// them: it asserted a *size floor* on its case table, which is a count, not
/// a coverage check. A count cannot witness an absent form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmDisposition {
    /// The VM lowers this form to bytecode natively.
    Compiled,
    /// The VM deliberately defers to the tree-walker via `Op::EvalSexp`.
    ///
    /// Note this is not free: an `EvalSexp` region's continuation lives on
    /// the host stack, so it cannot be parked — which is why the fallback
    /// list is a liability to keep short rather than a place to add forms.
    Fallback,
}

impl SpecialForm {
    /// Every special form, in declaration order.
    ///
    /// The coverage gate walks this. Adding a variant without adding it here
    /// is caught by `all_covers_every_from_symbol_name`.
    pub const ALL: &'static [Self] = &[
        Self::Quote,
        Self::Quasiquote,
        Self::If,
        Self::Cond,
        Self::When,
        Self::Unless,
        Self::Let,
        Self::LetStar,
        Self::LetRec,
        Self::Lambda,
        Self::Define,
        Self::Set,
        Self::Begin,
        Self::And,
        Self::Or,
        Self::Not,
        Self::Try,
        Self::MacroexpandOne,
        Self::MacroexpandAll,
        Self::Delay,
        Self::Eval,
        Self::Provide,
        Self::Require,
    ];

    /// The head symbol that selects this form. Inverse of `from_symbol`.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Quote => "quote",
            Self::Quasiquote => "quasiquote",
            Self::If => "if",
            Self::Cond => "cond",
            Self::When => "when",
            Self::Unless => "unless",
            Self::Let => "let",
            Self::LetStar => "let*",
            Self::LetRec => "letrec",
            Self::Lambda => "lambda",
            Self::Define => "define",
            Self::Set => "set!",
            Self::Begin => "begin",
            Self::And => "and",
            Self::Or => "or",
            Self::Not => "not",
            Self::Try => "try",
            Self::MacroexpandOne => "macroexpand-1",
            Self::MacroexpandAll => "macroexpand",
            Self::Delay => "delay",
            Self::Eval => "eval",
            Self::Provide => "provide",
            Self::Require => "require",
        }
    }

    /// How the bytecode VM handles this form.
    ///
    /// **This is the declaration the VM's dispatch must agree with**, and the
    /// agreement is enforced by `vm::compile`'s coverage test rather than by
    /// review. Every form must be assigned; there is no third state, because
    /// the third state is the silent-miscompile bug this type exists to
    /// remove.
    pub fn vm_disposition(self) -> VmDisposition {
        match self {
            // Lowered to bytecode in `vm/compile.rs::compile_list`.
            Self::Quote
            | Self::If
            | Self::Begin
            | Self::Define
            | Self::Let
            | Self::Lambda
            | Self::Set
            | Self::And
            | Self::Or
            | Self::Not
            | Self::Try => VmDisposition::Compiled,

            // Deferred to the tree-walker: these need `&mut Interpreter`
            // for the loader, the module registry, or runtime expansion.
            Self::Require
            | Self::Provide
            | Self::Delay
            | Self::Eval
            | Self::MacroexpandAll
            | Self::MacroexpandOne => VmDisposition::Fallback,

            // Derived forms. Each has tree-walker semantics the VM does not
            // yet lower; until it does they route through the fallback so
            // they cannot reach the wildcard and miscompile into a call to
            // an unbound global.
            Self::Quasiquote
            | Self::Cond
            | Self::When
            | Self::Unless
            | Self::LetStar
            | Self::LetRec => VmDisposition::Fallback,
        }
    }

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
            "eval" => Self::Eval,
            "provide" => Self::Provide,
            "require" => Self::Require,
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

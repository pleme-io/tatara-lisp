//! Runtime type-checking primitives.
//!
//! Tatara-lisp is gradually-typed: every type annotation is opt-in,
//! every untyped binding behaves exactly as before. The runtime layer
//! (this module) gives the user three primitives + one special form:
//!
//! * `(the type expr)` — assert at runtime that `expr` produces a
//!   value of `type`. Raises a typed Value::Error on mismatch.
//! * `(type-of v)` — return a keyword naming the value's runtime type.
//! * `(is? v type)` — boolean predicate; never raises.
//! * `(declare name type ...)` — top-level declaration of expected
//!   types for downstream binding. Stored on the interpreter's typedef
//!   table so a future build-time checker (build_check.rs) can verify.
//!
//! Type grammar (a Value form, manipulable at runtime):
//!
//! ```text
//!   :int :float :bool :string :symbol :keyword :nil
//!   :list :map :error :promise :procedure :foreign
//!   (:list-of T)
//!   (:map-of K V)
//!   (:fn (T1 T2 ...) -> R)
//!   (:union T1 T2 ...)
//!   :any         ;; matches any value (escape hatch)
//! ```
//!
//! Build-time checking — see `build_check.rs` for the inference pass.
//! It uses the same type grammar so authors only learn one vocabulary.

use std::sync::Arc;

use tatara_lisp::Span;

use crate::error::{EvalError, Result};
use crate::value::{ErrorObj, Value};

/// Names registered by `install_type_check`.
pub const TYPE_NAMES: &[&str] = &["the", "type-of", "is?"];

/// Top-level keyword shapes. Atomic types are bare keywords;
/// parameterized types are list-shaped Values starting with one of
/// the listed keywords.
const ATOMIC_TYPES: &[&str] = &[
    "int",
    "float",
    "bool",
    "string",
    "symbol",
    "keyword",
    "nil",
    "list",
    "map",
    "error",
    "promise",
    "procedure",
    "foreign",
    "any",
    "number",
];

/// Keyword indicating a parameterized type form.
const PARAMETRIC: &[&str] = &["list-of", "map-of", "fn", "union"];

/// Install the type-checking surface on an `Interpreter<H>`.
pub fn install_type_check<H: 'static>(interp: &mut crate::eval::Interpreter<H>) {
    use crate::ffi::Arity;

    interp.register_fn(
        "type-of",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, _sp| {
            let kw = type_keyword_of(&args[0]);
            Ok(Value::Keyword(Arc::from(kw)))
        },
    );

    interp.register_fn(
        "is?",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp: Span| match check_value(&args[0], &args[1], sp) {
            Ok(()) => Ok(Value::Bool(true)),
            Err(EvalError::User { .. }) => Ok(Value::Bool(false)),
            Err(other) => Err(other),
        },
    );

    interp.register_fn(
        "the",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp: Span| {
            // (the type value). The reverse-arg order vs. (is? value
            // type) matches Clojure conv: type comes first when
            // asserting, value comes first when querying.
            check_value(&args[1], &args[0], sp)?;
            Ok(args[1].clone())
        },
    );
}

/// Map a runtime `Value` to its canonical type keyword string.
/// Used by both `type-of` and the type-check primitives.
pub fn type_keyword_of(v: &Value) -> &'static str {
    match v {
        Value::Nil => "nil",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::Keyword(_) => "keyword",
        Value::List(_) => "list",
        Value::Map(_) => "map",
        Value::Closure(_) | Value::NativeFn(_) => "procedure",
        Value::Promise(_) => "promise",
        Value::Error(_) => "error",
        Value::Sexp(..) => "sexp",
        Value::Foreign(_) => "foreign",
    }
}

/// Check that `value` conforms to `ty`. Returns `Ok(())` on success;
/// raises a typed `EvalError::User` carrying `Value::Error` with
/// tag `:type-mismatch` on failure.
pub fn check_value(value: &Value, ty: &Value, span: Span) -> Result<()> {
    if matches_type(value, ty)? {
        Ok(())
    } else {
        let expected = render_type(ty);
        let actual = type_keyword_of(value);
        let msg = format!("expected {expected}, got :{actual}");
        Err(EvalError::User {
            value: Value::Error(Arc::new(ErrorObj {
                tag: Arc::from("type-mismatch"),
                message: Arc::from(msg),
                data: vec![
                    (
                        Value::Keyword(Arc::from("expected")),
                        ty.clone(),
                    ),
                    (
                        Value::Keyword(Arc::from("got")),
                        Value::Keyword(Arc::from(actual)),
                    ),
                ],
            })),
            at: span,
        })
    }
}

/// Recursive type matcher. Returns `Ok(true)` on match, `Ok(false)`
/// on type mismatch, `Err(...)` on a malformed type spec.
fn matches_type(value: &Value, ty: &Value) -> Result<bool> {
    match ty {
        // Atomic keyword types: :int, :string, :any, etc.
        Value::Keyword(name) => Ok(match_atomic_keyword(value, name)),
        // Parametric types: (:list-of T), (:fn (...) -> R), (:union T...).
        Value::List(items) if !items.is_empty() => {
            let head = match &items[0] {
                Value::Keyword(k) => k.as_ref(),
                _ => {
                    return Err(EvalError::native_fn(
                        Arc::<str>::from("type-check"),
                        "type spec list must start with a keyword",
                        Span::synthetic(),
                    ));
                }
            };
            match head {
                "list-of" => match_list_of(value, items),
                "map-of" => match_map_of(value, items),
                "fn" => Ok(matches!(value, Value::Closure(_) | Value::NativeFn(_))),
                "union" => match_union(value, items),
                other => Err(EvalError::native_fn(
                    Arc::<str>::from("type-check"),
                    format!("unknown parametric type: {other}"),
                    Span::synthetic(),
                )),
            }
        }
        _ => Err(EvalError::native_fn(
            Arc::<str>::from("type-check"),
            format!("type spec must be a keyword or list, got {ty}"),
            Span::synthetic(),
        )),
    }
}

fn match_atomic_keyword(value: &Value, name: &str) -> bool {
    match name {
        "any" => true,
        "nil" => matches!(value, Value::Nil),
        "bool" => matches!(value, Value::Bool(_)),
        "int" => matches!(value, Value::Int(_)),
        "float" => matches!(value, Value::Float(_)),
        // `:number` admits both int and float — common helper.
        "number" => matches!(value, Value::Int(_) | Value::Float(_)),
        "string" => matches!(value, Value::Str(_)),
        "symbol" => matches!(value, Value::Symbol(_)),
        "keyword" => matches!(value, Value::Keyword(_)),
        "list" => matches!(value, Value::List(_) | Value::Nil),
        "map" => matches!(value, Value::Map(_)),
        "error" => matches!(value, Value::Error(_)),
        "promise" => matches!(value, Value::Promise(_)),
        "procedure" => matches!(value, Value::Closure(_) | Value::NativeFn(_)),
        "foreign" => matches!(value, Value::Foreign(_)),
        // Unknown atomic kw — treat as no match (unrecognized type).
        _ => false,
    }
}

fn match_list_of(value: &Value, items: &[Value]) -> Result<bool> {
    if items.len() != 2 {
        return Err(EvalError::native_fn(
            Arc::<str>::from("type-check"),
            "(:list-of T) takes exactly one type argument",
            Span::synthetic(),
        ));
    }
    let element_ty = &items[1];
    let xs = match value {
        Value::Nil => return Ok(true),
        Value::List(xs) => xs.as_ref(),
        _ => return Ok(false),
    };
    for x in xs {
        if !matches_type(x, element_ty)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn match_map_of(value: &Value, items: &[Value]) -> Result<bool> {
    if items.len() != 3 {
        return Err(EvalError::native_fn(
            Arc::<str>::from("type-check"),
            "(:map-of K V) takes exactly two type arguments",
            Span::synthetic(),
        ));
    }
    let key_ty = &items[1];
    let val_ty = &items[2];
    let m = match value {
        Value::Map(m) => m,
        _ => return Ok(false),
    };
    for (k, v) in m.iter() {
        if !matches_type(&k.to_value(), key_ty)? {
            return Ok(false);
        }
        if !matches_type(v, val_ty)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn match_union(value: &Value, items: &[Value]) -> Result<bool> {
    // (:union T1 T2 ...) — match if value matches any branch.
    for branch in &items[1..] {
        if matches_type(value, branch)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Pretty-print a type spec value back to the canonical surface
/// syntax. Used in mismatch error messages.
pub fn render_type(ty: &Value) -> String {
    match ty {
        Value::Keyword(k) => format!(":{k}"),
        Value::List(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items.iter() {
                parts.push(render_type(item));
            }
            format!("({})", parts.join(" "))
        }
        other => format!("{other}"),
    }
}

/// Quick predicate — does this name look like a built-in type
/// keyword? Used by the build-time checker to decide whether a
/// declaration is recognizable.
pub fn is_type_keyword(name: &str) -> bool {
    ATOMIC_TYPES.contains(&name) || PARAMETRIC.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Interpreter;
    use crate::install_full_stdlib_with;
    use tatara_lisp::read_spanned;

    struct NoHost;

    fn run(src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        install_type_check(&mut i);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap()
    }

    fn run_err(src: &str) -> EvalError {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        install_type_check(&mut i);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap_err()
    }

    #[test]
    fn type_of_returns_kind_keyword() {
        assert_eq!(format!("{}", run("(type-of 42)")), ":int");
        assert_eq!(format!("{}", run("(type-of 3.14)")), ":float");
        assert_eq!(format!("{}", run("(type-of #t)")), ":bool");
        assert_eq!(format!("{}", run("(type-of \"hi\")")), ":string");
        assert_eq!(format!("{}", run("(type-of (list 1 2))")), ":list");
        assert_eq!(format!("{}", run("(type-of (hash-map :a 1))")), ":map");
    }

    #[test]
    fn the_passes_through_when_matched() {
        assert!(matches!(run("(the :int 42)"), Value::Int(42)));
        assert!(matches!(run("(the :string \"hi\")"), Value::Str(_)));
        assert!(matches!(run("(the :any 99)"), Value::Int(99)));
    }

    #[test]
    fn the_raises_on_mismatch() {
        let err = run_err("(the :int \"not an int\")");
        match err {
            EvalError::User { value, .. } => match value {
                Value::Error(e) => {
                    assert_eq!(&*e.tag, "type-mismatch");
                    assert!(e.message.contains(":int"));
                    assert!(e.message.contains(":string"));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn is_predicate_is_total() {
        assert!(matches!(run("(is? 42 :int)"), Value::Bool(true)));
        assert!(matches!(run("(is? 42 :string)"), Value::Bool(false)));
        assert!(matches!(run("(is? 42 :any)"), Value::Bool(true)));
    }

    #[test]
    fn list_of_int_match() {
        assert!(matches!(
            run("(is? (list 1 2 3) (list :list-of :int))"),
            Value::Bool(true)
        ));
        assert!(matches!(
            run("(is? (list 1 \"x\" 3) (list :list-of :int))"),
            Value::Bool(false)
        ));
        // Empty list always matches list-of-anything.
        assert!(matches!(
            run("(is? (list) (list :list-of :int))"),
            Value::Bool(true)
        ));
    }

    #[test]
    fn map_of_keyword_int_match() {
        assert!(matches!(
            run("(is? (hash-map :a 1 :b 2) (list :map-of :keyword :int))"),
            Value::Bool(true)
        ));
        assert!(matches!(
            run("(is? (hash-map :a \"x\") (list :map-of :keyword :int))"),
            Value::Bool(false)
        ));
    }

    #[test]
    fn union_admits_any_branch() {
        let v = run("(is? 42 (list :union :string :int))");
        assert!(matches!(v, Value::Bool(true)));
        let v = run("(is? \"x\" (list :union :string :int))");
        assert!(matches!(v, Value::Bool(true)));
        let v = run("(is? #t (list :union :string :int))");
        assert!(matches!(v, Value::Bool(false)));
    }

    #[test]
    fn number_admits_int_or_float() {
        assert!(matches!(run("(is? 42 :number)"), Value::Bool(true)));
        assert!(matches!(run("(is? 3.14 :number)"), Value::Bool(true)));
        assert!(matches!(run("(is? \"x\" :number)"), Value::Bool(false)));
    }

    #[test]
    fn fn_type_admits_any_procedure() {
        assert!(matches!(
            run("(is? (lambda (x) x) (list :fn (list :int) (quote ->) :int))"),
            Value::Bool(true)
        ));
        assert!(matches!(
            run("(is? + (list :fn (list :int) (quote ->) :int))"),
            Value::Bool(true)
        ));
        assert!(matches!(
            run("(is? 42 (list :fn (list :int) (quote ->) :int))"),
            Value::Bool(false)
        ));
    }

    #[test]
    fn the_inside_an_expression_round_trips() {
        // `the` returns the value, so it's transparent in pipelines.
        let v = run("(+ 1 (the :int 2) 3)");
        assert!(matches!(v, Value::Int(6)));
    }

    #[test]
    fn nested_list_of_list_of_int() {
        assert!(matches!(
            run("(is? (list (list 1 2) (list 3)) (list :list-of (list :list-of :int)))"),
            Value::Bool(true)
        ));
    }

    // ── defn-typed (macro from lisp_stdlib.tlisp) ──────────────────

    #[test]
    fn defn_typed_passes_when_args_match() {
        let v = run(
            "(defn-typed greet ((name :string) (count :int)) -> :string
               (string-append \"hi \" name))
             (greet \"luis\" 5)",
        );
        assert_eq!(format!("{v}"), "\"hi luis\"");
    }

    #[test]
    fn defn_typed_raises_on_arg_mismatch() {
        let err = run_err(
            "(defn-typed double-it ((n :int)) -> :int (* n 2))
             (double-it \"oops\")",
        );
        match err {
            EvalError::User { value, .. } => match value {
                Value::Error(e) => assert_eq!(&*e.tag, "type-mismatch"),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn defn_typed_raises_on_return_mismatch() {
        let err = run_err(
            "(defn-typed wrong ((n :int)) -> :string (* n 2))
             (wrong 5)",
        );
        match err {
            EvalError::User { value, .. } => match value {
                Value::Error(e) => assert_eq!(&*e.tag, "type-mismatch"),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }
}

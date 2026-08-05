//! Runtime type-checking primitives.
//!
//! Tatara-lisp is gradually-typed: every type annotation is opt-in,
//! every untyped binding behaves exactly as before. The runtime layer
//! (this module) gives the user three primitives + one special form:
//!
//! * `(the type expr)` — assert at runtime that `expr` produces a
//!   value of `type`. Raises a typed Value::Error on mismatch.
//! * `(cast type expr)` — CHECKED COERCION. Same assertion as `the`
//!   for every non-numeric type, plus a genuine narrowing at the seven
//!   Rust numeric widths, rejecting a value the width cannot hold
//!   instead of returning a truncated one.
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
//!   :i32 :i64 :u32 :u64 :usize :f32 :f64   ;; the seven Rust widths
//!                                          ;; :int  IS :i64
//!                                          ;; :float IS :f64
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

use tatara_closed_set::ClosedSet;
use tatara_lisp::{NumericAxis, NumericLiteral, NumericWidth, Span};

use crate::error::{EvalError, Result};
use crate::value::{ErrorObj, Value};

/// Names registered by `install_type_check`.
pub const TYPE_NAMES: &[&str] = &["the", "type-of", "is?", "cast"];

/// Atomic keyword shapes that are NOT numeric. The numeric half of the
/// vocabulary is deliberately absent here: it is projected from
/// [`NumericWidth`]'s closed set plus [`WIDTH_ALIASES`] by
/// [`atomic_type_keywords`], so there is exactly ONE place a numeric
/// type keyword can be spelled.
///
/// `:int` and `:float` used to be listed here AND handled by
/// [`numeric_width_of`] AND given their own arms in
/// [`match_atomic_keyword`] — three spellings of one fact, and they
/// disagreed. See [`WIDTH_ALIASES`].
const NON_NUMERIC_ATOMIC_TYPES: &[&str] = &[
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

/// The two ALIAS spellings for the identity widths — THE one table.
///
/// `:int` is the tatara-lisp spelling of `i64` and `:float` of `f64`,
/// which is exactly what the reader's `extract_int` / `extract_float`
/// return. Every consumer that needs "is this keyword numeric, and at
/// which width" reads this table through [`numeric_width_of`] /
/// [`is_width_alias`] rather than re-spelling the two names.
///
/// **This table exists because the duplicate spelling was a shipped
/// bug.** [`match_atomic_keyword`] carried its own early `"int"` /
/// `"float"` arms that shadowed the width path, so `(is? 7 :float)`
/// answered `#f` while `(cast :float 7)` answered `7.0` and
/// `(is? 7 :f64)` answered `#t` — an alias disagreeing with the width
/// it aliases, and the predicate disagreeing with the caster it is
/// supposed to describe. Those arms are gone; the aliases now reach the
/// same [`wide_literal_on`] + [`NumericWidth::narrow_literal`] chain
/// `coerce_value` reaches, so the three cannot differ.
const WIDTH_ALIASES: [(&str, NumericWidth); 2] =
    [("int", NumericWidth::I64), ("float", NumericWidth::F64)];

/// EVERY atomic type keyword the runtime accepts, in one projection.
///
/// The numeric half is swept out of [`NumericWidth`]'s closed set plus
/// [`WIDTH_ALIASES`] rather than re-listed, which is what makes this
/// usable as a TEST domain: a sweep written against this function
/// cannot silently omit the label where a defect lives, the way a
/// hand-written `[:u32, :i32, :f32]` subset omitted `:int` / `:float`
/// — the only two labels the shipped predicate/caster disagreement
/// actually reached.
///
/// Parameterized heads ([`PARAMETRIC`]) are not here: they are list
/// heads, not bare keywords.
#[must_use]
pub fn atomic_type_keywords() -> Vec<&'static str> {
    let mut out = NON_NUMERIC_ATOMIC_TYPES.to_vec();
    out.extend(WIDTH_ALIASES.iter().map(|&(alias, _)| alias));
    out.extend(NumericWidth::ALL.iter().map(|w| ClosedSet::label(*w)));
    out
}

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

    interp.register_fn(
        "cast",
        Arity::Exact(2),
        // (cast type value) — same argument order as `the`, because it
        // is the same act with a stronger obligation: assert, and where
        // the target is a numeric WIDTH, actually perform the narrowing.
        |args: &[Value], _h: &mut H, sp: Span| coerce_value(&args[1], &args[0], sp),
    );
}

/// Decode a type keyword into one of the seven Rust numeric widths, or
/// `None` if it names something else.
///
/// The vocabulary is swept out of [`NumericWidth::ALL`] rather than
/// re-listed here. That is the whole point: the derive's closed set and
/// the caster's accepted keywords are the SAME seven, so an eighth
/// width added to the enum is reachable from lisp the moment it exists
/// and cannot be reachable under a name the derive would not recognise.
///
/// `:int` and `:float` are ALIASES for the two identity widths — the
/// tatara-lisp spellings of the reader's own `i64` / `f64` axes, which
/// is exactly what `extract_int` / `extract_float` return. Aliasing
/// them (rather than routing them down the plain-assertion path) is
/// what makes `(cast :int x)` literally the derive's `i64` narrowing:
/// total, so it never rejects — the same answer `NarrowNumeric<i64>
/// for i64` gives.
///
/// The canonical half is [`ClosedSet::find_by_label`] — the trait's own
/// `ALL` + `label` sweep — not a hand-rolled
/// `ALL.into_iter().find(|w| w.label() == name)` re-derivation of it.
/// `NumericWidth` derives `DeriveClosedSet`, so a future canonicalizing
/// label projection lands once on the trait and reaches this decoder
/// for free.
pub(crate) fn numeric_width_of(name: &str) -> Option<NumericWidth> {
    NumericWidth::find_by_label(name).or_else(|| width_alias_of(name))
}

/// Resolve one of the two [`WIDTH_ALIASES`] spellings, or `None`.
fn width_alias_of(name: &str) -> Option<NumericWidth> {
    WIDTH_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map(|&(_, width)| width)
}

/// Is `name` an ALIAS spelling rather than a canonical width label?
///
/// Reads the same [`WIDTH_ALIASES`] table [`numeric_width_of`] does, so
/// a consumer that must treat the two spellings differently — the
/// build-time checker renders `:int` as `:int`, never as `:i64` —
/// cannot drift out of step with the decoder.
pub(crate) fn is_width_alias(name: &str) -> bool {
    width_alias_of(name).is_some()
}

/// Zero-allocation membership peer of [`numeric_width_of`], for the
/// `if`-gates that only need the yes/no.
///
/// The canonical half is [`ClosedSet::contains_label`], the trait's own
/// pure-membership predicate — precisely the primitive
/// [`is_type_keyword`] used to re-derive.
fn is_numeric_width_keyword(name: &str) -> bool {
    NumericWidth::contains_label(name) || is_width_alias(name)
}

/// Project a runtime `Value` onto the wide axis a [`NumericWidth`]
/// narrows from. `None` means the value is not on that axis at all —
/// the SHAPE gate, which the derive fails inside `extract_int` /
/// `extract_float` before any narrowing is attempted.
///
/// The two axes are DELIBERATELY ASYMMETRIC, mirroring
/// `tatara_lisp::Sexp` exactly:
///
/// * `Sexp::as_int` is strict — a float atom is not an int, so
///   `(cast :u32 3.5)` is a shape mismatch, as `:port 3.5` is on the
///   derive.
/// * `Sexp::as_float` is `a.as_float().or_else(|| a.as_int().map(|n| n
///   as f64))` — it WIDENS an int atom, so `#[derive(TataraDomain)]`
///   accepts `:scale 7` into an `f32` field, and `(cast :f32 7)` must
///   therefore answer `7.0` rather than reject.
///
/// That widening is an `as` cast, lossy above 2^53, and we reproduce it
/// rather than improve on it. Tightening it here would be a DIVERGENCE
/// from the derive — two surfaces disagreeing about the same literal —
/// which is worse than a shared, documented, testable imprecision. If
/// it should be fixed, it gets fixed once at `Sexp::as_float` and both
/// surfaces move together.
fn wide_literal_on(value: &Value, axis: NumericAxis) -> Option<NumericLiteral> {
    match (axis, value) {
        (NumericAxis::Int, Value::Int(n)) => Some(NumericLiteral::Int(*n)),
        (NumericAxis::Float, Value::Float(x)) => Some(NumericLiteral::Float(*x)),
        (NumericAxis::Float, Value::Int(n)) =>
        {
            #[allow(clippy::cast_precision_loss)]
            Some(NumericLiteral::Float(*n as f64))
        }
        _ => None,
    }
}

/// Rebuild a runtime `Value` from a narrowed literal. Total — the two
/// `NumericLiteral` variants are exactly the two numeric `Value` arms.
fn value_of_literal(lit: NumericLiteral) -> Value {
    match lit {
        NumericLiteral::Int(n) => Value::Int(n),
        NumericLiteral::Float(x) => Value::Float(x),
    }
}

/// Checked coercion — the engine behind `(cast type value)`.
///
/// Two gates, in the derive's own order, so the two surfaces classify a
/// failure identically:
///
/// 1. **Shape.** Is the value on the target width's axis at all?
///    `(cast :u32 "8080")` is not, and fails through [`check_value`]
///    with the ordinary `:type-mismatch` — the same rejection
///    `#[derive(TataraDomain)]` takes inside `extract_int` before it
///    ever narrows.
/// 2. **Width.** Can the target hold the value? `(cast :u32 -1)` cannot,
///    and fails with `:out-of-range`, quoting the width and the
///    author's literal. This is [`NumericWidth::narrow_literal`], which
///    is the derive's own [`tatara_lisp::NarrowNumeric`] impls under a
///    runtime dispatch — not a second bounds table.
///
/// A non-numeric target has no width gate and degrades to exactly what
/// `(the type value)` does, so `cast` is a superset of `the` rather
/// than a parallel vocabulary.
pub fn coerce_value(value: &Value, ty: &Value, span: Span) -> Result<Value> {
    let width = match ty {
        Value::Keyword(name) => numeric_width_of(name),
        _ => None,
    };
    let Some(width) = width else {
        check_value(value, ty, span)?;
        return Ok(value.clone());
    };

    // Gate 1 — shape. Delegating the diagnostic to `check_value` keeps
    // ONE rendering of "expected X, got Y"; it is guaranteed to fail
    // here because `match_atomic_keyword`'s width arm and this routing
    // read the same `wide_literal_on`.
    let Some(wide) = wide_literal_on(value, width.axis()) else {
        check_value(value, ty, span)?;
        return Ok(value.clone());
    };

    // Gate 2 — width.
    width
        .narrow_literal(wide)
        .map(value_of_literal)
        .ok_or_else(|| out_of_range(width, wide, value, span))
}

/// Raise the width-gate rejection, sibling to [`check_value`]'s
/// shape-gate one: an `EvalError::User` carrying a `Value::Error`, so a
/// lisp-level `try` / `catch` binds it the way it binds every other
/// runtime type failure, and the tag tells the two gates apart without
/// parsing the message.
///
/// The message is the TAIL of [`tatara_lisp::LispError::KwargOutOfRange`]'s
/// own rendering — `"{value} is out of range for {target}"` — built
/// from the same `NumericLiteral` and `NumericWidth` `Display` impls.
/// The derive's full string prefixes `"compile error in :<kwarg>: "`,
/// which a cast has no kwarg to fill; the `cast_message_is_the_tail_of
/// _the_derive_diagnostic` test pins the two against each other so the
/// wording cannot drift apart.
fn out_of_range(
    target: NumericWidth,
    literal: NumericLiteral,
    original: &Value,
    span: Span,
) -> EvalError {
    let msg = format!("{literal} is out of range for {target}");
    EvalError::User {
        value: Value::Error(Arc::new(ErrorObj {
            tag: Arc::from("out-of-range"),
            message: Arc::from(msg),
            data: vec![
                (
                    Value::Keyword(Arc::from("target")),
                    Value::Keyword(Arc::from(target.label())),
                ),
                (Value::Keyword(Arc::from("value")), original.clone()),
            ],
        })),
        at: span,
    }
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
                    (Value::Keyword(Arc::from("expected")), ty.clone()),
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
        // NOTE — there are deliberately no `"int"` / `"float"` arms
        // here. They are ALIASES for `i64` / `f64` (see
        // [`WIDTH_ALIASES`]) and fall through to the width arm below,
        // which is the SAME chain `coerce_value` walks. Two early arms
        // used to shadow that fall-through, and that is exactly how the
        // predicate and the caster came to disagree about
        // `(is? 7 :float)`. A third hand-written numeric arm here
        // re-opens the class.
        //
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
        // One of the seven Rust numeric widths. A width is a genuine
        // runtime type here — `:u32` is the set of ints this fleet's
        // `#[derive(TataraDomain)]` would accept into a `u32` field —
        // so `(is? -1 :u32)` is `#f`, not a "type I don't recognise".
        // Sharing ONE vocabulary with `cast` is deliberate: a caster
        // with its own private width table would be free to disagree
        // with the predicate that is supposed to describe it.
        other => numeric_width_of(other).is_some_and(|w| {
            wide_literal_on(value, w.axis()).is_some_and(|lit| w.narrow_literal(lit).is_some())
        }),
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
    NON_NUMERIC_ATOMIC_TYPES.contains(&name)
        || PARAMETRIC.contains(&name)
        || is_numeric_width_keyword(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_full_stdlib_with;
    use crate::Interpreter;
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

    /// `run` without the `unwrap` — the sweeps below ask "did this
    /// ACCEPT or REJECT" thousands of times and must not panic on the
    /// reject half.
    fn try_run(src: &str) -> Result<Value> {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        install_type_check(&mut i);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost)
    }

    /// The value corpus every cross-surface sweep runs each keyword
    /// against. Deliberately dense around the numeric gates — both
    /// sides of every width boundary, both axes, plus one witness of
    /// each non-numeric shape so the sweep is a whole-vocabulary
    /// statement and not a numeric one.
    const WITNESSES: &[&str] = &[
        "7",
        "-1",
        "0",
        "3.5",
        "0.1",
        "4294967296",
        "2147483648",
        "-9223372036854775808",
        "1.0e300",
        "\"x\"",
        "#t",
        "(list 1 2)",
        "(hash-map :a 1)",
        "(quote sym)",
        ":kw",
        "(lambda (x) x)",
    ];

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

    // ── (cast T x) — checked coercion ──────────────────────────────
    //
    // Two gates in the derive's own order (shape, then width) and two
    // distinguishable error identities. Every rejection below is a
    // value `#[derive(TataraDomain)]` also rejects, at the same width,
    // with the same wording — that agreement is what
    // `cast_message_is_the_tail_of_the_derive_diagnostic` pins
    // mechanically rather than by eye.

    /// Bind the `Value::Error` a failing `cast` raises. Panics loudly on
    /// any other error shape, so "it errored" can never be mistaken for
    /// "it errored the way we claim".
    fn cast_err(src: &str) -> Arc<ErrorObj> {
        match run_err(src) {
            EvalError::User {
                value: Value::Error(e),
                ..
            } => e,
            other => panic!("expected a typed Value::Error from cast, got {other:?}"),
        }
    }

    /// A cast that succeeds is transparent — it returns the value, so it
    /// composes inside an expression exactly like `the`.
    #[test]
    fn cast_returns_the_value_when_it_fits() {
        assert!(matches!(run("(cast :int 42)"), Value::Int(42)));
        assert!(matches!(run("(cast :u32 8080)"), Value::Int(8080)));
        assert!(matches!(run("(cast :i32 -42)"), Value::Int(-42)));
        assert!(matches!(
            run("(cast :u32 4294967295)"),
            Value::Int(4_294_967_295)
        ));
        assert!(matches!(run("(+ 1 (cast :u32 2) 3)"), Value::Int(6)));
    }

    /// Non-numeric targets have no width gate — `cast` degrades to
    /// exactly what `the` does, so it is a superset of `the` rather
    /// than a second, subtly different vocabulary.
    #[test]
    fn cast_on_a_non_numeric_type_behaves_as_the_assertion() {
        assert!(matches!(run("(cast :string \"hi\")"), Value::Str(_)));
        assert!(matches!(run("(cast :any 99)"), Value::Int(99)));
        assert!(matches!(
            run("(cast (list :list-of :int) (list 1 2 3))"),
            Value::List(_)
        ));
        let e = cast_err("(cast :string 42)");
        assert_eq!(&*e.tag, "type-mismatch");
    }

    /// GATE 1 — shape. The value is not on the target width's axis at
    /// all. The derive fails this inside `extract_int` before narrowing,
    /// and so does the caster: `:type-mismatch`, NOT `:out-of-range`.
    /// Reporting "3.5 is out of range for u32" would name the wrong gate.
    #[test]
    fn cast_off_the_targets_axis_is_a_shape_mismatch_not_a_range_one() {
        for src in [
            "(cast :u32 \"8080\")",
            "(cast :u32 3.5)",
            "(cast :i32 #t)",
            "(cast :f32 \"7\")",
            "(cast :f32 #t)",
        ] {
            let e = cast_err(src);
            assert_eq!(&*e.tag, "type-mismatch", "{src} named the wrong gate");
        }
    }

    /// The two axes are ASYMMETRIC and the caster copies the asymmetry
    /// rather than tidying it. `Sexp::as_float` widens an int atom, so
    /// `#[derive(TataraDomain)]` accepts `:scale 7` into an `f32` field;
    /// `Sexp::as_int` does NOT accept a float atom, so `:port 3.5` is a
    /// shape rejection there. `(cast :f32 7)` and `(cast :u32 3.5)` must
    /// answer the same way, and the FIRST of those is the one an
    /// intuitively-tidier "an int is not a float" caster gets wrong.
    #[test]
    fn cast_copies_the_readers_int_into_float_widening() {
        assert!(matches!(run("(cast :f32 7)"), Value::Float(x) if (x - 7.0).abs() < f64::EPSILON));
        assert!(matches!(run("(cast :f64 7)"), Value::Float(x) if (x - 7.0).abs() < f64::EPSILON));
        assert!(
            matches!(run("(cast :float 7)"), Value::Float(x) if (x - 7.0).abs() < f64::EPSILON)
        );
        // The other direction stays STRICT, because `Sexp::as_int` is.
        assert_eq!(&*cast_err("(cast :u32 3.5)").tag, "type-mismatch");
        assert_eq!(&*cast_err("(cast :int 3.5)").tag, "type-mismatch");
    }

    /// GATE 2 — width. The four values S2 measured the derive
    /// truncating are rejected here too, at the same width. Pre-S2 the
    /// derive's `as` cast turned each of these into the number in the
    /// comment; a caster that returned those numbers would have
    /// re-opened the class on a new surface.
    #[test]
    fn cast_rejects_every_value_the_derive_rejects() {
        for (src, width) in [
            ("(cast :u32 4294967296)", "u32"), // pre-S2: 0
            ("(cast :u32 -1)", "u32"),         // pre-S2: 4294967295
            ("(cast :i32 2147483648)", "i32"), // pre-S2: -2147483648
            ("(cast :f32 1.0e300)", "f32"),    // pre-S2: inf
        ] {
            let e = cast_err(src);
            assert_eq!(&*e.tag, "out-of-range", "{src} named the wrong gate");
            assert!(
                e.message.contains(width),
                "{src} must name the width it failed to fit, got {}",
                e.message
            );
        }
    }

    /// THE AGREEMENT PIN. The caster's message is byte-for-byte the tail
    /// of the derive's own `LispError::KwargOutOfRange` rendering for the
    /// same (value, width) pair — the derive prefixes the kwarg path a
    /// cast has none of, and nothing else differs. Both sides are built
    /// from the same `NumericLiteral` / `NumericWidth` `Display`, so this
    /// fails the moment either wording moves.
    #[test]
    fn cast_message_is_the_tail_of_the_derive_diagnostic() {
        for (src, target, literal) in [
            (
                "(cast :u32 4294967296)",
                NumericWidth::U32,
                NumericLiteral::Int(4_294_967_296),
            ),
            ("(cast :u32 -1)", NumericWidth::U32, NumericLiteral::Int(-1)),
            (
                "(cast :i32 2147483648)",
                NumericWidth::I32,
                NumericLiteral::Int(2_147_483_648),
            ),
            (
                "(cast :f32 1.0e300)",
                NumericWidth::F32,
                NumericLiteral::Float(1.0e300),
            ),
        ] {
            let derived = tatara_lisp::LispError::KwargOutOfRange {
                form: tatara_lisp::KwargPath::named("port"),
                target,
                value: literal,
            }
            .to_string();
            let cast = cast_err(src).message.to_string();
            assert_eq!(
                derived,
                format!("compile error in :port: {cast}"),
                "the caster and the derive disagree about how to say this"
            );
        }
    }

    /// The typed payload, not just the message: an authoring surface
    /// pattern-matches `:target` / `:value` rather than parsing prose.
    #[test]
    fn the_range_rejection_carries_the_width_and_the_value_as_data() {
        let e = cast_err("(cast :u32 -1)");
        let target = e
            .data
            .iter()
            .find(|(k, _)| matches!(k, Value::Keyword(k) if &**k == "target"))
            .map(|(_, v)| format!("{v}"));
        let value = e
            .data
            .iter()
            .find(|(k, _)| matches!(k, Value::Keyword(k) if &**k == "value"))
            .map(|(_, v)| format!("{v}"));
        assert_eq!(target.as_deref(), Some(":u32"));
        assert_eq!(value.as_deref(), Some("-1"));
    }

    /// `:int` / `:float` are the identity widths (`i64` / `f64`), and
    /// `NarrowNumeric` makes those TOTAL — so `(cast :int x)` never
    /// rejects an int, exactly as the derive's `i64` fields never
    /// reject one. Same trait, same verdict.
    #[test]
    fn cast_to_the_identity_widths_never_rejects() {
        assert!(matches!(
            run("(cast :int -9223372036854775808)"),
            Value::Int(i64::MIN)
        ));
        assert!(matches!(
            run("(cast :i64 -9223372036854775808)"),
            Value::Int(i64::MIN)
        ));
        assert!(matches!(run("(cast :f64 1.0e300)"), Value::Float(_)));
        assert!(matches!(run("(cast :float 1.0e300)"), Value::Float(_)));
    }

    /// An `f32` cast returns the value at `f32` PRECISION, not the value
    /// the author typed. Anything else would report a coercion it did
    /// not perform. `0.1` is the same witness the derive's
    /// `narrowing_accepts_every_in_range_value_including_lossy_f32` uses.
    #[test]
    fn cast_to_f32_returns_the_value_at_f32_precision() {
        #[allow(clippy::cast_possible_truncation)]
        let expected = f64::from(0.1_f64 as f32);
        match run("(cast :f32 0.1)") {
            Value::Float(x) => {
                assert!((x - expected).abs() < f64::EPSILON, "got {x}");
                assert_ne!(x, 0.1_f64, "the coercion must be real, not a pass-through");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A failing cast is CATCHABLE, not a panic and not a process abort
    /// — it rides the same `EvalError::User` / `Value::Error` channel
    /// every other runtime type failure does.
    #[test]
    fn a_failing_cast_is_catchable_lisp_data() {
        let v = run("(try
               (cast :u32 -1)
               (catch (e) (error-tag e)))");
        assert!(matches!(v, Value::Keyword(s) if &*s == "out-of-range"));
    }

    /// The width keywords are ONE vocabulary shared with the predicate,
    /// so `is?` describes exactly what `cast` will accept. A caster with
    /// its own private width table would let these two disagree.
    #[test]
    fn the_width_predicate_agrees_with_the_caster() {
        assert!(matches!(run("(is? 8080 :u32)"), Value::Bool(true)));
        assert!(matches!(run("(is? -1 :u32)"), Value::Bool(false)));
        assert!(matches!(run("(is? 4294967296 :u32)"), Value::Bool(false)));
        assert!(matches!(run("(is? 2147483648 :i32)"), Value::Bool(false)));
        assert!(matches!(run("(is? 1.0e300 :f32)"), Value::Bool(false)));
        assert!(matches!(run("(is? 2.5 :f32)"), Value::Bool(true)));
        // …including the reader's asymmetry: an int IS an `:f32` (the
        // widening `Sexp::as_float` performs), a float is NOT a `:u32`.
        assert!(matches!(run("(is? 7 :f32)"), Value::Bool(true)));
        assert!(matches!(run("(is? 3.5 :u32)"), Value::Bool(false)));
    }

    /// The accepted keyword vocabulary IS `NumericWidth::ALL` — swept,
    /// not re-listed. An eighth width added to the closed set becomes
    /// castable the moment it exists, and no keyword the derive would
    /// not recognise is castable at all.
    #[test]
    fn every_closed_set_width_is_reachable_as_a_type_keyword() {
        for w in NumericWidth::ALL {
            assert_eq!(
                numeric_width_of(w.label()),
                Some(w),
                "{w} is in the closed set but not reachable from lisp"
            );
            assert!(is_type_keyword(w.label()));
        }
        assert_eq!(numeric_width_of("i8"), None, "i8 is not in the closed set");
        assert_eq!(numeric_width_of("u16"), None);
        assert_eq!(numeric_width_of("nonsense"), None);
    }

    // ── defn-typed (macro from lisp_stdlib.tlisp) ──────────────────

    #[test]
    fn defn_typed_passes_when_args_match() {
        let v = run("(defn-typed greet ((name :string) (count :int)) -> :string
               (string-append \"hi \" name))
             (greet \"luis\" 5)");
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

    // ── WHOLE-VOCABULARY AGREEMENT SWEEPS ──────────────────────────
    //
    // Three defects shipped in the `(cast T x)` commit, and all three
    // survived a green suite for the SAME reason: every existing test
    // named the labels it checked by hand. The width sweep above
    // (`the_width_predicate_agrees_with_the_caster`) listed `:u32`,
    // `:i32`, `:f32` — and the disagreement lived at `:int` / `:float`,
    // the only two labels it did not name.
    //
    // So these sweep the DOMAIN, never a subset of it:
    // `atomic_type_keywords()` is the closed set + the alias table +
    // the non-numeric names, projected — so a keyword that exists is
    // a keyword that gets swept, including one added tomorrow.

    /// DEFECT 1 — the predicate and the caster must accept exactly the
    /// same set, for every keyword and every witness.
    ///
    /// `is?` is documented as the boolean face of what `cast` will do;
    /// two hand-written arms in `match_atomic_keyword` made that false
    /// at `:float`, where `(is? 7 :float)` said `#f` and
    /// `(cast :float 7)` returned `7.0`.
    #[test]
    fn every_type_keyword_agrees_between_the_predicate_and_the_caster() {
        let mut disagreements = Vec::new();
        for kw in atomic_type_keywords() {
            for witness in WITNESSES {
                let predicate = matches!(
                    try_run(&format!("(is? {witness} :{kw})")),
                    Ok(Value::Bool(true))
                );
                let caster = try_run(&format!("(cast :{kw} {witness})")).is_ok();
                if predicate != caster {
                    disagreements.push(format!(
                        "  (is? {witness} :{kw}) = {predicate}  but  (cast :{kw} {witness}) \
                         accepted = {caster}"
                    ));
                }
            }
        }
        assert!(
            disagreements.is_empty(),
            "the predicate must describe the caster exactly — {} disagreement(s):\n{}",
            disagreements.len(),
            disagreements.join("\n")
        );
    }

    /// DEFECT 1, the other half — an ALIAS must be indistinguishable
    /// from the width it aliases. `:float` IS `:f64`; if the two answer
    /// differently on any witness, one of them has grown a private
    /// implementation.
    #[test]
    fn every_width_alias_behaves_exactly_like_the_width_it_aliases() {
        let mut drift = Vec::new();
        for (alias, width) in WIDTH_ALIASES {
            assert_eq!(
                numeric_width_of(alias),
                Some(width),
                ":{alias} must decode to the width it documents"
            );
            let canonical = ClosedSet::label(width);
            for witness in WITNESSES {
                let alias_is = matches!(
                    try_run(&format!("(is? {witness} :{alias})")),
                    Ok(Value::Bool(true))
                );
                let canonical_is = matches!(
                    try_run(&format!("(is? {witness} :{canonical})")),
                    Ok(Value::Bool(true))
                );
                let alias_cast = try_run(&format!("(cast :{alias} {witness})")).is_ok();
                let canonical_cast = try_run(&format!("(cast :{canonical} {witness})")).is_ok();
                if alias_is != canonical_is {
                    drift.push(format!(
                        "  (is? {witness} :{alias}) = {alias_is}  but  \
                         (is? {witness} :{canonical}) = {canonical_is}"
                    ));
                }
                if alias_cast != canonical_cast {
                    drift.push(format!(
                        "  (cast :{alias} {witness}) accepted = {alias_cast}  but  \
                         (cast :{canonical} {witness}) accepted = {canonical_cast}"
                    ));
                }
            }
        }
        assert!(
            drift.is_empty(),
            "an alias diverged from the width it aliases — {} case(s):\n{}",
            drift.len(),
            drift.join("\n")
        );
    }

    /// DEFECT 3 — vocabulary parity. Every keyword the runtime accepts
    /// must PARSE as a build-check type spec. A keyword that does not
    /// becomes `BadTypeSpec`, so the two surfaces disagree about
    /// whether the program is even well-typed: `(the :u32 8080)` ran
    /// fine and was flagged at build-check.
    #[test]
    fn every_runtime_type_keyword_is_a_build_check_type_spec() {
        let mut unparsed = Vec::new();
        for kw in atomic_type_keywords() {
            let src = format!(":{kw}");
            let forms = read_spanned(&src).unwrap();
            if crate::build_check::StaticType::from_spanned(&forms[0]).is_none() {
                unparsed.push(src);
            }
        }
        assert!(
            unparsed.is_empty(),
            "the runtime accepts {} keyword(s) the build checker calls a bad type spec: {}",
            unparsed.len(),
            unparsed.join(" ")
        );
    }

    /// DEFECT 3, the behavioural half — the build checker must never
    /// FLAG an annotation the runtime accepts.
    ///
    /// One direction only, on purpose: the pass is deliberately
    /// incomplete (it carries no literal values, so it cannot know
    /// `4294967296` overflows a `:u32`), and conceding is sound. Being
    /// stricter than the runtime is not — it fails a program that runs.
    #[test]
    fn the_build_checker_never_flags_an_annotation_the_runtime_accepts() {
        let mut false_positives = Vec::new();
        for kw in atomic_type_keywords() {
            for witness in WITNESSES {
                let src = format!("(the :{kw} {witness})");
                if try_run(&src).is_err() {
                    // The runtime rejects it; the build checker is free
                    // to say anything at all about it.
                    continue;
                }
                let forms = read_spanned(&src).unwrap();
                for d in crate::build_check::check_program(&forms) {
                    false_positives.push(format!("  {src} → {}", d.render(&src)));
                }
            }
        }
        assert!(
            false_positives.is_empty(),
            "the build checker flagged {} annotation(s) the runtime accepts:\n{}",
            false_positives.len(),
            false_positives.join("\n")
        );
    }

    /// DEFECT 2 — the vocabulary is a PROJECTION, not a hand-list.
    ///
    /// Pins that every closed-set width and every alias is reachable,
    /// that nothing outside them is, and that no keyword is spelled
    /// twice — a name appearing in two tables is precisely the shape
    /// that let `:int` / `:float` be handled in two places and diverge.
    #[test]
    fn the_type_keyword_vocabulary_is_swept_not_hand_listed() {
        let vocabulary = atomic_type_keywords();
        for width in NumericWidth::ALL {
            let label = ClosedSet::label(width);
            assert!(
                vocabulary.contains(&label),
                "{label} is in the closed set but not in the type vocabulary"
            );
            assert!(is_type_keyword(label), "{label} must be a type keyword");
        }
        for (alias, _) in WIDTH_ALIASES {
            assert!(
                vocabulary.contains(&alias),
                ":{alias} is not in the vocabulary"
            );
            assert!(is_type_keyword(alias), ":{alias} must be a type keyword");
        }
        for outsider in ["i8", "u16", "i128", "nonsense", ""] {
            assert!(
                !is_type_keyword(outsider),
                ":{outsider} is not in the closed set and must not be a type keyword"
            );
        }
        let mut sorted = vocabulary.clone();
        sorted.sort_unstable();
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(
            sorted, deduped,
            "a keyword is spelled in two tables — that duplication IS the defect class"
        );
    }
}

//! JSON parse + stringify, mapping to the tatara-lisp `Value` tree.
//!
//!   (json-parse STR)      → nested Value (null → nil, objects → alist)
//!   (json-stringify V)    → string
//!   (alist-get ALIST KEY) → value at KEY, or nil
//!   (alist-get ALIST KEY DEFAULT) → value at KEY, or DEFAULT

use std::sync::Arc;

use serde_json::Value as JsonValue;
use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "json-parse",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "json-parse", sp)?;
            let parsed: JsonValue = serde_json::from_str(&s)
                .map_err(|e| EvalError::native_fn("json-parse", e.to_string(), sp))?;
            Ok(json_to_value(&parsed))
        },
    );

    interp.register_fn(
        "json-stringify",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = serde_json::to_string(&value_to_json(&args[0]))
                .map_err(|e| EvalError::native_fn("json-stringify", e.to_string(), sp))?;
            Ok(Value::Str(Arc::from(s)))
        },
    );

    interp.register_fn(
        "alist-get",
        Arity::Range(2, 3),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let key = match &args[1] {
                Value::Str(s) => s.clone(),
                Value::Symbol(s) | Value::Keyword(s) => s.clone(),
                other => {
                    return Err(EvalError::native_fn(
                        "alist-get",
                        format!("key must be string/symbol/keyword, got {}", other.type_name()),
                        sp,
                    ))
                }
            };
            let default = args.get(2).cloned().unwrap_or(Value::Nil);
            Ok(alist_lookup(&args[0], &key).unwrap_or(default))
        },
    );
}

/// Convert a `serde_json::Value` into a tatara-lisp `Value`.
/// Objects become association lists: `((key . v) (key . v) ...)` where
/// each pair is a 2-element list for easy alist-get lookup.
pub fn json_to_value(j: &JsonValue) -> Value {
    match j {
        JsonValue::Null => Value::Nil,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        JsonValue::String(s) => Value::Str(Arc::from(s.as_str())),
        JsonValue::Array(xs) => Value::list(xs.iter().map(json_to_value).collect()),
        JsonValue::Object(m) => Value::list(
            m.iter()
                .map(|(k, v)| {
                    Value::list(vec![Value::Str(Arc::from(k.as_str())), json_to_value(v)])
                })
                .collect(),
        ),
    }
}

/// Convert a tatara-lisp `Value` into a `serde_json::Value` for serialization.
/// Closures / native fns / foreign / quoted-sexp collapse to `null`.
pub fn value_to_json(v: &Value) -> JsonValue {
    match v {
        Value::Nil => JsonValue::Null,
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Int(n) => JsonValue::Number((*n).into()),
        Value::Float(n) => serde_json::Number::from_f64(*n)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Value::Str(s) | Value::Symbol(s) | Value::Keyword(s) => {
            JsonValue::String(s.as_ref().to_owned())
        }
        Value::List(xs) => {
            // Heuristic: if every element is a 2-list with a string first,
            // treat it as an object; else array.
            let looks_like_object = !xs.is_empty()
                && xs.iter().all(|entry| {
                    if let Value::List(pair) = entry {
                        pair.len() == 2 && matches!(pair[0], Value::Str(_) | Value::Symbol(_) | Value::Keyword(_))
                    } else {
                        false
                    }
                });
            if looks_like_object {
                let mut m = serde_json::Map::with_capacity(xs.len());
                for entry in xs.iter() {
                    if let Value::List(pair) = entry {
                        let k = match &pair[0] {
                            Value::Str(s) | Value::Symbol(s) | Value::Keyword(s) => {
                                s.as_ref().to_owned()
                            }
                            _ => unreachable!(),
                        };
                        m.insert(k, value_to_json(&pair[1]));
                    }
                }
                JsonValue::Object(m)
            } else {
                JsonValue::Array(xs.iter().map(value_to_json).collect())
            }
        }
        _ => JsonValue::Null,
    }
}

/// Look up `key` in an alist represented as a list of 2-element lists.
fn alist_lookup(alist: &Value, key: &str) -> Option<Value> {
    let Value::List(entries) = alist else {
        return None;
    };
    for entry in entries.iter() {
        let Value::List(pair) = entry else { continue };
        if pair.len() != 2 {
            continue;
        }
        let matches = match &pair[0] {
            Value::Str(s) | Value::Symbol(s) | Value::Keyword(s) => s.as_ref() == key,
            _ => false,
        };
        if matches {
            return Some(pair[1].clone());
        }
    }
    None
}

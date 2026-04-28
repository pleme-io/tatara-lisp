//! TOML parse + stringify. Same `Value` shape as JSON/YAML (objects
//! become alists of 2-lists) so `alist-get` works uniformly.
//!
//!   (toml-parse STR)       → nested Value
//!   (toml-read PATH)       → parse a file
//!   (toml-stringify VALUE) → TOML text

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};
use toml::Value as TomlValue;

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "toml-parse",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "toml-parse", sp)?;
            let parsed: TomlValue = s.parse().map_err(|e: toml::de::Error| {
                EvalError::native_fn("toml-parse", e.to_string(), sp)
            })?;
            Ok(toml_to_value(&parsed))
        },
    );

    interp.register_fn(
        "toml-read",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "toml-read", sp)?;
            let body = std::fs::read_to_string(&*path)
                .map_err(|e| EvalError::native_fn("toml-read", format!("{path}: {e}"), sp))?;
            let parsed: TomlValue = body.parse().map_err(|e: toml::de::Error| {
                EvalError::native_fn("toml-read", e.to_string(), sp)
            })?;
            Ok(toml_to_value(&parsed))
        },
    );

    interp.register_fn(
        "toml-stringify",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let tv = value_to_toml(&args[0]).ok_or_else(|| {
                EvalError::native_fn(
                    "toml-stringify",
                    "TOML requires a table at the root".to_string(),
                    sp,
                )
            })?;
            let s = toml::to_string(&tv)
                .map_err(|e| EvalError::native_fn("toml-stringify", e.to_string(), sp))?;
            Ok(Value::Str(Arc::from(s)))
        },
    );
}

fn toml_to_value(t: &TomlValue) -> Value {
    match t {
        TomlValue::String(s) => Value::Str(Arc::from(s.as_str())),
        TomlValue::Integer(n) => Value::Int(*n),
        TomlValue::Float(f) => Value::Float(*f),
        TomlValue::Boolean(b) => Value::Bool(*b),
        TomlValue::Datetime(d) => Value::Str(Arc::from(d.to_string())),
        TomlValue::Array(xs) => Value::list(xs.iter().map(toml_to_value).collect::<Vec<_>>()),
        TomlValue::Table(m) => Value::list(
            m.iter()
                .map(|(k, v)| {
                    Value::list(vec![Value::Str(Arc::from(k.as_str())), toml_to_value(v)])
                })
                .collect::<Vec<_>>(),
        ),
    }
}

fn value_to_toml(v: &Value) -> Option<TomlValue> {
    match v {
        Value::Nil => None,
        Value::Bool(b) => Some(TomlValue::Boolean(*b)),
        Value::Int(n) => Some(TomlValue::Integer(*n)),
        Value::Float(f) => Some(TomlValue::Float(*f)),
        Value::Str(s) | Value::Symbol(s) | Value::Keyword(s) => {
            Some(TomlValue::String(s.as_ref().to_owned()))
        }
        Value::List(xs) => {
            let looks_like_table = !xs.is_empty()
                && xs.iter().all(|entry| {
                    if let Value::List(pair) = entry {
                        pair.len() == 2
                            && matches!(
                                pair[0],
                                Value::Str(_) | Value::Symbol(_) | Value::Keyword(_)
                            )
                    } else {
                        false
                    }
                });
            if looks_like_table {
                let mut m = toml::map::Map::new();
                for entry in xs.iter() {
                    if let Value::List(pair) = entry {
                        let k = match &pair[0] {
                            Value::Str(s) | Value::Symbol(s) | Value::Keyword(s) => {
                                s.as_ref().to_owned()
                            }
                            _ => unreachable!(),
                        };
                        if let Some(v) = value_to_toml(&pair[1]) {
                            m.insert(k, v);
                        }
                    }
                }
                Some(TomlValue::Table(m))
            } else {
                Some(TomlValue::Array(
                    xs.iter().filter_map(value_to_toml).collect(),
                ))
            }
        }
        _ => None,
    }
}

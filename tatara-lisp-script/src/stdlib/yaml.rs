//! YAML parse → tatara-lisp `Value` tree. Shape matches the JSON mapping
//! (objects → alists of 2-lists), so `alist-get` works identically for
//! YAML and JSON documents.
//!
//!   (yaml-parse STR) → nested Value
//!   (yaml-read PATH) → same but from a file path

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "yaml-parse",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let s = str_arg(&args[0], "yaml-parse", sp)?;
            let parsed: serde_yaml::Value = serde_yaml::from_str(&s)
                .map_err(|e| EvalError::native_fn("yaml-parse", e.to_string(), sp))?;
            Ok(yaml_to_value(&parsed))
        },
    );

    interp.register_fn(
        "yaml-read",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "yaml-read", sp)?;
            let body = std::fs::read_to_string(&*path)
                .map_err(|e| EvalError::native_fn("yaml-read", format!("{path}: {e}"), sp))?;
            let parsed: serde_yaml::Value = serde_yaml::from_str(&body)
                .map_err(|e| EvalError::native_fn("yaml-read", format!("{path}: {e}"), sp))?;
            Ok(yaml_to_value(&parsed))
        },
    );
}

fn yaml_to_value(y: &serde_yaml::Value) -> Value {
    match y {
        serde_yaml::Value::Null => Value::Nil,
        serde_yaml::Value::Bool(b) => Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_yaml::Value::String(s) => Value::Str(Arc::from(s.as_str())),
        serde_yaml::Value::Sequence(xs) => {
            Value::list(xs.iter().map(yaml_to_value).collect::<Vec<_>>())
        }
        serde_yaml::Value::Mapping(m) => Value::list(
            m.iter()
                .map(|(k, v)| {
                    let key_str = match k {
                        serde_yaml::Value::String(s) => s.clone(),
                        other => serde_yaml::to_string(other)
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                    };
                    Value::list(vec![Value::Str(Arc::from(key_str)), yaml_to_value(v)])
                })
                .collect::<Vec<_>>(),
        ),
        serde_yaml::Value::Tagged(t) => yaml_to_value(&t.value),
    }
}

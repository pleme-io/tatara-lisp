//! JSON parse + stringify, mapping to the tatara-lisp `Value` tree.
//!
//!   (json-parse STR)      → nested Value (null → nil, objects → alist)
//!   (json-stringify V)    → string
//!   (alist-get ALIST KEY) → value at KEY, or nil
//!   (alist-get ALIST KEY DEFAULT) → value at KEY, or DEFAULT

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;
use tatara_lisp_eval::{Arity, EvalError, Interpreter, MapKey, Value};

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
                        format!(
                            "key must be string/symbol/keyword, got {}",
                            other.type_name()
                        ),
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
        JsonValue::Array(xs) => Value::list(xs.iter().map(json_to_value).collect::<Vec<_>>()),
        // The EMPTY object is the one case the alist representation cannot
        // carry: `{}` and `[]` both become the empty list, and `value_to_json`
        // then has nothing left to decide on, so it picks `[]` and the object
        // is gone. That is not hypothetical — it silently rewrote every
        // credsStore-backed `"<registry>": {}` entry in ~/.docker/config.json
        // to `"<registry>": []` on each home-manager activation, which the
        // docker CLI rejects outright:
        //   json: cannot unmarshal array into Go struct field
        //   ConfigFile.auths of type types.AuthConfig
        // i.e. one activation of an unrelated script bricked the docker CLI
        // for every registry on the machine.
        //
        // `Value::Map` is the representation that CAN say "object" with no
        // entries, so the empty case uses it and round-trips exactly. Every
        // non-empty object stays an alist: that is the shape the whole
        // authoring surface (`alist-get`, `alist-upsert`, the `as-alist`
        // idiom) is written against, and changing it is a separate, much
        // larger move — see the KNOWN REMAINING AMBIGUITY note on
        // `value_to_json`.
        JsonValue::Object(m) if m.is_empty() => Value::Map(Arc::new(HashMap::new())),
        JsonValue::Object(m) => Value::list(
            m.iter()
                .map(|(k, v)| {
                    Value::list(vec![Value::Str(Arc::from(k.as_str())), json_to_value(v)])
                })
                .collect::<Vec<_>>(),
        ),
    }
}

/// Convert a tatara-lisp `Value` into a `serde_json::Value` for serialization.
/// Closures / native fns / foreign / quoted-sexp collapse to `null`.
///
/// KNOWN REMAINING AMBIGUITY (deliberate, not overlooked). The list arm below
/// decides object-vs-array by *shape*, so a genuine JSON array whose every
/// element is a 2-element array with a string first — `[["a",1],["b",2]]` —
/// still stringifies as `{"a":1,"b":2}`. That is the same lossy heuristic the
/// empty-object case above escaped, and the destination is the same for both:
/// objects are `Value::Map`, arrays are `Value::List`, and round-trip is
/// identity by construction rather than by heuristic. Getting there means
/// migrating every `alist-get`/`alist-upsert` caller in the fleet, so it is a
/// separate change; the empty case was split out first because it was actively
/// corrupting a file on every activation and the array-of-pairs case has never
/// been observed in fleet data.
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
                        pair.len() == 2
                            && matches!(
                                pair[0],
                                Value::Str(_) | Value::Symbol(_) | Value::Keyword(_)
                            )
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
        // A Map is unambiguously an object — the only Value that is. Before
        // this arm existed it fell through to `_ => Null`, so every Map that
        // reached json-stringify serialized as `null`.
        //
        // JSON object keys are strings, so non-string keys render through
        // their scalar spelling rather than being dropped: an entry that
        // silently vanished would be worse than one that is findable under
        // "1" or "true".
        Value::Map(m) => JsonValue::Object(
            m.iter()
                .map(|(k, v)| (map_key_to_json_key(k), value_to_json(v)))
                .collect(),
        ),
        _ => JsonValue::Null,
    }
}

/// Render a `MapKey` as a JSON object key. Total by construction — every
/// variant has a spelling, so no entry can be dropped on the way out.
fn map_key_to_json_key(k: &MapKey) -> String {
    match k {
        MapKey::Str(s) | MapKey::Symbol(s) | MapKey::Keyword(s) => s.as_ref().to_owned(),
        MapKey::Nil => "null".to_owned(),
        MapKey::Bool(b) => b.to_string(),
        MapKey::Int(n) => n.to_string(),
        MapKey::Float(bits) => f64::from_bits(*bits).to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `json-parse` then `json-stringify` must be identity on the JSON value,
    /// which is the property the whole "read a config, upsert one leaf, write
    /// it back" idiom rests on. Compared as parsed values, so key order is not
    /// asserted.
    fn assert_round_trips(src: &str) {
        let parsed: JsonValue = serde_json::from_str(src).expect("fixture is valid JSON");
        let out = value_to_json(&json_to_value(&parsed));
        assert_eq!(out, parsed, "round-trip changed the document\nin:  {src}");
    }

    #[test]
    fn empty_object_round_trips() {
        assert_round_trips("{}");
        assert_round_trips(r#"{"a":{}}"#);
        assert_round_trips(r#"{"a":{"b":{}}}"#);
        assert_round_trips(r#"[{},{}]"#);
    }

    #[test]
    fn empty_array_is_not_confused_for_an_object() {
        assert_round_trips("[]");
        assert_round_trips(r#"{"a":[]}"#);
        // The two empties must stay distinguishable side by side.
        assert_round_trips(r#"{"obj":{},"arr":[]}"#);
    }

    /// The exact document class this fix was written for. Docker Desktop
    /// writes a bare `{}` for every registry whose credentials live in the
    /// credential store; round-tripping that through the old code produced
    /// `"ghcr.io":[]`, which the docker CLI refuses to unmarshal, taking down
    /// every registry on the machine.
    #[test]
    fn docker_config_with_credstore_entries_survives() {
        let src = r#"{
            "auths": {
                "ghcr.io": {},
                "localhost:5000": {},
                "registry.example.com": {"auth":"dXNlcjpwYXNz"}
            },
            "credsStore": "desktop",
            "currentContext": "desktop-linux",
            "features": {"hooks":"true"}
        }"#;
        assert_round_trips(src);

        // And it must still be an object after the round trip, not merely
        // equal to something — assert the shape the CLI actually requires.
        let parsed: JsonValue = serde_json::from_str(src).unwrap();
        let out = value_to_json(&json_to_value(&parsed));
        assert!(
            out["auths"]["ghcr.io"].is_object(),
            "credsStore-backed entry must stay an object, got {}",
            out["auths"]["ghcr.io"]
        );
    }

    #[test]
    fn non_empty_objects_stay_alists() {
        // The authoring surface (alist-get / alist-upsert) depends on this.
        let v = json_to_value(&serde_json::json!({"a": 1}));
        assert!(matches!(v, Value::List(_)), "non-empty object must be an alist");
        // `Value` has no PartialEq, so match the shape rather than compare.
        assert!(matches!(alist_lookup(&v, "a"), Some(Value::Int(1))));
    }

    #[test]
    fn map_serializes_as_object_not_null() {
        let mut m = HashMap::new();
        m.insert(MapKey::Str(Arc::from("k")), Value::Int(7));
        let out = value_to_json(&Value::Map(Arc::new(m)));
        assert_eq!(out, serde_json::json!({"k": 7}));
    }

    #[test]
    fn map_with_non_string_keys_keeps_every_entry() {
        let mut m = HashMap::new();
        m.insert(MapKey::Int(1), Value::Str(Arc::from("one")));
        m.insert(MapKey::Bool(true), Value::Str(Arc::from("yes")));
        let out = value_to_json(&Value::Map(Arc::new(m)));
        assert_eq!(out, serde_json::json!({"1": "one", "true": "yes"}));
    }
}

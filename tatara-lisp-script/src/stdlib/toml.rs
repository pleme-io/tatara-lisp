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
            // `Table`, not `Value`. Under toml 0.9 (spec 1.1) `FromStr for
            // Value` parses a single TOML *value*, so a DOCUMENT — the only
            // thing anyone passes here — failed on its first `key = …`, and
            // even `""` failed. Documents parse as `Table`.
            let parsed: toml::Table = s.parse().map_err(|e: toml::de::Error| {
                EvalError::native_fn("toml-parse", e.to_string(), sp)
            })?;
            Ok(toml_to_value(&TomlValue::Table(parsed)))
        },
    );

    interp.register_fn(
        "toml-read",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let path = str_arg(&args[0], "toml-read", sp)?;
            let body = std::fs::read_to_string(&*path)
                .map_err(|e| EvalError::native_fn("toml-read", format!("{path}: {e}"), sp))?;
            // See the note in `toml-parse`: a file is a document, so `Table`.
            let parsed: toml::Table = body.parse().map_err(|e: toml::de::Error| {
                EvalError::native_fn("toml-read", e.to_string(), sp)
            })?;
            Ok(toml_to_value(&TomlValue::Table(parsed)))
        },
    );

    interp.register_fn(
        "toml-stringify",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let tv = root_toml(&args[0]).ok_or_else(|| {
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

/// `value_to_toml`, plus the one decision only the root can make.
///
/// An empty alist and an empty array are the SAME `Value`, so the shared
/// mapper cannot tell them apart and guesses Array. At the root that guess is
/// always wrong — TOML's grammar admits only a table there — so it is resolved
/// here rather than by making the mapper guess differently everywhere else.
fn root_toml(v: &Value) -> Option<TomlValue> {
    match value_to_toml(v)? {
        TomlValue::Array(a) if a.is_empty() => Some(TomlValue::Table(toml::map::Map::new())),
        other => Some(other),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A DOCUMENT must parse. Under toml 0.9 (spec 1.1) `FromStr for Value`
    /// parses a single TOML *value*, so parsing a document through `Value`
    /// rejected every real input — including the empty document — with
    /// "unexpected content, expected nothing". There were no TOML tests in
    /// this crate at all, which is why it shipped that way.
    fn parse_document(src: &str) -> Value {
        let table: toml::Table = src
            .parse()
            .unwrap_or_else(|e| panic!("document must parse: {src:?}: {e}"));
        toml_to_value(&TomlValue::Table(table))
    }

    /// `Value` has no `PartialEq`, so the assertion goes back through TOML:
    /// an empty document must survive the trip as an empty document.
    #[test]
    fn an_empty_document_parses_to_an_empty_table() {
        let v = parse_document("");
        let back = root_toml(&v).expect("root is a table");
        assert_eq!(toml::to_string(&back).expect("serialises"), "");
    }

    /// The shape that first exposed the bug: attic's `config.toml` opens with
    /// `default-server = "…"`. Dashes are legal in TOML bare keys.
    #[test]
    fn a_hyphenated_bare_key_parses() {
        let v = parse_document(r#"default-server = "nexus""#);
        let rendered = root_toml(&v).expect("root is a table");
        assert_eq!(
            toml::to_string(&rendered).expect("serialises"),
            "default-server = \"nexus\"\n"
        );
    }

    #[test]
    fn nested_tables_survive_a_round_trip() {
        let src = "default-server = \"nexus\"\n\n[servers.nexus]\nendpoint = \"http://rio:8080/nexus\"\ntoken = \"t\"\n";
        let v = parse_document(src);
        let back = root_toml(&v).expect("root is a table");
        let out = toml::to_string(&back).expect("serialises");
        let reparsed: toml::Table = out.parse().expect("output re-parses");
        let original: toml::Table = src.parse().expect("input parses");
        assert_eq!(
            reparsed, original,
            "round-trip changed the document (tokens live in these tables)"
        );
    }

    /// A genuinely malformed document must still be an error, so the fix did
    /// not simply make every input succeed.
    #[test]
    fn malformed_toml_is_still_rejected() {
        let bad: Result<toml::Table, _> = "a = = 1".parse();
        assert!(bad.is_err(), "malformed TOML must not parse");
    }
}

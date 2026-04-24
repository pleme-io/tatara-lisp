//! Argument parsing — small, script-focused.
//!
//! Typical script startup:
//!
//!   (define spec
//!     (list (list :name "template" :short "t" :kind :string :required #t)
//!           (list :name "verbose"  :short "v" :kind :flag)))
//!
//!   (define args (parse-args spec))
//!   (define tpl  (alist-get args "template"))
//!   (define verbose? (alist-get args "verbose"))
//!   (define positional (alist-get args ""))   ; leftover positional args
//!
//! Each spec entry is a plist / alist with:
//!   :name       — long flag name (required)
//!   :short      — optional single-char alias
//!   :kind       — :string | :flag | :int
//!   :required   — bool; default #f
//!   :default    — default value when not provided
//!   :help       — one-line description for (print-usage spec)
//!
//! `parse-args` returns an alist mapping name → value; "" (empty string)
//! collects leftover positional args as a list.
//!
//! `(print-usage spec progname)` prints a usage banner to stderr.

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "parse-args",
        Arity::Exact(1),
        |args: &[Value], ctx: &mut ScriptCtx, sp| {
            let spec = parse_spec(&args[0], sp)?;
            let argv = ctx.argv.clone();
            let result = do_parse(&spec, &argv).map_err(|e| {
                EvalError::native_fn("parse-args", e, sp)
            })?;
            Ok(result)
        },
    );

    interp.register_fn(
        "print-usage",
        Arity::Range(1, 2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let spec = parse_spec(&args[0], sp)?;
            let prog = args
                .get(1)
                .map(|v| str_arg(v, "print-usage", sp))
                .transpose()?
                .unwrap_or_else(|| Arc::from("tatara-script"));
            eprintln!("usage: {prog} [flags]");
            for opt in &spec {
                let short = opt
                    .short
                    .as_ref()
                    .map(|c| format!("-{c}, "))
                    .unwrap_or_default();
                let kind_hint = match opt.kind {
                    Kind::Flag => "".to_string(),
                    Kind::String => " <str>".to_string(),
                    Kind::Int => " <int>".to_string(),
                };
                let req = if opt.required { " (required)" } else { "" };
                let help = opt.help.as_deref().unwrap_or("");
                eprintln!(
                    "  {}--{}{}{}   {}",
                    short, opt.name, kind_hint, req, help
                );
            }
            Ok(Value::Nil)
        },
    );
}

enum Kind {
    Flag,
    String,
    Int,
}

struct Spec {
    name: String,
    short: Option<String>,
    kind: Kind,
    required: bool,
    default: Option<Value>,
    help: Option<String>,
}

fn parse_spec(v: &Value, sp: tatara_lisp::Span) -> Result<Vec<Spec>, EvalError> {
    let Value::List(items) = v else {
        return Err(EvalError::native_fn(
            "parse-args",
            "spec must be a list",
            sp,
        ));
    };
    items
        .iter()
        .map(|entry| parse_spec_entry(entry, sp))
        .collect()
}

fn parse_spec_entry(v: &Value, sp: tatara_lisp::Span) -> Result<Spec, EvalError> {
    let Value::List(pairs) = v else {
        return Err(EvalError::native_fn(
            "parse-args",
            "spec entry must be a plist like (:name \"template\" :kind :string)",
            sp,
        ));
    };
    let mut spec = Spec {
        name: String::new(),
        short: None,
        kind: Kind::Flag,
        required: false,
        default: None,
        help: None,
    };
    let mut i = 0;
    while i + 1 < pairs.len() {
        let Value::Keyword(k) = &pairs[i] else {
            return Err(EvalError::native_fn(
                "parse-args",
                "spec entry keys must be keywords (:name etc)",
                sp,
            ));
        };
        let key: &str = k.as_ref();
        let val = &pairs[i + 1];
        match key {
            "name" => match val {
                Value::Str(s) => spec.name = s.as_ref().to_owned(),
                _ => return Err(EvalError::native_fn("parse-args", ":name must be string", sp)),
            },
            "short" => match val {
                Value::Str(s) => spec.short = Some(s.as_ref().to_owned()),
                _ => return Err(EvalError::native_fn("parse-args", ":short must be string", sp)),
            },
            "kind" => match val {
                Value::Keyword(k) => {
                    spec.kind = match k.as_ref() {
                        "flag" => Kind::Flag,
                        "string" => Kind::String,
                        "int" => Kind::Int,
                        other => {
                            return Err(EvalError::native_fn(
                                "parse-args",
                                format!("unknown :kind {other}"),
                                sp,
                            ))
                        }
                    }
                }
                _ => return Err(EvalError::native_fn("parse-args", ":kind must be keyword", sp)),
            },
            "required" => {
                spec.required = matches!(val, Value::Bool(true));
            }
            "default" => {
                spec.default = Some(val.clone());
            }
            "help" => match val {
                Value::Str(s) => spec.help = Some(s.as_ref().to_owned()),
                _ => {}
            },
            other => {
                return Err(EvalError::native_fn(
                    "parse-args",
                    format!("unknown spec key :{other}"),
                    sp,
                ))
            }
        }
        i += 2;
    }
    if spec.name.is_empty() {
        return Err(EvalError::native_fn("parse-args", ":name required", sp));
    }
    Ok(spec)
}

fn do_parse(spec: &[Spec], argv: &[String]) -> Result<Value, String> {
    use std::collections::HashMap;
    let mut matched: HashMap<String, Value> = HashMap::new();
    let mut positional: Vec<Value> = Vec::new();

    // Build lookup tables
    let by_long: HashMap<&str, &Spec> =
        spec.iter().map(|s| (s.name.as_str(), s)).collect();
    let by_short: HashMap<&str, &Spec> = spec
        .iter()
        .filter_map(|s| s.short.as_deref().map(|k| (k, s)))
        .collect();

    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let entry = by_long
                .get(name)
                .ok_or_else(|| format!("unknown flag --{name}"))?;
            consume_flag(entry, &mut it, &mut matched)?;
        } else if let Some(name) = arg.strip_prefix('-') {
            if name.is_empty() {
                positional.push(Value::Str(Arc::from(arg.as_str())));
                continue;
            }
            let entry = by_short
                .get(name)
                .ok_or_else(|| format!("unknown flag -{name}"))?;
            consume_flag(entry, &mut it, &mut matched)?;
        } else {
            positional.push(Value::Str(Arc::from(arg.as_str())));
        }
    }

    // Apply defaults + verify required
    for entry in spec {
        if matched.contains_key(&entry.name) {
            continue;
        }
        if let Some(d) = &entry.default {
            matched.insert(entry.name.clone(), d.clone());
        } else if entry.required {
            return Err(format!("missing required flag --{}", entry.name));
        } else if matches!(entry.kind, Kind::Flag) {
            matched.insert(entry.name.clone(), Value::Bool(false));
        }
    }

    // Format as alist — 2-lists with string keys
    let mut out: Vec<Value> = matched
        .into_iter()
        .map(|(k, v)| Value::list(vec![Value::Str(Arc::from(k.as_str())), v]))
        .collect();
    // Always include the "" key for positional args (may be empty list)
    out.push(Value::list(vec![
        Value::Str(Arc::from("")),
        Value::list(positional),
    ]));
    Ok(Value::list(out))
}

fn consume_flag<'a>(
    entry: &Spec,
    it: &mut std::slice::Iter<'a, String>,
    matched: &mut std::collections::HashMap<String, Value>,
) -> Result<(), String> {
    let v = match entry.kind {
        Kind::Flag => Value::Bool(true),
        Kind::String => {
            let raw = it
                .next()
                .ok_or_else(|| format!("--{} expects a value", entry.name))?;
            Value::Str(Arc::from(raw.as_str()))
        }
        Kind::Int => {
            let raw = it
                .next()
                .ok_or_else(|| format!("--{} expects an integer", entry.name))?;
            let n: i64 = raw
                .parse()
                .map_err(|e| format!("--{} bad int: {e}", entry.name))?;
            Value::Int(n)
        }
    };
    matched.insert(entry.name.clone(), v);
    Ok(())
}

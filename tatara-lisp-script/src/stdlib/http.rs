//! HTTP client.
//!
//!   (http-get URL)                        → body string
//!   (http-get URL HEADERS)                → body string
//!   (http-get-json URL HEADERS)           → parsed JSON (nested Value tree)
//!   (http-post-json URL HEADERS BODY)     → parsed JSON response
//!
//! HEADERS is a list of (KEY . VALUE) cons cells or `(KEY VALUE)` lists.
//! Tatara-lisp's `(cons)` produces a 2-element list, so both shapes work.
//! A typical Cloudflare call:
//!
//!   (http-get-json
//!     "https://api.cloudflare.com/client/v4/zones?name=lilitu.io"
//!     (list (list "Authorization" (string-append "Bearer " token))
//!           (list "Content-Type" "application/json")))

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::str_arg;
use crate::stdlib::json::json_to_value;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "http-get",
        Arity::Range(1, 2),
        |args: &[Value], ctx: &mut ScriptCtx, sp| {
            let url = str_arg(&args[0], "http-get", sp)?;
            let headers = headers_from_value(args.get(1), sp)?;
            let body = do_get(ctx, &url, &headers, sp)?;
            Ok(Value::Str(Arc::from(body)))
        },
    );

    interp.register_fn(
        "http-get-json",
        Arity::Range(1, 2),
        |args: &[Value], ctx: &mut ScriptCtx, sp| {
            let url = str_arg(&args[0], "http-get-json", sp)?;
            let headers = headers_from_value(args.get(1), sp)?;
            let body = do_get(ctx, &url, &headers, sp)?;
            let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
                EvalError::native_fn(
                    "http-get-json",
                    format!("response not JSON: {e} (body={body:.200})"),
                    sp,
                )
            })?;
            Ok(json_to_value(&parsed))
        },
    );

    interp.register_fn(
        "http-post-json",
        Arity::Exact(3),
        |args: &[Value], ctx: &mut ScriptCtx, sp| {
            let url = str_arg(&args[0], "http-post-json", sp)?;
            let headers = headers_from_value(Some(&args[1]), sp)?;
            let body_json = crate::stdlib::json::value_to_json(&args[2]);
            let agent = ctx.http().clone();
            let mut req = agent.post(&*url);
            for (k, v) in &headers {
                req = req.header(&**k, &**v);
            }
            let resp_body = req
                .send(
                    serde_json::to_string(&body_json)
                        .map_err(|e| EvalError::native_fn("http-post-json", e.to_string(), sp))?,
                )
                .map_err(|e| EvalError::native_fn("http-post-json", e.to_string(), sp))?
                .into_body()
                .read_to_string()
                .map_err(|e| EvalError::native_fn("http-post-json", e.to_string(), sp))?;
            let parsed: serde_json::Value =
                serde_json::from_str(&resp_body).map_err(|e| {
                    EvalError::native_fn(
                        "http-post-json",
                        format!("response not JSON: {e}"),
                        sp,
                    )
                })?;
            Ok(json_to_value(&parsed))
        },
    );
}

fn do_get(
    ctx: &mut ScriptCtx,
    url: &str,
    headers: &[(Arc<str>, Arc<str>)],
    sp: tatara_lisp::Span,
) -> Result<String, EvalError> {
    let agent = ctx.http().clone();
    let mut req = agent.get(url);
    for (k, v) in headers {
        req = req.header(&**k, &**v);
    }
    let resp = req
        .call()
        .map_err(|e| EvalError::native_fn("http-get", format!("{url}: {e}"), sp))?;
    resp.into_body()
        .read_to_string()
        .map_err(|e| EvalError::native_fn("http-get", format!("{url}: {e}"), sp))
}

/// Accept headers as a list of 2-element lists: `((k1 v1) (k2 v2) ...)`.
/// Empty / nil means no headers.
fn headers_from_value(
    v: Option<&Value>,
    sp: tatara_lisp::Span,
) -> Result<Vec<(Arc<str>, Arc<str>)>, EvalError> {
    let Some(v) = v else { return Ok(vec![]) };
    match v {
        Value::Nil => Ok(vec![]),
        Value::List(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for entry in entries.iter() {
                let Value::List(pair) = entry else {
                    return Err(EvalError::native_fn(
                        "http",
                        "header entry must be a (KEY VALUE) list",
                        sp,
                    ));
                };
                if pair.len() != 2 {
                    return Err(EvalError::native_fn(
                        "http",
                        format!("header entry must be (KEY VALUE), got {} elements", pair.len()),
                        sp,
                    ));
                }
                let k = match &pair[0] {
                    Value::Str(s) => s.clone(),
                    Value::Symbol(s) | Value::Keyword(s) => s.clone(),
                    other => {
                        return Err(EvalError::native_fn(
                            "http",
                            format!("header key must be string/symbol/keyword, got {}", other.type_name()),
                            sp,
                        ))
                    }
                };
                let v = match &pair[1] {
                    Value::Str(s) => s.clone(),
                    other => {
                        return Err(EvalError::native_fn(
                            "http",
                            format!("header value must be string, got {}", other.type_name()),
                            sp,
                        ))
                    }
                };
                out.push((k, v));
            }
            Ok(out)
        }
        other => Err(EvalError::native_fn(
            "http",
            format!("headers must be a list, got {}", other.type_name()),
            sp,
        )),
    }
}

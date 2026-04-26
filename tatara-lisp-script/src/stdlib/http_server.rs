//! HTTP server.
//!
//! Lets a `.tlisp` script spin up an HTTP listener with a static
//! routing table. Suitable for hello-world demos, smoke-test
//! services, and the most basic "deploy a tatara-lisp program live"
//! shape.
//!
//! ```text
//! ;; Static routes — fixed responses per path.
//! (http-serve-static
//!   8080
//!   '(("/healthz"   200 "{\"status\":\"ok\"}")
//!     ("/"          200 "{\"message\":\"Hello, world!\"}")
//!     ("/hello"     200 "{\"message\":\"Hello, world!\"}")))
//! ```
//!
//! Blocks the calling thread until SIGTERM/SIGINT.
//!
//! Production HTTP services (with dynamic handlers, path params,
//! middleware) ship through wasm-operator per
//! [theory/WASM-STACK.md §IV](../../../../theory/WASM-STACK.md). This
//! stdlib primitive is for the smallest demo case where a script
//! "serves something" without coordinating a cluster.

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "http-serve-static",
        Arity::Exact(2),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let port: u16 = match &args[0] {
                Value::Int(n) => u16::try_from(*n).map_err(|_| {
                    EvalError::native_fn("http-serve-static", format!("http-serve-static: port {n} out of range"), sp)
                })?,
                v => {
                    return Err(EvalError::native_fn("http-serve-static", 
                        format!("http-serve-static: port must be int, got {v:?}"),
                        sp,
                    ))
                }
            };

            let routes = parse_routes(&args[1], sp)?;
            let addr = format!("0.0.0.0:{port}");
            let server = tiny_http::Server::http(&addr).map_err(|e| {
                EvalError::native_fn("http-serve-static", format!("http-serve-static bind {addr} failed: {e}"), sp)
            })?;

            eprintln!("[http-serve-static] listening on http://{addr}");
            for path_route in &routes {
                eprintln!("[http-serve-static]   {} → {}", path_route.path, path_route.status);
            }

            for request in server.incoming_requests() {
                let path = request.url();
                let path_only = path.split('?').next().unwrap_or(path).to_string();

                let chosen: Option<&Route> = routes.iter().find(|r| r.path == path_only);

                let response = match chosen {
                    Some(r) => {
                        eprintln!("[http-serve-static] {} {} → {}", request.method(), path, r.status);
                        tiny_http::Response::from_string(r.body.clone())
                            .with_status_code(r.status)
                            .with_header(json_header())
                    }
                    None => {
                        eprintln!("[http-serve-static] {} {} → 404", request.method(), path);
                        tiny_http::Response::from_string(
                            r#"{"error":"not_found"}"#.to_string(),
                        )
                        .with_status_code(404)
                        .with_header(json_header())
                    }
                };
                let _ = request.respond(response);
            }
            Ok(Value::Nil)
        },
    );
}

fn json_header() -> tiny_http::Header {
    tiny_http::Header::from_bytes(
        b"Content-Type".as_ref(),
        b"application/json".as_ref(),
    )
    .expect("static header")
}

#[derive(Debug)]
struct Route {
    path: String,
    status: i32,
    body: String,
}

fn parse_routes(value: &Value, sp: tatara_lisp::Span) -> Result<Vec<Route>, EvalError> {
    let outer = match value {
        Value::List(l) => l.as_ref(),
        v => {
            return Err(EvalError::native_fn("http-serve-static", 
                format!("http-serve-static: routes must be a list, got {v:?}"),
                sp,
            ))
        }
    };

    let mut routes = Vec::with_capacity(outer.len());
    for entry in outer {
        let triple = match entry {
            Value::List(l) => l.as_ref(),
            v => {
                return Err(EvalError::native_fn("http-serve-static", 
                    format!("http-serve-static: each route must be a 3-list, got {v:?}"),
                    sp,
                ))
            }
        };
        if triple.len() != 3 {
            return Err(EvalError::native_fn("http-serve-static", 
                format!(
                    "http-serve-static: route needs (path status body), got {} elements",
                    triple.len()
                ),
                sp,
            ));
        }

        let path = match &triple[0] {
            Value::Str(s) => s.to_string(),
            v => {
                return Err(EvalError::native_fn("http-serve-static", 
                    format!("http-serve-static: path must be string, got {v:?}"),
                    sp,
                ))
            }
        };
        let status: i32 = match &triple[1] {
            Value::Int(n) => i32::try_from(*n).map_err(|_| {
                EvalError::native_fn("http-serve-static", 
                    format!("http-serve-static: status {n} out of range"),
                    sp,
                )
            })?,
            v => {
                return Err(EvalError::native_fn("http-serve-static", 
                    format!("http-serve-static: status must be int, got {v:?}"),
                    sp,
                ))
            }
        };
        let body = match &triple[2] {
            Value::Str(s) => s.to_string(),
            v => {
                return Err(EvalError::native_fn("http-serve-static", 
                    format!("http-serve-static: body must be string, got {v:?}"),
                    sp,
                ))
            }
        };
        routes.push(Route { path, status, body });
    }
    Ok(routes)
}

// Silence the unused-Arc-import lint for a clean module.
#[allow(dead_code)]
fn _unused() -> Arc<()> {
    Arc::new(())
}

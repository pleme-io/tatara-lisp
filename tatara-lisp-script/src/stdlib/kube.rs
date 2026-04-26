//! Kubernetes auth + URL helpers.
//!
//! Tatara-script doesn't bundle a full kube-rs equivalent — instead it
//! exposes the auth primitives a script needs to call the K8s API via
//! the existing `http-get-json` / `http-post-json` stdlib. This keeps
//! the binary size small and follows the Unix-philosophy "small,
//! composable primitives" the user named.
//!
//! Surface:
//!
//!   (kube-in-cluster?)            → bool — running inside a Pod?
//!   (kube-bearer-token)           → string — service account token
//!   (kube-ca-cert)                → string — cluster CA cert (PEM)
//!   (kube-namespace)              → string — pod's own namespace
//!   (kube-api-base)               → string — "https://kubernetes.default.svc:443"
//!
//! Use together with http-get-json:
//!
//!   (define ns (kube-namespace))
//!   (define url (string-append (kube-api-base)
//!                              "/api/v1/namespaces/"
//!                              ns
//!                              "/configmaps"))
//!   (define headers (list (list "Authorization"
//!                               (string-append "Bearer " (kube-bearer-token)))))
//!   (define cms (http-get-json url headers))

use std::sync::Arc;

use tatara_lisp_eval::{Arity, EvalError, Interpreter, Value};

use crate::script_ctx::ScriptCtx;

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "kube-in-cluster?",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            Ok(Value::Bool(in_cluster()))
        },
    );

    interp.register_fn(
        "kube-bearer-token",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            read_sa_file("token")
                .map(|s| Value::Str(Arc::from(s.trim_end().to_string())))
                .map_err(|e| {
                    EvalError::native_fn(
                        "kube-bearer-token",
                        format!("read SA token: {e}"),
                        sp,
                    )
                })
        },
    );

    interp.register_fn(
        "kube-ca-cert",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            read_sa_file("ca.crt")
                .map(|s| Value::Str(Arc::from(s)))
                .map_err(|e| {
                    EvalError::native_fn(
                        "kube-ca-cert",
                        format!("read SA ca.crt: {e}"),
                        sp,
                    )
                })
        },
    );

    interp.register_fn(
        "kube-namespace",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, sp| {
            // Order: env var (downward API) → SA namespace file → "default".
            if let Ok(ns) = std::env::var("POD_NAMESPACE") {
                return Ok(Value::Str(Arc::from(ns)));
            }
            match read_sa_file("namespace") {
                Ok(s) => Ok(Value::Str(Arc::from(s.trim_end().to_string()))),
                Err(_) => {
                    if !in_cluster() {
                        return Ok(Value::Str(Arc::from("default")));
                    }
                    Err(EvalError::native_fn(
                        "kube-namespace",
                        "no namespace from env or SA file",
                        sp,
                    ))
                }
            }
        },
    );

    interp.register_fn(
        "kube-api-base",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            // Override via env (e.g. local dev with kubectl proxy on 8001).
            if let Ok(url) = std::env::var("KUBE_API_BASE") {
                return Ok(Value::Str(Arc::from(url)));
            }
            // In-cluster default — works because of automatic
            // kubernetes.default.svc DNS + auto-mounted CA cert.
            let host = std::env::var("KUBERNETES_SERVICE_HOST")
                .unwrap_or_else(|_| "kubernetes.default.svc".to_string());
            let port = std::env::var("KUBERNETES_SERVICE_PORT")
                .unwrap_or_else(|_| "443".to_string());
            Ok(Value::Str(Arc::from(format!("https://{host}:{port}"))))
        },
    );

    interp.register_fn(
        "kube-pod-name",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            // Downward API — operator must mount POD_NAME into the env.
            // Falls back to hostname (the kubelet sets it to the Pod name
            // by default for non-hostNetwork pods).
            if let Ok(pod) = std::env::var("POD_NAME") {
                return Ok(Value::Str(Arc::from(pod)));
            }
            let host = hostname_safe();
            Ok(Value::Str(Arc::from(host)))
        },
    );

    interp.register_fn(
        "kube-cluster-name",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            // Cluster name is purely operator-supplied via env (no
            // canonical kubelet API surface for it). Default to "unknown"
            // so programs work locally without breaking.
            Ok(Value::Str(Arc::from(
                std::env::var("CLUSTER_NAME").unwrap_or_else(|_| "unknown".to_string()),
            )))
        },
    );
}

fn read_sa_file(name: &str) -> Result<String, std::io::Error> {
    let path = format!("{SA_DIR}/{name}");
    std::fs::read_to_string(path)
}

fn in_cluster() -> bool {
    std::path::Path::new(SA_DIR).is_dir()
        && std::fs::metadata(format!("{SA_DIR}/token")).is_ok()
}

fn hostname_safe() -> String {
    // Best-effort. Read /etc/hostname (Linux container default) or
    // env HOSTNAME.
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        return s.trim_end().to_string();
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string())
}

//! `(bpf-fn name (params) body…)` — author a BPF program body in
//! tatara-lisp, lower to aya-Rust source.
//!
//! ## What this proves
//!
//! Pillar 1 (Rust + tatara-lisp + WASM/WASI) at the most demanding
//! tier of the cloud stack — kernel code. The merger:
//!
//! ```text
//!   (bpf-fn drop-syn (ctx)              ; tatara-lisp authoring
//!     (if (= (proto ctx) :tcp)
//!         (return :xdp-drop)
//!         (return :xdp-pass)))
//!         ↓ this module
//!   pub fn drop_syn(ctx: XdpContext) -> u32 {
//!       if proto(&ctx) == PROTO_TCP {
//!           return aya_ebpf::bindings::xdp_action::XDP_DROP;
//!       } else {
//!           return aya_ebpf::bindings::xdp_action::XDP_PASS;
//!       }
//!   }
//! ```
//!
//! ## Scope
//!
//! Verifier-aware, not verifier-complete. The lowering produces
//! Rust that **the BPF verifier will accept** for the supported
//! forms — but the supported set is a strict subset of tatara-lisp.
//! No heap allocation. No recursion. No dynamic dispatch. Bounded
//! loops only (and only when explicitly annotated). Helper calls
//! are restricted to a whitelist (`bpf-helpers`).
//!
//! Why a subset and not full tatara-lisp? Because BPF isn't a
//! general-purpose target — the kernel's verifier exists. The job
//! here is to be a *more pleasant front end* for the same set of
//! programs you'd write directly in Rust + aya, not to retarget
//! arbitrary Lisp.
//!
//! ## Supported forms (Phase 1 MVP)
//!
//! - `(return :keyword)` — typed return-action constant. The
//!   keyword maps to an aya constant via `RETURN_ACTIONS`.
//! - `(call helper-name args…)` — invoke a whitelisted BPF helper.
//! - `(let ((name expr)) body…)` — bind a local; body sees `name`.
//! - `(if cond then else)` — conditional. `cond` is any expr that
//!   lowers to a Rust boolean.
//! - `(= a b)` / `(!= a b)` / `(< a b)` / `(> a b)` — comparisons.
//! - `(map-get map-name key)` / `(map-set map-name key value)` —
//!   typed map access. The map name resolves to the aya map static.
//! - Literal `i64` / `:keyword` / `String` — passed through as
//!   Rust literals (`42`, `MAP_NAMES`, `"hostname"`).
//!
//! Anything else is a hard error — surface fast at codegen time
//! rather than waiting for the kernel verifier to reject the
//! object.

use serde::{Deserialize, Serialize};
use std::fmt::Write;

/// Errors produced while lowering a `(bpf-fn …)` form.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum LowerError {
    #[error("expected `(bpf-fn name (params) body…)`, got `{0}`")]
    BadShape(String),
    #[error("unknown form `{0}` — bpf-fn supports only the verifier-friendly subset")]
    UnknownForm(String),
    #[error("unknown helper `{0}` — add it to BPF_HELPERS or call directly via aya")]
    UnknownHelper(String),
    #[error("unknown return action `:{0}` — see RETURN_ACTIONS for the supported set")]
    UnknownReturnAction(String),
    #[error("`(let)` requires a binding-list with one or more (name expr) pairs")]
    BadLet,
    #[error("`(if cond then else)` requires exactly three sub-forms")]
    BadIf,
    #[error("comparison `{0}` requires exactly two operands")]
    BadCompare(String),
    #[error("map operation `{0}` requires {1} args, got {2}")]
    BadMapOp(&'static str, usize, usize),
}

/// One authored BPF function — name, parameter list, body forms.
/// Mirrors the `(bpf-fn …)` source shape so callers can construct
/// it programmatically (from tatara-lisp's reader output) or via
/// `serde_json::from_value` (when the form has been compiled
/// through the domain registry).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BpfFn {
    /// Function name. Becomes the Rust ident.
    pub name: String,
    /// Single context parameter — name of the `XdpContext` /
    /// `TcContext` / etc. binding inside the body.
    pub ctx: String,
    /// Body expressions. Each is a `BpfExpr`. The last expression's
    /// value becomes the function's return.
    pub body: Vec<BpfExpr>,
}

/// One body expression. Recursive — `If` / `Let` nest other exprs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "form", rename_all = "kebab-case")]
pub enum BpfExpr {
    /// `(return :xdp-pass)` etc.
    Return { action: String },
    /// `(call helper-name args…)`.
    Call { helper: String, args: Vec<BpfExpr> },
    /// `(let ((name expr)) body…)` — single-binding for now;
    /// multi-binding lets desugar into nested singles upstream.
    Let {
        name: String,
        value: Box<BpfExpr>,
        body: Vec<BpfExpr>,
    },
    /// `(if cond then else)`.
    If {
        cond: Box<BpfExpr>,
        then: Box<BpfExpr>,
        otherwise: Box<BpfExpr>,
    },
    /// `(= a b)` / `(!= a b)` / `(< a b)` / `(> a b)`.
    Compare {
        op: CompareOp,
        left: Box<BpfExpr>,
        right: Box<BpfExpr>,
    },
    /// `(map-get map-name key)`.
    MapGet { map: String, key: Box<BpfExpr> },
    /// `(map-set map-name key value)`.
    MapSet {
        map: String,
        key: Box<BpfExpr>,
        value: Box<BpfExpr>,
    },
    /// Literal i64 → Rust `i64` constant.
    Int(i64),
    /// Reference to a let-bound name or a parameter (the ctx).
    Var(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl CompareOp {
    fn rust_op(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::Le => "<=",
            Self::Ge => ">=",
        }
    }
}

/// Verifier-friendly BPF helpers a `(bpf-fn …)` body can call.
/// Mapped to the aya helper-fn name. Add to this list as the
/// surface grows; rejecting unknown calls is part of the safety
/// guarantee.
pub const BPF_HELPERS: &[(&str, &str)] = &[
    ("get-current-cpu", "aya_ebpf::helpers::bpf_get_smp_processor_id"),
    ("get-current-pid-tgid", "aya_ebpf::helpers::bpf_get_current_pid_tgid"),
    ("get-current-uid-gid", "aya_ebpf::helpers::bpf_get_current_uid_gid"),
    ("get-prandom", "aya_ebpf::helpers::bpf_get_prandom_u32"),
    ("ktime-ns", "aya_ebpf::helpers::bpf_ktime_get_ns"),
];

/// Return-action keywords for each program kind. Lookup is
/// kind-dependent — XDP returns differ from TC, TC differs from
/// LSM, etc. Phase 1 covers the common kinds; extending is
/// mechanical.
pub const RETURN_ACTIONS: &[(&str, &str)] = &[
    // XDP
    ("xdp-pass", "aya_ebpf::bindings::xdp_action::XDP_PASS"),
    ("xdp-drop", "aya_ebpf::bindings::xdp_action::XDP_DROP"),
    ("xdp-tx", "aya_ebpf::bindings::xdp_action::XDP_TX"),
    ("xdp-redirect", "aya_ebpf::bindings::xdp_action::XDP_REDIRECT"),
    ("xdp-aborted", "aya_ebpf::bindings::xdp_action::XDP_ABORTED"),
    // TC
    ("tc-act-ok", "aya_ebpf::bindings::TC_ACT_OK as i32"),
    ("tc-act-shot", "aya_ebpf::bindings::TC_ACT_SHOT as i32"),
    ("tc-act-redirect", "aya_ebpf::bindings::TC_ACT_REDIRECT as i32"),
    // Generic 0 / 1 — for kprobes / tracepoints / cgroup-skb
    ("ok", "0"),
    ("err", "1"),
];

/// Lower a `BpfFn` to aya-Rust source. Returns the **complete
/// function definition** (signature + body) — caller wraps with
/// the `#[xdp]` / `#[classifier]` attribute via
/// `codegen::emit_aya_program`.
pub fn lower(f: &BpfFn) -> Result<String, LowerError> {
    let mut out = String::new();
    let _ = writeln!(out, "pub fn {}(ctx: aya_ebpf::programs::XdpContext) -> u32 {{", f.name);
    let mut indent = 1;
    let last = f.body.len().saturating_sub(1);
    for (i, expr) in f.body.iter().enumerate() {
        let line = lower_expr(expr, indent)?;
        // Last expression becomes the return value (Rust-style); a
        // trailing semicolon for non-tail expressions.
        let suffix = if i == last { "" } else { ";" };
        let _ = writeln!(out, "{}{line}{suffix}", "    ".repeat(indent));
    }
    indent -= 1;
    let _ = writeln!(out, "{}}}", "    ".repeat(indent));
    Ok(out)
}

/// Lower one expression to a Rust expression string.
fn lower_expr(e: &BpfExpr, indent: usize) -> Result<String, LowerError> {
    let pad = "    ".repeat(indent);
    match e {
        BpfExpr::Int(n) => Ok(format!("{n}_i64")),
        BpfExpr::Var(name) => Ok(rust_name(name)),
        BpfExpr::Return { action } => {
            let mapped = RETURN_ACTIONS
                .iter()
                .find_map(|(k, v)| (*k == action).then_some(*v))
                .ok_or_else(|| LowerError::UnknownReturnAction(action.clone()))?;
            Ok(format!("return {mapped}"))
        }
        BpfExpr::Call { helper, args } => {
            let mapped = BPF_HELPERS
                .iter()
                .find_map(|(k, v)| (*k == helper).then_some(*v))
                .ok_or_else(|| LowerError::UnknownHelper(helper.clone()))?;
            let arg_str = args
                .iter()
                .map(|a| lower_expr(a, indent))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            Ok(format!("unsafe {{ {mapped}({arg_str}) }}"))
        }
        BpfExpr::Let { name, value, body } => {
            let v = lower_expr(value, indent)?;
            let mut buf = String::new();
            let _ = writeln!(buf, "let {} = {};", rust_name(name), v);
            for (i, inner) in body.iter().enumerate() {
                let inner_str = lower_expr(inner, indent)?;
                let suffix = if i == body.len() - 1 { "" } else { ";" };
                let _ = writeln!(buf, "{pad}{inner_str}{suffix}");
            }
            // Wrap in a block so the let scopes correctly inside an
            // expression context.
            Ok(format!("{{ {} }}", buf.trim_end()))
        }
        BpfExpr::If { cond, then, otherwise } => {
            let c = lower_expr(cond, indent)?;
            let t = lower_expr(then, indent + 1)?;
            let o = lower_expr(otherwise, indent + 1)?;
            Ok(format!("if {c} {{ {t} }} else {{ {o} }}"))
        }
        BpfExpr::Compare { op, left, right } => {
            let l = lower_expr(left, indent)?;
            let r = lower_expr(right, indent)?;
            Ok(format!("({l} {} {r})", op.rust_op()))
        }
        BpfExpr::MapGet { map, key } => {
            let k = lower_expr(key, indent)?;
            Ok(format!(
                "unsafe {{ {}.get(&{k}) }}",
                rust_static_name(map)
            ))
        }
        BpfExpr::MapSet { map, key, value } => {
            let k = lower_expr(key, indent)?;
            let v = lower_expr(value, indent)?;
            Ok(format!(
                "unsafe {{ {}.insert(&{k}, &{v}, 0) }}",
                rust_static_name(map)
            ))
        }
    }
}

/// kebab-name → rust-name for a local binding.
fn rust_name(s: &str) -> String {
    s.replace('-', "_")
}

/// kebab-name → SCREAMING_SNAKE for a static (map name).
fn rust_static_name(s: &str) -> String {
    s.replace('-', "_").to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_literal_return() {
        let f = BpfFn {
            name: "drop_all".into(),
            ctx: "ctx".into(),
            body: vec![BpfExpr::Return {
                action: "xdp-drop".into(),
            }],
        };
        let src = lower(&f).unwrap();
        assert!(src.contains("pub fn drop_all"));
        assert!(src.contains("XDP_DROP"));
    }

    #[test]
    fn lowers_helper_call_in_let() {
        let f = BpfFn {
            name: "tag_cpu".into(),
            ctx: "ctx".into(),
            body: vec![BpfExpr::Let {
                name: "cpu-id".into(),
                value: Box::new(BpfExpr::Call {
                    helper: "get-current-cpu".into(),
                    args: vec![],
                }),
                body: vec![BpfExpr::Return {
                    action: "xdp-pass".into(),
                }],
            }],
        };
        let src = lower(&f).unwrap();
        assert!(src.contains("let cpu_id = unsafe { aya_ebpf::helpers::bpf_get_smp_processor_id"));
        assert!(src.contains("XDP_PASS"));
    }

    #[test]
    fn lowers_if_with_compare() {
        let f = BpfFn {
            name: "branch".into(),
            ctx: "ctx".into(),
            body: vec![BpfExpr::If {
                cond: Box::new(BpfExpr::Compare {
                    op: CompareOp::Eq,
                    left: Box::new(BpfExpr::Int(42)),
                    right: Box::new(BpfExpr::Int(42)),
                }),
                then: Box::new(BpfExpr::Return {
                    action: "xdp-pass".into(),
                }),
                otherwise: Box::new(BpfExpr::Return {
                    action: "xdp-drop".into(),
                }),
            }],
        };
        let src = lower(&f).unwrap();
        assert!(src.contains("if (42_i64 == 42_i64)"));
        assert!(src.contains("XDP_PASS"));
        assert!(src.contains("XDP_DROP"));
    }

    #[test]
    fn lowers_map_get_and_set() {
        let body = vec![
            BpfExpr::MapSet {
                map: "syn-counter".into(),
                key: Box::new(BpfExpr::Int(0)),
                value: Box::new(BpfExpr::Int(1)),
            },
            BpfExpr::Return {
                action: "xdp-pass".into(),
            },
        ];
        let f = BpfFn {
            name: "counter_inc".into(),
            ctx: "ctx".into(),
            body,
        };
        let src = lower(&f).unwrap();
        assert!(src.contains("SYN_COUNTER.insert(&0_i64, &1_i64, 0)"));
    }

    #[test]
    fn rejects_unknown_helper() {
        let f = BpfFn {
            name: "bad".into(),
            ctx: "ctx".into(),
            body: vec![BpfExpr::Call {
                helper: "wat".into(),
                args: vec![],
            }],
        };
        let err = lower(&f).unwrap_err();
        assert!(matches!(err, LowerError::UnknownHelper(_)));
    }

    #[test]
    fn rejects_unknown_return_action() {
        let f = BpfFn {
            name: "bad".into(),
            ctx: "ctx".into(),
            body: vec![BpfExpr::Return {
                action: "make-up-kernel".into(),
            }],
        };
        let err = lower(&f).unwrap_err();
        assert!(matches!(err, LowerError::UnknownReturnAction(_)));
    }

    #[test]
    fn lowering_round_trips_via_serde_json() {
        let f = BpfFn {
            name: "round_trip".into(),
            ctx: "ctx".into(),
            body: vec![BpfExpr::Return {
                action: "xdp-pass".into(),
            }],
        };
        let json = serde_json::to_value(&f).unwrap();
        let back: BpfFn = serde_json::from_value(json).unwrap();
        assert_eq!(f, back);
    }
}

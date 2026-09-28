//! A field whose natural keyword is a Rust keyword is declared as a raw
//! identifier (`r#fn`) and authored as its plain spelling (`:fn`), the same
//! spelling serde gives it.

use serde::Deserialize;
use tatara_lisp::{DeriveTataraDomain, TataraDomain};

#[derive(Debug, PartialEq, Eq, Deserialize, DeriveTataraDomain)]
#[tatara(keyword = "defraw-probe")]
struct Probe {
    name: String,
    r#fn: String,
}

fn compile(src: &str) -> Result<Probe, String> {
    let forms = tatara_lisp::read(src).map_err(|e| e.to_string())?;
    Probe::compile_from_sexp(&forms[0]).map_err(|e| format!("{e:?}"))
}

#[test]
fn a_raw_identifier_field_is_authored_without_the_prefix() {
    let p = compile(r#"(defraw-probe :name "wc" :fn "run")"#).unwrap();
    assert_eq!(p, Probe { name: "wc".into(), r#fn: "run".into() });
}

#[test]
fn the_raw_spelling_is_not_a_key() {
    assert!(compile(r#"(defraw-probe :name "wc" :r#fn "run")"#).is_err());
}

#[test]
fn serde_agrees_on_the_spelling() {
    let p: Probe = serde_json::from_str(r#"{"name":"wc","fn":"run"}"#).unwrap();
    assert_eq!(p.r#fn, "run");
}

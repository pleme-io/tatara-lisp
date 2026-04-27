//! End-to-end proof — author a multi-domain env, compile to a
//! typed graph, validate cross-resource coherence.
//!
//! Pulls in two real catalog crates (`tatara-gateway-api`,
//! `tatara-ebpf`) plus `tatara-env` itself, registers each, then
//! exercises the full pipeline a synthesizer pass would walk.

use tatara_env::{compile_into_env, register, validate, ValidationError};
use tatara_lisp::read;

fn register_test_domains() {
    // Tatara-env's own keyword.
    register();
    // Two real catalog domains pulled in via dev-deps.
    tatara_gateway_api::register();
    tatara_ebpf::register();
}

const SAMPLE_PROGRAM: &str = r#"
;; A small but real production-like env. Imports two catalog
;; crates + the ebpf crate, declares one resource per imported
;; domain. The compile pass collects every (def<keyword> …) into
;; the typed Env; the validate pass checks cross-import coherence.

(defenv
  :name "production"
  :description "Edge-protected production cluster."
  :imports ("tatara-gateway-api" "tatara-ebpf")
  :labels (:tier "prod" :region "us-east-1"))

(defgateway
  :gateway-class-name "nginx"
  :listeners ())

(defbpf-map
  :name "syn_counter"
  :kind :per-cpu-array
  :key-size 4
  :value-size 8
  :max-entries 1)

(defbpf-program
  :name "drop_syn_flood"
  :kind :xdp
  :attach (:target "eth0")
  :source "bpf/drop_syn.rs"
  :license "GPL"
  :uses-maps ("syn_counter"))

(defbpf-policy
  :name "edge_protection"
  :description "L4 SYN-flood mitigation."
  :programs ("drop_syn_flood")
  :maps ("syn_counter"))
"#;

#[test]
fn compiles_a_multi_domain_program_into_one_env() {
    register_test_domains();
    let forms = read(SAMPLE_PROGRAM).expect("reader parses sample program");
    let env = compile_into_env(&forms).expect("compile succeeds");
    assert_eq!(env.spec.name, "production");
    assert_eq!(
        env.spec.imports,
        vec!["tatara-gateway-api", "tatara-ebpf"]
    );

    // Five typed resources collected: gateway, map, program, policy.
    // (4 distinct + the env metadata isn't a resource.)
    assert_eq!(
        env.resources.len(),
        4,
        "expected 4 typed resources, got {} ({:?})",
        env.resources.len(),
        env.keywords()
    );

    // Filter helpers work.
    let gateways = env.resources_by_keyword("defgateway");
    assert_eq!(gateways.len(), 1);
    let bpf_progs = env.resources_by_keyword("defbpf-program");
    assert_eq!(bpf_progs.len(), 1);

    // Keywords appear in declaration order (deterministic).
    assert_eq!(
        env.keywords(),
        vec!["defgateway", "defbpf-map", "defbpf-program", "defbpf-policy"]
    );
}

#[test]
fn validate_passes_on_well_formed_env() {
    register_test_domains();
    let forms = read(SAMPLE_PROGRAM).expect("read");
    let env = compile_into_env(&forms).expect("compile");
    validate(&env).expect("env validates cleanly");
}

#[test]
fn validate_flags_unregistered_keyword_when_import_is_missing() {
    register_test_domains();
    // Drop the `tatara-ebpf` import but keep the bpf resources —
    // the validator should flag every bpf keyword as having no
    // matching import.
    let mismatched = r#"
        (defenv :name "broken" :description "x" :imports ("tatara-gateway-api"))
        (defgateway :gateway-class-name "nginx" :listeners ())
        (defbpf-map :name "m" :kind :array :value-size 8 :max-entries 1)
    "#;
    let forms = read(mismatched).unwrap();
    let env = compile_into_env(&forms).unwrap();
    let errors = validate(&env).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ValidationError::UnregisteredKeyword { keyword, .. } if keyword == "defbpf-map")),
        "expected UnregisteredKeyword for defbpf-map, got {errors:?}"
    );
}

#[test]
fn validate_flags_unused_import() {
    register_test_domains();
    // Import `tatara-ebpf` but declare no bpf resources.
    let unused = r#"
        (defenv :name "unused-import" :description "x"
                :imports ("tatara-gateway-api" "tatara-ebpf"))
        (defgateway :gateway-class-name "nginx" :listeners ())
    "#;
    let forms = read(unused).unwrap();
    let env = compile_into_env(&forms).unwrap();
    let errors = validate(&env).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ValidationError::UnusedImport(c, _) if c == "tatara-ebpf")),
        "expected UnusedImport for tatara-ebpf, got {errors:?}"
    );
}

#[test]
fn validate_flags_duplicate_resource_id() {
    register_test_domains();
    let dup = r#"
        (defenv :name "dup" :description "x" :imports ("tatara-ebpf"))
        (defbpf-map :name "shared" :kind :array :value-size 8 :max-entries 1)
        (defbpf-map :name "shared" :kind :hash :value-size 16 :max-entries 1)
    "#;
    let forms = read(dup).unwrap();
    let env = compile_into_env(&forms).unwrap();
    let errors = validate(&env).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ValidationError::DuplicateResource { keyword, id } if keyword == "defbpf-map" && id == "shared")),
        "expected DuplicateResource for defbpf-map `shared`, got {errors:?}"
    );
}

#[test]
fn missing_defenv_errors() {
    register_test_domains();
    // Just resource forms, no `(defenv …)`.
    let no_env = r#"
        (defgateway :gateway-class-name "nginx" :listeners ())
    "#;
    let forms = read(no_env).unwrap();
    let err = compile_into_env(&forms).unwrap_err();
    assert!(matches!(
        err,
        tatara_env::CompileError::MissingDefenv
    ));
}

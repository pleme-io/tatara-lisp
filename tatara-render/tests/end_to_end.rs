//! End-to-end proof — eight-phase loop in test form.
//!
//! Walks the user-facing pipeline:
//!
//!   1. DECLARE   — author resources in tatara-lisp source
//!   2. SIMULATE  — `tatara_env::validate(&env)`
//!   3. RENDER    — `KubernetesYaml.render(&env)` produces YAML
//!                  manifests in the expected K8s shape
//!
//! The remaining phases (DEPLOY / VERIFY / RECONVERGE) bolt on
//! via `tatara_rollout::diff_envs` + an apply driver outside
//! this crate.

use tatara_env::{compile_into_env, register as register_env, validate};
use tatara_lisp::read;
use tatara_render::{Backend, KubernetesYaml};

const PROGRAM: &str = r#"
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
"#;

fn register_all() {
    register_env();
    tatara_gateway_api::register();
    tatara_ebpf::register();
}

#[test]
fn full_pipeline_declare_validate_render() {
    register_all();
    // Phase 1 — DECLARE.
    let forms = read(PROGRAM).unwrap();
    let env = compile_into_env(&forms).expect("compile");
    // Phase 2 — SIMULATE / validate. We don't actually deploy
    // anything; this just type-checks the env structure.
    validate(&env).expect("env validates");
    // Phase 5 — RENDER.
    let backend = KubernetesYaml::default();
    let manifests = backend.render(&env).expect("render");
    // Three resources → three manifests (one Gateway, one
    // ConfigMap for the bpf-map, one ConfigMap for the bpf-program).
    assert_eq!(manifests.len(), 3, "got {} manifests", manifests.len());

    // Spot-check the gateway manifest. Path is `gateway/<name>.yaml`
    // (kind.to_lowercase()) per the generic registry-driven path.
    let gw = manifests
        .iter()
        .find(|m| m.path.starts_with("gateway/"))
        .expect("gateway manifest present");
    assert!(gw.content.contains("apiVersion: gateway.networking.k8s.io/v1"));
    assert!(gw.content.contains("kind: Gateway"));
    assert!(gw.content.contains("nginx"), "gateway_class_name embedded");
    assert!(gw.content.contains("pleme.io/env: production"), "env label propagates");
    assert!(gw.content.contains("pleme.io/tier: prod"), "user labels propagate");

    // Spot-check the BPF program ConfigMap.
    let bpf = manifests
        .iter()
        .find(|m| m.path.contains("bpf-program-drop_syn_flood"))
        .expect("bpf-program manifest present");
    assert!(bpf.content.contains("kind: ConfigMap"));
    assert!(bpf.content.contains("spec.json:"));
    assert!(bpf.content.contains("drop_syn_flood"));
    assert!(bpf.content.contains(":xdp"));
}

#[test]
fn render_fails_softly_on_unsupported_keyword() {
    register_all();
    // Author an env with a domain we DON'T have a renderer for —
    // simulate by constructing an Env directly with a bogus
    // keyword.
    use serde_json::json;
    use tatara_env::compile::{Env, Resource};
    use tatara_env::EnvSpec;
    let env = Env {
        spec: EnvSpec {
            name: "unsupported".into(),
            description: "x".into(),
            imports: vec![],
            labels: indexmap::IndexMap::new(),
        },
        resources: vec![Resource {
            keyword: "deftotallyfake".into(),
            value: json!({"name": "fake"}),
        }],
    };
    let err = KubernetesYaml::default()
        .render(&env)
        .expect_err("unsupported keyword errors");
    assert!(format!("{err}").contains("deftotallyfake"));
}

#[test]
fn forge_generated_domains_auto_render_via_registry() {
    // The compounding claim made testable. Registering a new
    // forge-generated domain produces working YAML on the next
    // render(), with zero edits to tatara-render.
    register_all();
    let kws = tatara_lisp::domain::registered_render_keywords();
    // Both `defgateway` (forge-generated, has CRD apiVersion +
    // kind) AND no `defbpf-*` (hand-written, intentionally no
    // RenderableDomain because BPF resources don't have a single
    // CR shape).
    assert!(kws.contains(&"defgateway"), "defgateway has render metadata");
    assert!(
        !kws.contains(&"defbpf-program"),
        "defbpf-program intentionally omitted from render registry"
    );
    // Every forge-generated render registration carries the
    // upstream apiVersion + kind verbatim — proving the metadata
    // round-trips from CRD YAML through forge's emit pass.
    let gw_meta = tatara_lisp::domain::lookup_render("defgateway").unwrap();
    assert_eq!(gw_meta.api_version, "gateway.networking.k8s.io/v1");
    assert_eq!(gw_meta.kind, "Gateway");
}

#[test]
fn manifests_are_kustomize_friendly() {
    // Each manifest has a unique path within the env's tree;
    // FluxCD/Kustomize handle the sub-trees natively.
    register_all();
    let forms = read(PROGRAM).unwrap();
    let env = compile_into_env(&forms).unwrap();
    let manifests = KubernetesYaml::default().render(&env).unwrap();
    let paths: Vec<&str> = manifests.iter().map(|m| m.path.as_str()).collect();
    // Each path is unique.
    let unique: std::collections::HashSet<_> = paths.iter().collect();
    assert_eq!(unique.len(), paths.len(), "duplicate paths: {paths:?}");
    // Each ends in `.yaml`.
    for p in &paths {
        assert!(p.ends_with(".yaml"), "non-yaml path: {p}");
    }
}

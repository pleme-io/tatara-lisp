//! End-to-end proof — the full edge-of-differences pipeline.

use tatara_env::{compile_into_env, register as register_env};
use tatara_lisp::read;
use tatara_rollout::{diff_envs, ordered_apply, Change};

fn register_all() {
    register_env();
    tatara_gateway_api::register();
    tatara_ebpf::register();
}

const ENV_V1: &str = r#"
(defenv :name "v1" :description "x" :imports ("tatara-gateway-api" "tatara-ebpf"))
(defgateway :gateway-class-name "nginx" :listeners ())
(defbpf-map :name "syn_counter" :kind :per-cpu-array :value-size 8 :max-entries 1)
(defbpf-program
  :name "drop_syn_flood" :kind :xdp
  :attach (:target "eth0") :source "bpf/drop_syn.rs"
  :license "GPL" :uses-maps ("syn_counter"))
"#;

const ENV_V2_CHANGED: &str = r#"
(defenv :name "v1" :description "x" :imports ("tatara-gateway-api" "tatara-ebpf"))
(defgateway :gateway-class-name "nginx" :listeners ())
(defbpf-map :name "syn_counter" :kind :per-cpu-array :value-size 16 :max-entries 1)
(defbpf-program
  :name "drop_syn_flood" :kind :xdp
  :attach (:target "eth0") :source "bpf/drop_syn.rs"
  :license "GPL" :uses-maps ("syn_counter"))
"#;

const ENV_V3_ADDED: &str = r#"
(defenv :name "v1" :description "x" :imports ("tatara-gateway-api" "tatara-ebpf"))
(defgateway :gateway-class-name "nginx" :listeners ())
(defbpf-map :name "syn_counter" :kind :per-cpu-array :value-size 8 :max-entries 1)
(defbpf-program
  :name "drop_syn_flood" :kind :xdp
  :attach (:target "eth0") :source "bpf/drop_syn.rs"
  :license "GPL" :uses-maps ("syn_counter"))
(defbpf-program
  :name "rate_limit_tcp" :kind :tc
  :attach (:target "eth0" :direction "egress")
  :source "bpf/rate_limit_tcp.rs" :license "GPL"
  :uses-maps ("syn_counter"))
"#;

const ENV_V4_REMOVED: &str = r#"
(defenv :name "v1" :description "x" :imports ("tatara-gateway-api" "tatara-ebpf"))
(defgateway :gateway-class-name "nginx" :listeners ())
(defbpf-program
  :name "drop_syn_flood" :kind :xdp
  :attach (:target "eth0") :source "bpf/drop_syn.rs"
  :license "GPL" :uses-maps ("syn_counter"))
"#;

#[test]
fn identical_envs_produce_empty_plan() {
    register_all();
    let forms = read(ENV_V1).unwrap();
    let env = compile_into_env(&forms).unwrap();
    let plan = diff_envs(&env, &env);
    assert!(plan.is_empty());
    assert_eq!(plan.unchanged.len(), 3);
}

#[test]
fn changed_resource_appears_in_changes_only() {
    register_all();
    let v1 = compile_into_env(&read(ENV_V1).unwrap()).unwrap();
    let v2 = compile_into_env(&read(ENV_V2_CHANGED).unwrap()).unwrap();
    let plan = diff_envs(&v1, &v2);
    assert!(plan.adds.is_empty());
    assert!(plan.removes.is_empty());
    assert_eq!(plan.changes.len(), 1, "exactly one resource changed");
    let change = &plan.changes[0];
    let id = change.id();
    assert_eq!(id.keyword, "defbpf-map");
    assert_eq!(id.name, "syn_counter");
    if let Change::Change {
        old_blake3,
        new_blake3,
        ..
    } = change
    {
        assert_ne!(old_blake3, new_blake3, "fingerprints must differ");
    } else {
        panic!("expected Change variant");
    }
    // The unchanged set is the rest.
    assert_eq!(plan.unchanged.len(), 2);
}

#[test]
fn added_resource_appears_in_adds_only() {
    register_all();
    let v1 = compile_into_env(&read(ENV_V1).unwrap()).unwrap();
    let v3 = compile_into_env(&read(ENV_V3_ADDED).unwrap()).unwrap();
    let plan = diff_envs(&v1, &v3);
    assert!(plan.removes.is_empty());
    assert!(plan.changes.is_empty());
    assert_eq!(plan.adds.len(), 1);
    assert_eq!(plan.adds[0].id().name, "rate_limit_tcp");
}

#[test]
fn removed_resource_appears_in_removes_only() {
    register_all();
    let v1 = compile_into_env(&read(ENV_V1).unwrap()).unwrap();
    let v4 = compile_into_env(&read(ENV_V4_REMOVED).unwrap()).unwrap();
    let plan = diff_envs(&v1, &v4);
    assert!(plan.adds.is_empty());
    assert!(plan.changes.is_empty());
    assert_eq!(plan.removes.len(), 1);
    assert_eq!(plan.removes[0].id().name, "syn_counter");
}

#[test]
fn ordered_apply_respects_typed_dependencies() {
    // The compounding test for Layer 4. tatara-ebpf declares:
    //   defbpf-policy   depends on defbpf-program + defbpf-map
    //   defbpf-program  depends on defbpf-map
    //   defbpf-map      depends on (nothing)
    //
    // In an env that adds all three, ordered_apply must sort
    // them so dependencies land before dependents.
    register_all();
    let v0 = compile_into_env(
        &read(r#"(defenv :name "v0" :description "x" :imports ("tatara-ebpf"))"#).unwrap(),
    )
    .unwrap();
    let v1 = compile_into_env(
        &read(
            r#"
        (defenv :name "v1" :description "x" :imports ("tatara-ebpf"))
        (defbpf-policy :name "p" :description "x" :programs ("prog") :maps ("m"))
        (defbpf-program :name "prog" :kind :xdp :attach (:target "eth0")
            :source "bpf/x.rs" :license "GPL" :uses-maps ("m"))
        (defbpf-map :name "m" :kind :array :value-size 8 :max-entries 1)
        "#,
        )
        .unwrap(),
    )
    .unwrap();
    let plan = diff_envs(&v0, &v1);
    assert_eq!(plan.adds.len(), 3, "all three resources are adds");

    let ordered = ordered_apply(&plan).expect("toposort succeeds");
    let keywords: Vec<&str> = ordered.iter().map(|c| c.id().keyword.as_str()).collect();
    let pos = |kw: &str| keywords.iter().position(|k| *k == kw).unwrap();
    // defbpf-map (no deps) → first
    // defbpf-program (depends on defbpf-map) → after map
    // defbpf-policy (depends on both) → last
    assert!(
        pos("defbpf-map") < pos("defbpf-program"),
        "map before program: {keywords:?}"
    );
    assert!(
        pos("defbpf-program") < pos("defbpf-policy"),
        "program before policy: {keywords:?}"
    );
}

#[test]
fn ordered_apply_reverses_removes_so_dependents_torn_down_first() {
    // Drift the same env back to empty — every resource is a
    // remove. Removes must go in REVERSE topo order: dependents
    // first (defbpf-policy), dependencies last (defbpf-map).
    register_all();
    let v0 = compile_into_env(
        &read(
            r#"
        (defenv :name "v0" :description "x" :imports ("tatara-ebpf"))
        (defbpf-policy :name "p" :description "x" :programs ("prog") :maps ("m"))
        (defbpf-program :name "prog" :kind :xdp :attach (:target "eth0")
            :source "bpf/x.rs" :license "GPL" :uses-maps ("m"))
        (defbpf-map :name "m" :kind :array :value-size 8 :max-entries 1)
        "#,
        )
        .unwrap(),
    )
    .unwrap();
    let v1 = compile_into_env(
        &read(r#"(defenv :name "v1" :description "x" :imports ("tatara-ebpf"))"#).unwrap(),
    )
    .unwrap();
    let plan = diff_envs(&v0, &v1);
    assert_eq!(plan.removes.len(), 3);

    let ordered = ordered_apply(&plan).unwrap();
    let keywords: Vec<&str> = ordered.iter().map(|c| c.id().keyword.as_str()).collect();
    let pos = |kw: &str| keywords.iter().position(|k| *k == kw).unwrap();
    // Removes go in REVERSE topo: policy first, map last.
    assert!(
        pos("defbpf-policy") < pos("defbpf-program"),
        "policy removed before program: {keywords:?}"
    );
    assert!(
        pos("defbpf-program") < pos("defbpf-map"),
        "program removed before map: {keywords:?}"
    );
}

#[test]
fn fingerprints_are_namespaced_per_attestation_layer() {
    // Layer 6 proof. Two resources with byte-identical JSON
    // payloads but different domain semantics produce different
    // fingerprints because their AttestableDomain namespaces
    // differ. Cross-domain hash collisions in the tameshi tree
    // are mechanically impossible.
    register_all();
    use serde_json::json;
    use tatara_env::compile::Resource;
    use tatara_rollout::fingerprint_resource;
    let r_bpf = Resource {
        keyword: "defbpf-map".into(),
        value: json!({"name": "shared", "value_size": 8}),
    };
    let r_gw = Resource {
        keyword: "defgateway".into(),
        value: json!({"name": "shared", "value_size": 8}),
    };
    let fp_bpf = fingerprint_resource(&r_bpf);
    let fp_gw = fingerprint_resource(&r_gw);
    assert_ne!(
        fp_bpf.blake3, fp_gw.blake3,
        "different namespaces → different fingerprints (layer 6)"
    );
}

#[test]
fn layer_8_lifecycle_strategy_is_per_keyword() {
    // Layer 8 proof. tatara-ebpf overrides the default
    // (Immediate) for its three keywords: programs + policies
    // need BlueGreen (kernel verifier won't accept partial
    // load), maps need Recreate (no in-place size resize).
    // Forge-generated catalog domains keep the Immediate default.
    register_all();
    let prog = tatara_lisp::domain::lookup_lifecycle("defbpf-program").unwrap();
    assert_eq!(prog.strategy, tatara_lisp::RolloutStrategy::BlueGreen);
    let map = tatara_lisp::domain::lookup_lifecycle("defbpf-map").unwrap();
    assert_eq!(map.strategy, tatara_lisp::RolloutStrategy::Recreate);
    let policy = tatara_lisp::domain::lookup_lifecycle("defbpf-policy").unwrap();
    assert_eq!(policy.strategy, tatara_lisp::RolloutStrategy::BlueGreen);
    let gw = tatara_lisp::domain::lookup_lifecycle("defgateway").unwrap();
    assert_eq!(gw.strategy, tatara_lisp::RolloutStrategy::Immediate);
    // BPF programs drain in 5s (faster than the 30s default —
    // they detach atomically at the kernel level).
    assert_eq!(prog.drain_seconds, 5);
}

#[test]
fn iter_actionable_orders_removes_then_adds_then_changes() {
    register_all();
    let v1 = compile_into_env(&read(ENV_V1).unwrap()).unwrap();
    let v2_combined = r#"
        (defenv :name "v1" :description "x" :imports ("tatara-gateway-api" "tatara-ebpf"))
        (defgateway :gateway-class-name "nginx" :listeners ())
        (defbpf-map :name "syn_counter" :kind :per-cpu-array :value-size 16 :max-entries 1)
        (defbpf-program :name "rate_limit_tcp" :kind :tc
            :attach (:target "eth0" :direction "egress")
            :source "bpf/rate_limit_tcp.rs" :license "GPL"
            :uses-maps ("syn_counter"))
    "#;
    let v2 = compile_into_env(&read(v2_combined).unwrap()).unwrap();
    let plan = diff_envs(&v1, &v2);
    // We removed `drop_syn_flood`, added `rate_limit_tcp`,
    // changed `syn_counter`.
    assert_eq!(plan.removes.len(), 1);
    assert_eq!(plan.adds.len(), 1);
    assert_eq!(plan.changes.len(), 1);
    let actionable: Vec<&str> = plan
        .iter_actionable()
        .map(|c| c.id().name.as_str())
        .collect();
    // removes first, then adds, then changes.
    assert_eq!(actionable[0], "drop_syn_flood");
    assert_eq!(actionable[1], "rate_limit_tcp");
    assert_eq!(actionable[2], "syn_counter");
}

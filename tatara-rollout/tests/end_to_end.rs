//! End-to-end proof — the full edge-of-differences pipeline.

use tatara_env::{compile_into_env, register as register_env};
use tatara_lisp::read;
use tatara_rollout::{diff_envs, Change};

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

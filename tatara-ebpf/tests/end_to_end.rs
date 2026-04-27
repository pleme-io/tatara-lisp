//! End-to-end proof for the eBPF authoring surface.
//!
//! Walks the full pipeline a user goes through:
//!
//!   1. Register the eBPF domain with tatara-lisp.
//!   2. Read a real (defbpf-program …) (defbpf-map …) (defbpf-policy …)
//!      authored block from a string.
//!   3. Compile each form via the registered handler — typed
//!      serde_json::Value out.
//!   4. Materialize the JSON back into typed Rust structs.
//!   5. Validate ref-coherence on the policy (every program / map
//!      reference resolves).
//!   6. Run codegen on a program — produce aya-compatible Rust source.
//!   7. Drive a SimulatedRuntime through the full lifecycle.
//!
//! If this passes, the eBPF tier of the pleme-io cloud stack is
//! authorable + composable + buildable end-to-end.

use std::collections::HashMap;
use tatara_ebpf::{
    codegen, register, runtime::BpfRuntime, runtime::SimulatedRuntime, BpfMapSpec,
    BpfPolicySpec, BpfProgramSpec,
};
use tatara_lisp::read;

const SAMPLE_POLICY: &str = r#"
;; Edge protection — drop SYN floods on the WAN interface, count them
;; per-CPU into a map a userspace exporter periodically scrapes.
;;
;; Convention: tatara `def…` forms are pure-kwarg after the head. The
;; `:name` field is the canonical identifier; the head symbol's
;; following token is reserved for future hygienic gensym binding.

(defbpf-map
  :name "syn_counter"
  :kind :per-cpu-array
  :key-size 4
  :value-size 8
  :max-entries 1
  :pin-path "/sys/fs/bpf/syn_counter")

(defbpf-program
  :name "drop_syn_flood"
  :kind :xdp
  :attach (:target "eth0")
  :source "bpf/drop_syn.rs"
  :license "GPL"
  :uses-maps ("syn_counter")
  :pin-path "/sys/fs/bpf/drop_syn_flood")

(defbpf-policy
  :name "edge_protection"
  :description "L4 SYN-flood mitigation on the WAN interface."
  :programs ("drop_syn_flood")
  :maps ("syn_counter"))
"#;

#[test]
fn end_to_end_ebpf_authoring_pipeline() {
    register();
    let kws = tatara_lisp::domain::registered_keywords();
    for required in ["defbpf-program", "defbpf-map", "defbpf-policy"] {
        assert!(
            kws.contains(&required),
            "missing keyword `{required}` after register, got {kws:?}"
        );
    }

    let forms = read(SAMPLE_POLICY).expect("reader parses sample policy");
    assert_eq!(forms.len(), 3, "three top-level forms in the sample");

    let mut programs_by_name: HashMap<String, BpfProgramSpec> = HashMap::new();
    let mut maps_by_name: HashMap<String, BpfMapSpec> = HashMap::new();
    let mut policy: Option<BpfPolicySpec> = None;

    for form in &forms {
        let list = form.as_list().expect("form is a list");
        let head = list[0].as_symbol().expect("head symbol");
        let handler =
            tatara_lisp::domain::lookup(head).expect("domain registered for this head");
        let value = (handler.compile)(&list[1..]).expect("compile succeeds");
        match head {
            "defbpf-map" => {
                let m: BpfMapSpec =
                    serde_json::from_value(value).expect("BpfMapSpec round-trip");
                maps_by_name.insert(m.name.clone(), m);
            }
            "defbpf-program" => {
                let p: BpfProgramSpec =
                    serde_json::from_value(value).expect("BpfProgramSpec round-trip");
                programs_by_name.insert(p.name.clone(), p);
            }
            "defbpf-policy" => {
                let p: BpfPolicySpec =
                    serde_json::from_value(value).expect("BpfPolicySpec round-trip");
                policy = Some(p);
            }
            _ => unreachable!(),
        }
    }

    let policy = policy.expect("sample contains one defbpf-policy");
    policy
        .validate(&programs_by_name, &maps_by_name)
        .expect("policy refs resolve cleanly");

    // Codegen — emit aya-compatible Rust for the program.
    let prog = programs_by_name.get("drop_syn_flood").expect("program present");
    assert!(matches!(
        codegen::classify_source(&prog.source).unwrap(),
        codegen::SourceShape::RustFile(_)
    ));
    let body = "// hand-written Rust body lives in bpf/drop_syn.rs\nOk(0)";
    let source = codegen::emit_aya_program(prog, body);
    assert!(source.contains("#[xdp]"));
    assert!(source.contains("pub fn drop_syn_flood(ctx: aya_ebpf::programs::XdpContext) -> u32"));
    assert!(source.contains("// hand-written Rust body lives in bpf/drop_syn.rs"));

    // Drive a simulated runtime — exercises the trait surface.
    let mut rt = SimulatedRuntime::new();
    for m in maps_by_name.values() {
        rt.create_map(m).unwrap();
    }
    let loaded = rt.load_program(prog).unwrap();
    rt.attach_program(&loaded).unwrap();
    assert_eq!(rt.attached_programs, vec!["drop_syn_flood"]);
    assert_eq!(rt.created_maps.len(), 1);
    rt.detach_program(loaded).unwrap();
    assert!(rt.attached_programs.is_empty());
}

#[test]
fn policy_validation_catches_dangling_program_ref() {
    let policy = BpfPolicySpec {
        name: "broken".into(),
        description: "policy referencing a program that doesn't exist".into(),
        programs: vec!["does_not_exist".into()],
        maps: vec![],
    };
    let errors = policy
        .validate(&HashMap::new(), &HashMap::new())
        .unwrap_err();
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].contains("does_not_exist"),
        "error message names the missing program"
    );
}

#[test]
fn policy_validation_catches_program_using_undeclared_map() {
    let prog = BpfProgramSpec {
        name: "p".into(),
        kind: tatara_ebpf::BpfProgramKind::Xdp,
        attach: tatara_ebpf::BpfAttachPoint {
            target: "eth0".into(),
            direction: None,
        },
        source: "bpf/p.rs".into(),
        license: "GPL".into(),
        pin_path: None,
        uses_maps: vec!["ghost".into()],
    };
    let policy = BpfPolicySpec {
        name: "broken-map".into(),
        description: "program references a map the policy doesn't declare".into(),
        programs: vec!["p".into()],
        maps: vec![],
    };
    let mut programs = HashMap::new();
    programs.insert(prog.name.clone(), prog);
    let errors = policy.validate(&programs, &HashMap::new()).unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("ghost")),
        "errors flag the missing `ghost` map: {errors:?}"
    );
}

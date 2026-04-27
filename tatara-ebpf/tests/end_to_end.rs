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

/// The "best merger of the two" — a tatara-lisp authored body
/// lowers to aya-Rust source, which the codegen pass wraps in
/// the right BPF attribute. Pillar 1 demonstrated end-to-end at
/// the kernel-up tier.
#[test]
fn lisp_authored_bpf_body_lowers_through_codegen() {
    use tatara_ebpf::bpf_fn::{lower, BpfExpr, BpfFn, CompareOp};
    use tatara_ebpf::codegen::emit_aya_program;
    use tatara_ebpf::{BpfAttachPoint, BpfProgramKind, BpfProgramSpec};

    // Equivalent Lisp source (for documentation):
    //
    //   (bpf-fn drop-syn-flood (ctx)
    //     (let ((cpu (call get-current-cpu)))
    //       (if (= cpu 0)
    //           (return :xdp-drop)
    //           (return :xdp-pass))))
    //
    // Programmatically constructed here so the test is hermetic;
    // the reader-side parsing of `(bpf-fn …)` is the next step.
    let f = BpfFn {
        name: "drop_syn_flood".into(),
        ctx: "ctx".into(),
        body: vec![BpfExpr::Let {
            name: "cpu".into(),
            value: Box::new(BpfExpr::Call {
                helper: "get-current-cpu".into(),
                args: vec![],
            }),
            body: vec![BpfExpr::If {
                cond: Box::new(BpfExpr::Compare {
                    op: CompareOp::Eq,
                    left: Box::new(BpfExpr::Var("cpu".into())),
                    right: Box::new(BpfExpr::Int(0)),
                }),
                then: Box::new(BpfExpr::Return {
                    action: "xdp-drop".into(),
                }),
                otherwise: Box::new(BpfExpr::Return {
                    action: "xdp-pass".into(),
                }),
            }],
        }],
    };
    let body_rust = lower(&f).expect("lisp body lowers to rust");

    // The lowered source already includes the `pub fn` signature
    // + body. The wrapper pass adds the `#[xdp]` attribute around
    // it. Compose them by extracting the body block and feeding
    // through `emit_aya_program`.
    let spec = BpfProgramSpec {
        name: "drop_syn_flood".into(),
        kind: BpfProgramKind::Xdp,
        attach: BpfAttachPoint {
            target: "eth0".into(),
            direction: None,
        },
        source: "examples/lisp-authored.tlisp:drop-syn-flood".into(),
        license: "GPL".into(),
        pin_path: None,
        uses_maps: vec![],
    };

    // The wrapper expects just the body block contents. Since
    // `lower` produces a complete `pub fn` definition, we slice
    // out the `{ … }` body for re-wrapping. (In a fuller pipeline,
    // `lower` would have a `lower_body_only` variant; for the
    // MVP we extract here.)
    let open = body_rust.find('{').unwrap();
    let close = body_rust.rfind('}').unwrap();
    let body_block = body_rust[open + 1..close].trim();
    let wrapped = emit_aya_program(&spec, body_block);

    assert!(wrapped.contains("#[xdp]"));
    assert!(wrapped.contains("pub fn drop_syn_flood"));
    assert!(wrapped.contains("aya_ebpf::helpers::bpf_get_smp_processor_id"));
    assert!(wrapped.contains("XDP_DROP"));
    assert!(wrapped.contains("XDP_PASS"));
    assert!(wrapped.contains("if (cpu == 0_i64)"));
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

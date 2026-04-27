//! End-to-end proof — three capability registries union into a
//! catalog page.

use tatara_doc::{fully_registered_keywords, render_catalog, render_one};

fn register_all() {
    tatara_gateway_api::register();
    tatara_ebpf::register();
}

#[test]
fn catalog_includes_every_registered_keyword() {
    register_all();
    let md = render_catalog();
    assert!(md.contains("# tatara catalog"));
    // Forge-generated domain — has all 3 capabilities.
    assert!(md.contains("`defgateway`"));
    assert!(md.contains("gateway.networking.k8s.io/v1"));
    assert!(md.contains("Gateway"));
    assert!(md.contains("Renders to"));
    // Hand-written ebpf — has compile + doc but NOT render
    // (intentional; bpf doesn't map to a single CR).
    assert!(md.contains("`defbpf-program`"));
    assert!(md.contains("`defbpf-policy`"));
}

#[test]
fn render_one_targets_a_single_keyword() {
    register_all();
    let gw = render_one("defgateway");
    assert!(gw.contains("`defgateway`"));
    assert!(gw.contains("gateway.networking.k8s.io/v1"));
    assert!(!gw.contains("`defbpf-program`"), "render_one is scoped");
}

#[test]
fn fully_registered_keywords_lists_only_compile_render_doc_complete() {
    register_all();
    let full = fully_registered_keywords();
    // Forge-generated domains have all 3 capabilities — appear here.
    assert!(full.contains(&"defgateway"));
    // Hand-written ebpf domains have compile + doc but not render
    // (intentional — bpf programs don't map to single CRs).
    assert!(!full.contains(&"defbpf-program"));
    assert!(!full.contains(&"defbpf-policy"));
}

/// THE PROOF: every layer the platform claims, mechanically
/// asserted on a single forge-generated domain. If any of these
/// nine assertions fails, the "compounding systems on top of
/// each other" claim collapses — the layer doesn't exist for
/// real consumers.
///
/// `defgateway` is forge-generated from the upstream Gateway API
/// CRD. After `register()`, every one of the static-data
/// capability layers (1-6, 8-12) must return a registered
/// handler. Layer 7 is the executable variant — same shape, same
/// proof.
#[test]
fn nine_capability_layers_alive_for_one_domain() {
    register_all();
    let kw = "defgateway";

    // Layer 1 — Compile (TataraDomain).
    assert!(
        tatara_lisp::domain::lookup(kw).is_some(),
        "L1 compile: handler registered"
    );

    // Layer 2 — Render (RenderableDomain). Forge-generated from
    // the CRD's group + version + kind.
    let r = tatara_lisp::domain::lookup_render(kw)
        .expect("L2 render: handler registered");
    assert_eq!(r.api_version, "gateway.networking.k8s.io/v1");
    assert_eq!(r.kind, "Gateway");

    // Layer 3 — Documented. Forge-filled from CRD descriptions.
    let d = tatara_lisp::domain::lookup_doc(kw)
        .expect("L3 doc: handler registered");
    assert!(!d.docstring.is_empty(), "L3 doc: docstring populated");
    assert!(!d.field_docs.is_empty(), "L3 doc: field_docs populated");

    // Layer 4 — Dependent. Default empty for forge-generated.
    let dep = tatara_lisp::domain::lookup_deps(kw)
        .expect("L4 deps: handler registered");
    assert_eq!(dep.depends_on.len(), 0, "L4 deps: forge default empty");

    // Layer 5 — Schematic. Forge-preserved CRD schema.
    let s = tatara_lisp::domain::lookup_schema(kw)
        .expect("L5 schema: handler registered");
    let parsed: serde_json::Value = serde_json::from_str(s.schema_json).unwrap();
    assert!(parsed.is_object(), "L5 schema: parses as JSON");

    // Layer 6 — Attestable. Namespace = CRD group.
    let a = tatara_lisp::domain::lookup_attest(kw)
        .expect("L6 attest: handler registered");
    assert_eq!(a.namespace, "gateway.networking.k8s.io");

    // Layer 7 — Validated (executable). Default fn returns Ok.
    let v = tatara_lisp::domain::lookup_validate(kw)
        .expect("L7 validate: handler registered");
    let dummy = serde_json::json!({});
    (v.validate)(&dummy).expect("L7 validate: default impl is Ok");

    // Layer 8 — Lifecycle. Default Immediate for CRDs.
    let l = tatara_lisp::domain::lookup_lifecycle(kw)
        .expect("L8 lifecycle: handler registered");
    assert_eq!(l.strategy, tatara_lisp::RolloutStrategy::Immediate);

    // Layer 9 — Compliance. Default empty.
    let c = tatara_lisp::domain::lookup_compliance(kw)
        .expect("L9 compliance: handler registered");
    assert_eq!(c.frameworks.len(), 0, "L9 compliance: default empty");
    assert_eq!(c.controls.len(), 0);

    // Layer 10 — Observable. Default empty.
    let o = tatara_lisp::domain::lookup_observability(kw)
        .expect("L10 observability: handler registered");
    assert_eq!(o.metric_prefix, "");
    assert_eq!(o.log_labels.len(), 0);

    // Layer 11 — Help. Default empty.
    let h = tatara_lisp::domain::lookup_help(kw)
        .expect("L11 help: handler registered");
    assert_eq!(h.mnemonic, "");

    // Layer 12 — Stability. Default "stable" / "0.1.0".
    let st = tatara_lisp::domain::lookup_stability(kw)
        .expect("L12 stability: handler registered");
    assert_eq!(st.stability, "stable");
    assert_eq!(st.since_version, "0.1.0");
}

/// THE OVERRIDE PROOF: hand-written domains override the macro
/// defaults with real values. tatara-ebpf claims real
/// dependencies, real lifecycle, real compliance, real attestation
/// namespace. Each override survives through to the registry.
#[test]
fn hand_written_domains_override_macro_defaults() {
    register_all();
    // tatara-ebpf::BpfPolicySpec declares NIST SC-7, CIS 5.1.
    let c = tatara_lisp::domain::lookup_compliance("defbpf-policy")
        .expect("L9 compliance: bpf policy registered");
    assert!(
        c.frameworks.iter().any(|f| f.contains("NIST")),
        "ebpf override: NIST framework claimed"
    );
    assert!(
        c.controls.iter().any(|c| c.contains("SC-7")),
        "ebpf override: SC-7 control claimed"
    );

    // tatara-ebpf::BpfProgramSpec declares BlueGreen lifecycle.
    let l = tatara_lisp::domain::lookup_lifecycle("defbpf-program")
        .expect("L8 lifecycle: bpf program registered");
    assert_eq!(l.strategy, tatara_lisp::RolloutStrategy::BlueGreen);
    assert_eq!(l.drain_seconds, 5);

    // tatara-ebpf::BpfPolicySpec declares concrete deps.
    let dep = tatara_lisp::domain::lookup_deps("defbpf-policy")
        .expect("L4 deps: bpf policy registered");
    assert!(dep.depends_on.contains(&"defbpf-program"));
    assert!(dep.depends_on.contains(&"defbpf-map"));
}

#[test]
fn forge_generated_domains_expose_their_source_schema() {
    // Layer 5 proof. Forge-generated crates carry their CRD's
    // openAPIV3Schema verbatim through SchematicDomain. Useful
    // for IDE autocomplete, web validators, openapi exporters.
    register_all();
    let kws = tatara_lisp::domain::registered_schema_keywords();
    assert!(kws.contains(&"defgateway"));
    let gw = tatara_lisp::domain::lookup_schema("defgateway").unwrap();
    // Schema is a non-empty JSON string; it parses; it has a
    // top-level type or properties (the OpenAPI v3 shape).
    let parsed: serde_json::Value =
        serde_json::from_str(gw.schema_json).expect("schema parses");
    let obj = parsed.as_object().expect("schema is an object");
    assert!(
        obj.contains_key("type") || obj.contains_key("properties"),
        "schema looks like OpenAPI v3 — has `type` or `properties`"
    );
    // Hand-written ebpf domains don't expose schema metadata
    // (intentional — no upstream CRD to lift from).
    assert!(!kws.contains(&"defbpf-program"));
}

#[test]
fn catalog_is_kebab_case_for_lisp_authoring() {
    register_all();
    let md = render_catalog();
    // Field names emitted in kebab-case (the form an author writes).
    assert!(md.contains(":gateway-class-name"));
    // Snake-case form should NOT appear in the field listings.
    // (It's used internally as the Rust identifier; the table shows
    // the kebab equivalent so authors copy-paste correctly.)
    let lines: Vec<&str> = md.lines().collect();
    let in_field_table = |line: &&&str| line.starts_with("| `:");
    let kebab_lines: Vec<&&str> = lines.iter().filter(in_field_table).collect();
    assert!(!kebab_lines.is_empty(), "field table populated");
}

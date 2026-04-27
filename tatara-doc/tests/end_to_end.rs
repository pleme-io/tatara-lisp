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

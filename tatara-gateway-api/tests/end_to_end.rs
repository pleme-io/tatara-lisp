//! End-to-end proof — the full vision validated in one test.
//!
//! Path:
//!
//! ```text
//!   K8s CRD YAML
//!     → tatara-domain-forge   (this crate is the output)
//!       → typed Rust struct + #[derive(TataraDomain)]
//!         → register() onto the global domain registry
//!           → tatara-lisp reader parses (defgateway …)
//!             → registry.compile(args) → typed serde_json::Value
//! ```
//!
//! If this test passes, the entire Pillar 12 pipeline works for
//! Gateway API specifically — and by construction, for every other
//! CRD-shaped CNCF project (cilium, prometheus-operator, argo-cd,
//! keda, knative-serving, …) since the forge is schema-agnostic.

use tatara_lisp::read;

#[test]
fn registering_gateway_api_domain_unlocks_defgateway_form() {
    // Step 1: register every keyword form this domain exposes.
    // After this call, the global domain registry has `defgateway`
    // wired to `GatewaySpec`'s compile fn.
    tatara_gateway_api::register();

    // Step 2: confirm the keyword landed in the registry.
    let keywords = tatara_lisp::domain::registered_keywords();
    assert!(
        keywords.contains(&"defgateway"),
        "expected `defgateway` registered, found {keywords:?}"
    );

    // Step 3: read a real-shaped Lisp form. This is what an embedder
    // would author in their .tlisp source — `gatewayClassName` is
    // a required field on the upstream CRD; the rest are optional.
    // Empty list is `()` (an empty s-expression). `(list)` would
    // be a 1-element list containing the symbol `list`.
    let src = r#"
        (defgateway
          :gateway-class-name "nginx"
          :listeners ())
    "#;
    let forms = read(src).expect("reader parses (defgateway …)");
    assert_eq!(forms.len(), 1, "one top-level form");

    // Step 4: pull the keyword out of the form, look up the
    // registry handler, hand it the args.
    let list = forms[0].as_list().expect("form is a list");
    let head = list[0].as_symbol().expect("head is a symbol");
    assert_eq!(head, "defgateway");

    let handler = tatara_lisp::domain::lookup(head)
        .expect("`defgateway` is registered after register()");
    let value = (handler.compile)(&list[1..]).expect("compile succeeds");

    // Step 5: assert the typed value round-tripped correctly. The
    // forge generated `gateway_class_name: String` (required) — we
    // should see it in the JSON. Optional fields default to nothing
    // (or an empty list for `listeners` since we passed `(list)`).
    let obj = value
        .as_object()
        .expect("compile returns a JSON object");
    assert_eq!(
        obj.get("gateway_class_name")
            .and_then(|v| v.as_str()),
        Some("nginx"),
        "required field round-trips"
    );
    let listeners = obj
        .get("listeners")
        .and_then(|v| v.as_array())
        .expect("listeners array present");
    assert_eq!(listeners.len(), 0, "empty list survives");
}

#[test]
fn missing_required_field_errors_loudly() {
    tatara_gateway_api::register();
    let src = "(defgateway :listeners (list))";
    let forms = read(src).unwrap();
    let list = forms[0].as_list().unwrap();
    let handler = tatara_lisp::domain::lookup("defgateway").unwrap();
    let err = (handler.compile)(&list[1..]).unwrap_err();
    // The error should mention the missing required field —
    // `gateway-class-name` is the kebab form of `gateway_class_name`.
    let msg = format!("{err}");
    assert!(
        msg.contains("gateway-class-name") || msg.contains("gateway_class_name"),
        "expected missing-field message to name the field, got `{msg}`"
    );
}

//! `exec-with-stdin` / `exec-with-env` — the credential-carrying exec forms.
//!
//! The property under test is not "the value arrives" but "the value arrives
//! WITHOUT passing through argv". argv is readable from the process table by
//! any local user, and in CI by co-tenant steps and sibling containers, so a
//! form that delivers a secret through an argument is the defect these exist
//! to replace.

use tatara_lisp_script::{eval_str, Value};

/// Credential-SHAPED so the tests are realistic, carrying an EXAMPLE marker so
/// a scanner can tell it is a fixture.
const SECRET: &str = "ghp_EXAMPLENOTAREALTOKENxxxxxxxxxxxxxxxx";

fn eval(src: &str) -> String {
    match eval_str(src).unwrap_or_else(|e| panic!("script failed: {e}")) {
        Value::Str(s) => s.to_string(),
        other => panic!("expected a string result, got {other:?}"),
    }
}

#[test]
fn stdin_sink_delivers_the_value() {
    let out = eval(&format!(
        r#"(alist-get (exec-with-stdin "{SECRET}" "cat") "stdout" "")"#
    ));
    assert!(out.contains(SECRET), "stdin payload did not reach the child");
}

#[test]
fn env_sink_delivers_the_value() {
    let out = eval(&format!(
        r#"(alist-get (exec-with-env (list (list "TOK" "{SECRET}")) "sh" "-c" "printf %s \"$TOK\"") "stdout" "")"#
    ));
    assert!(out.contains(SECRET), "env value did not reach the child");
}

/// The point of both forms: the child can see the secret, but it was never an
/// argument. Asked of the child itself via "$@" rather than asserted about our
/// own call, so this measures the process rather than the intent.
#[test]
fn neither_sink_puts_the_value_in_argv() {
    let via_stdin = eval(&format!(
        r#"(alist-get (exec-with-stdin "{SECRET}" "sh" "-c" "printf %s \"$*\"" "_" "login" "--password-stdin") "stdout" "")"#
    ));
    assert!(
        !via_stdin.contains(SECRET),
        "exec-with-stdin leaked the value into argv: {via_stdin}"
    );

    let via_env = eval(&format!(
        r#"(alist-get (exec-with-env (list (list "TOK" "{SECRET}")) "sh" "-c" "printf %s \"$*\"" "_" "login") "stdout" "")"#
    ));
    assert!(
        !via_env.contains(SECRET),
        "exec-with-env leaked the value into argv: {via_env}"
    );
}

/// The form being replaced, kept as the contrast. If this ever stops leaking,
/// the tests above have lost their meaning.
#[test]
fn plain_exec_capture_does_leak_into_argv() {
    let leaked = eval(&format!(
        r#"(alist-get (exec-capture "sh" "-c" "printf %s \"$*\"" "_" "login" "--password" "{SECRET}") "stdout" "")"#
    ));
    assert!(
        leaked.contains(SECRET),
        "expected the plain form to expose the value in argv — if it no longer \
         does, the contrast these tests rest on is gone"
    );
}

/// A malformed env entry must error rather than be skipped: silently dropping
/// a pair runs the child WITHOUT its credential and reports success, which is
/// harder to diagnose than a failure.
#[test]
fn malformed_env_alist_is_rejected_not_skipped() {
    assert!(
        eval_str(r#"(exec-with-env (list "NOTAPAIR") "true")"#).is_err(),
        "a malformed env entry must be an error"
    );
    assert!(
        eval_str(r#"(exec-with-env "notalist" "true")"#).is_err(),
        "a non-list env argument must be an error"
    );
}

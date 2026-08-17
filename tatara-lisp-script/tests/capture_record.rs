//! The CAPTURE RECORD — the eight fields every capture-form exec returns.
//!
//! The property under test is not "the command ran" but "the record can answer
//! WHICH binary ran and WHERE". That is what makes a deshellify port provable:
//! the correctness argument is always "the new thing does what the old thing
//! did", which is a COMPARISON, and an invocation that was not recorded cannot
//! be compared.
//!
//! Before 2026-08-17 the record carried three fields (status/stdout/stderr), so
//! `Command::new("kubectl")` running whatever `PATH` resolved first — the
//! silent-PATH-fallback class — was invisible from the record and could only be
//! settled by reading source.

use tatara_lisp_script::{Value, eval_str};

fn eval_str_field(src: &str) -> String {
    match eval_str(src).unwrap_or_else(|e| panic!("script failed: {e}")) {
        Value::Str(s) => s.to_string(),
        other => panic!("expected a string, got {other:?}"),
    }
}

fn eval_int_field(src: &str) -> i64 {
    match eval_str(src).unwrap_or_else(|e| panic!("script failed: {e}")) {
        Value::Int(n) => n,
        other => panic!("expected an int, got {other:?}"),
    }
}

#[test]
fn the_three_original_fields_are_unchanged() {
    // The regression guard: widening the record must not disturb any existing
    // consumer. Nothing in the tree asserted its length, and these three are
    // what `status-of` / `stdout-of` / `stderr-of` read.
    assert_eq!(
        eval_str_field(r#"(alist-get (exec-capture "echo" "hi") "stdout" "")"#).trim(),
        "hi"
    );
    assert_eq!(
        eval_int_field(r#"(alist-get (exec-capture "true") "status" -99)"#),
        0
    );
    assert_eq!(
        eval_int_field(r#"(alist-get (exec-capture "false") "status" -99)"#),
        1,
        "a non-zero exit must still be reported as itself"
    );
}

#[test]
fn program_records_what_was_asked_for() {
    assert_eq!(
        eval_str_field(r#"(alist-get (exec-capture "echo" "x") "program" "")"#),
        "echo"
    );
}

#[test]
fn resolved_answers_which_binary_actually_ran() {
    // THE field the widening exists for. A bare name must come back as an
    // absolute path, so "did the right tool run" is answerable from the record
    // instead of by reading source.
    let resolved = eval_str_field(r#"(alist-get (exec-capture "echo" "x") "resolved" "")"#);
    assert!(
        resolved.starts_with('/'),
        "a bare program name must resolve to an absolute path, got {resolved:?}"
    );
    assert!(
        resolved.ends_with("echo"),
        "the resolved path must name the program, got {resolved:?}"
    );
    assert_ne!(
        resolved, "echo",
        "resolved must not merely echo the program back — that would render \
         'resolved' and 'unresolved' as the same bytes"
    );
}

#[test]
fn an_absolute_program_is_already_unambiguous() {
    // No PATH lookup to do; the record should carry it verbatim rather than
    // re-resolving and risking a different answer.
    let resolved = eval_str_field(r#"(alist-get (exec-capture "/bin/echo" "x") "resolved" "")"#);
    assert_eq!(resolved, "/bin/echo");
}

#[test]
fn argv_is_a_list_and_keeps_arguments_separate() {
    // argv must be a LIST, never a joined string: re-quoting a command changes
    // it, so a single string cannot be compared against what was run. An
    // argument containing a space is the case that proves it.
    let joined = eval_str_field(
        r#"(string-join "|" (alist-get (exec-capture "echo" "a b" "c") "argv" (list)))"#,
    );
    assert_eq!(
        joined, "echo|a b|c",
        "argv must preserve argument boundaries, including a space INSIDE one"
    );
}

#[test]
fn cwd_is_recorded_as_an_absolute_path() {
    // The field that distinguishes a write to a flake's read-only /nix/store
    // source copy from one to the work tree — a distinction that otherwise
    // surfaces only as a bare `Permission denied (os error 13)`.
    let cwd = eval_str_field(r#"(alist-get (exec-capture "true") "cwd" "")"#);
    assert!(cwd.starts_with('/'), "cwd must be absolute, got {cwd:?}");
}

#[test]
fn duration_is_present_and_plausible() {
    // Separates "failed" from "hung", which a status alone cannot.
    let fast = eval_int_field(r#"(alist-get (exec-capture "true") "duration-ms" -1)"#);
    assert!(fast >= 0, "duration must be non-negative, got {fast}");
    assert!(
        fast < 60_000,
        "`true` should not take a minute; got {fast}ms — suggests the clock is \
         measuring the wrong span"
    );
}

#[test]
fn sh_exec_records_that_a_SHELL_was_involved() {
    // sh-exec is the one primitive that hands a string to a shell, and the
    // record must SHOW that rather than presenting the script as the program —
    // otherwise a reader auditing for shell usage cannot see it in the record.
    assert_eq!(
        eval_str_field(r#"(alist-get (sh-exec "echo hi") "program" "")"#),
        "sh"
    );
    let joined =
        eval_str_field(r#"(string-join "|" (alist-get (sh-exec "echo hi") "argv" (list)))"#);
    assert_eq!(joined, "sh|-c|echo hi");
}

#[test]
fn every_capture_form_returns_the_same_field_set() {
    // One record shape, four primitives. A caller must not have to know WHICH
    // exec form produced a record in order to read it.
    for expr in [
        r#"(exec-capture "echo" "x")"#,
        r#"(exec-with-stdin "" "cat")"#,
        r#"(exec-with-env (list (list "K" "V")) "echo" "x")"#,
        r#"(sh-exec "echo x")"#,
    ] {
        for field in [
            "status",
            "stdout",
            "stderr",
            "argv",
            "program",
            "resolved",
            "cwd",
            "duration-ms",
        ] {
            let probe = format!(
                r#"(if (equal? (alist-get {expr} "{field}" :MISSING) :MISSING) "MISSING" "present")"#
            );
            assert_eq!(
                eval_str_field(&probe),
                "present",
                "{expr} is missing the {field} field"
            );
        }
    }
}

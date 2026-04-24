//! tatara-script — the scripting binary.
//!
//! Usage:
//!   tatara-script path/to/script.tlisp [arg ...]
//!   tatara-script --test path/to/tests.tlisp
//!   tatara-script --repl
//!
//! Reads the .tlisp file, expands macros via tatara-lisp, evaluates each
//! form against a `ScriptCtx` host with the full stdlib (http, json,
//! yaml, sops, toml, file I/O, env, sha256, regex, time, cli, log,
//! encoding, crypto_extra, os, process, string, list) registered.
//!
//! `(require "path.tlisp")` at the top level of a script is handled by
//! this driver — it resolves the path against the current file's dir,
//! reads + parses the target, and evaluates those forms in the same
//! interpreter before continuing. Canonical paths are cached; requiring
//! the same file twice is a no-op.
//!
//! `--test` collects `(deftest NAME BODY...)` forms and runs each
//! in turn, catching errors per test and reporting pass/fail summary.
//!
//! `--repl` drops into an interactive read-eval-print loop using
//! tatara-lisp-eval's ReplSession shape.

use std::path::PathBuf;
use std::process::ExitCode;

use tatara_lisp::{read_spanned, Spanned, SpannedForm};
use tatara_lisp_eval::{Interpreter, Value};
use tatara_lisp_script::{install_stdlib, ScriptCtx};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--repl") => run_repl(args[1..].to_vec()),
        Some("--test") => {
            if let Some(path) = args.get(1) {
                run_test_mode(path, args[2..].to_vec())
            } else {
                eprintln!("usage: tatara-script --test <script.tlisp>");
                ExitCode::from(2)
            }
        }
        Some("--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(path) if path.starts_with("--") => {
            eprintln!("tatara-script: unknown flag {path:?}; see --help");
            ExitCode::from(2)
        }
        Some(path) => run_script(path, args[1..].to_vec()),
        None => {
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    eprintln!(
        "tatara-script — pleme-io Lisp scripting\n\
         \n\
         Usage:\n  \
           tatara-script <script.tlisp> [arg ...]    run a script\n  \
           tatara-script --test <script.tlisp>        collect + run (deftest …) forms\n  \
           tatara-script --repl                       interactive read-eval-print loop\n  \
           tatara-script --help                       this banner\n\
         \n\
         See the tatara-lisp-script crate stdlib docs for the full primitive list."
    );
}

fn run_script(script_path: &str, rest: Vec<String>) -> ExitCode {
    let path = PathBuf::from(script_path);
    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    install_stdlib(&mut interp);

    let mut ctx = ScriptCtx::with_argv(rest);
    ctx.current_file = Some(path.clone());

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tatara-script: cannot read {script_path}: {e}");
            return ExitCode::from(2);
        }
    };
    let forms = match read_spanned(&src) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tatara-script: parse error in {script_path}: {e:?}");
            return ExitCode::from(1);
        }
    };

    match eval_forms_with_require(&mut interp, &forms, &mut ctx, &path) {
        Ok(_) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("tatara-script: {msg}");
            ExitCode::from(1)
        }
    }
}

fn run_test_mode(script_path: &str, rest: Vec<String>) -> ExitCode {
    let path = PathBuf::from(script_path);
    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    install_stdlib(&mut interp);

    let mut ctx = ScriptCtx::with_argv(rest);
    ctx.current_file = Some(path.clone());

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tatara-script --test: cannot read {script_path}: {e}");
            return ExitCode::from(2);
        }
    };
    let forms = match read_spanned(&src) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tatara-script --test: parse error in {script_path}: {e:?}");
            return ExitCode::from(1);
        }
    };

    // Evaluate all top-level forms; (deftest …) is treated as a macro
    // that registers into ctx.tests instead of executing immediately.
    if let Err(msg) = eval_forms_with_require(&mut interp, &forms, &mut ctx, &path) {
        eprintln!("tatara-script --test: top-level error: {msg}");
        return ExitCode::from(1);
    }

    // Drain + run collected tests. Each test body runs via
    // `interp.eval_program` so it sees every global that top-level
    // forms defined (helpers, test fixtures). Tests share a single
    // global env — v1 limitation; isolation comes in a follow-up.
    let tests = std::mem::take(&mut ctx.tests);
    if tests.is_empty() {
        eprintln!("tatara-script --test: no (deftest …) forms found in {script_path}");
        return ExitCode::from(2);
    }
    let total = tests.len();
    let mut passed = 0;
    for test in tests {
        match interp.eval_program(&test.body, &mut ctx) {
            Ok(_) => {
                println!("  ✓ {}", test.name);
                passed += 1;
            }
            Err(e) => {
                eprintln!("  ✘ {} — {e:?}", test.name);
            }
        }
    }
    println!("\n{passed}/{total} passed");
    if passed == total {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn run_repl(_rest: Vec<String>) -> ExitCode {
    use std::io::{BufRead, Write};

    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    install_stdlib(&mut interp);
    let mut ctx = ScriptCtx::with_argv(Vec::<String>::new());

    eprintln!("tatara-script REPL — ^D to exit");
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut buffer = String::new();
    loop {
        buffer.clear();
        print!("λ ");
        stdout.flush().ok();

        // Read forms: keep appending lines until parens balance.
        loop {
            let mut line = String::new();
            let n = match stdin.lock().read_line(&mut line) {
                Ok(n) => n,
                Err(_) => return ExitCode::SUCCESS,
            };
            if n == 0 {
                // ^D
                println!();
                return ExitCode::SUCCESS;
            }
            buffer.push_str(&line);
            if parens_balanced(&buffer) {
                break;
            }
            print!("… ");
            stdout.flush().ok();
        }

        if buffer.trim().is_empty() {
            continue;
        }

        match read_spanned(&buffer) {
            Ok(forms) => {
                for form in &forms {
                    match interp.eval_spanned(form, &mut ctx) {
                        Ok(v) => println!("{}", render_value(&v)),
                        Err(e) => eprintln!("error: {e:?}"),
                    }
                }
            }
            Err(e) => eprintln!("parse error: {e:?}"),
        }
    }
}

/// Evaluate a program with top-level (require) + (deftest) handling.
/// Every other form flows to `interp.eval_spanned` as usual.
fn eval_forms_with_require(
    interp: &mut Interpreter<ScriptCtx>,
    forms: &[Spanned],
    ctx: &mut ScriptCtx,
    current: &std::path::Path,
) -> Result<Value, String> {
    let prior_file = ctx.current_file.replace(current.to_path_buf());
    let mut last = Value::Nil;
    for form in forms {
        last = dispatch_top_form(interp, form, ctx)?;
    }
    ctx.current_file = prior_file;
    Ok(last)
}

fn dispatch_top_form(
    interp: &mut Interpreter<ScriptCtx>,
    form: &Spanned,
    ctx: &mut ScriptCtx,
) -> Result<Value, String> {
    // Peek the head symbol for require / deftest.
    if let SpannedForm::List(items) = &form.form {
        if let Some(head) = items.first().and_then(Spanned::as_symbol) {
            match head {
                "require" => return dispatch_require(interp, items, ctx),
                "deftest" => return dispatch_deftest(items, ctx),
                _ => {}
            }
        }
    }
    interp
        .eval_spanned(form, ctx)
        .map_err(|e| format!("{e:?}"))
}

fn dispatch_require(
    interp: &mut Interpreter<ScriptCtx>,
    items: &[Spanned],
    ctx: &mut ScriptCtx,
) -> Result<Value, String> {
    if items.len() != 2 {
        return Err("require: expected (require \"path.tlisp\")".to_string());
    }
    let target = match &items[1].form {
        SpannedForm::Atom(tatara_lisp::Atom::Str(s)) => s.clone(),
        _ => return Err("require: path must be a string literal".to_string()),
    };
    let Some(path) = tatara_lisp_script::stdlib::module::plan_require(ctx, &target)? else {
        return Ok(Value::Nil); // already required
    };
    let forms = tatara_lisp_script::stdlib::module::read_forms(&path)?;
    eval_forms_with_require(interp, &forms, ctx, &path)
}

fn dispatch_deftest(items: &[Spanned], ctx: &mut ScriptCtx) -> Result<Value, String> {
    if items.len() < 3 {
        return Err("deftest: expected (deftest NAME BODY …)".to_string());
    }
    let name = match &items[1].form {
        SpannedForm::Atom(tatara_lisp::Atom::Str(s)) => s.as_str().to_string(),
        SpannedForm::Atom(tatara_lisp::Atom::Symbol(s)) => s.as_str().to_string(),
        _ => return Err("deftest: NAME must be a string or symbol".to_string()),
    };
    ctx.tests.push(tatara_lisp_script::script_ctx::TestCase {
        name,
        body: items[2..].to_vec(),
    });
    Ok(Value::Nil)
}

fn parens_balanced(s: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escape = false;
    for c in s.chars() {
        if escape {
            escape = false;
            continue;
        }
        if in_str {
            if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ';' => {
                // comment to end of line — skip rest
                break;
            }
            _ => {}
        }
    }
    depth <= 0
}

fn render_value(v: &Value) -> String {
    match v {
        Value::Nil => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Str(s) => format!("{:?}", s.as_ref()),
        Value::Symbol(s) => s.as_ref().to_string(),
        Value::Keyword(s) => format!(":{}", s.as_ref()),
        Value::List(xs) => {
            let parts: Vec<String> = xs.iter().map(render_value).collect();
            format!("({})", parts.join(" "))
        }
        other => format!("{other:?}"),
    }
}

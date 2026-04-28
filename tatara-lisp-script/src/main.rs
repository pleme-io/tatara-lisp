//! tatara-script — the scripting binary.
//!
//! Usage:
//!   tatara-script <path-or-url> [arg ...]
//!   tatara-script --test <path-or-url>
//!   tatara-script --repl
//!
//! `<path-or-url>` accepts any of:
//!     ./local/path.tlisp                                       file
//!     github:owner/repo/path/to/program.tlisp[?ref=v0.1.0]    GitHub
//!     gitlab:owner/repo/path[?ref=main]                        GitLab
//!     codeberg:owner/repo/path[?ref=...]                       Codeberg
//!     https://example.com/program.tlisp[#blake3=hex]           direct + pin
//!
//! See theory/WASM-PACKAGING.md for the URL grammar. URLs are
//! BLAKE3-cached at ~/.cache/tatara/sources so subsequent runs of
//! the same ref skip the network.
//!
//! Reads the source, expands macros via tatara-lisp, evaluates each
//! form against a `ScriptCtx` host with the full stdlib (http,
//! http-server, json, yaml, sops, toml, file I/O, env, sha256,
//! regex, time, cli, log, encoding, crypto_extra, os, process,
//! string, list) registered.
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

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tatara_lisp::{read_spanned, Spanned, SpannedForm};
use tatara_lisp_eval::{Interpreter, Value};
use tatara_lisp_script::{install_stdlib, ScriptCtx};
use tatara_lisp_source::{FileCache, Resolver, Source};

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
           tatara-script <path-or-url> [arg ...]      run a script\n  \
           tatara-script --test <path-or-url>          collect + run (deftest …) forms\n  \
           tatara-script --repl                        interactive read-eval-print loop\n  \
           tatara-script --help                        this banner\n\
         \n\
         <path-or-url> can be:\n  \
           ./local/path.tlisp                           file path\n  \
           github:owner/repo/path/...[?ref=tag]         GitHub source\n  \
           gitlab:owner/repo/path[?ref=main]            GitLab source\n  \
           codeberg:owner/repo/path                     Codeberg source\n  \
           https://example.com/...[#blake3=hex]         direct fetch + optional pin\n\
         \n\
         URLs cache at ~/.cache/tatara/sources keyed by BLAKE3.\n\
         See the tatara-lisp-script crate stdlib docs for the full primitive list."
    );
}

/// Resolve a path-or-URL into (source-text, canonical-path-or-pseudo).
/// For local paths the canonical path is the real filesystem path; for
/// remote URLs we synthesize a deterministic pseudo-path under the
/// cache so `(require ...)` relative resolution still works.
fn resolve_input(input: &str) -> Result<(String, PathBuf), String> {
    let source = Source::parse(input).map_err(|e| format!("parse source {input:?}: {e}"))?;

    // Local paths read directly — no cache, no network.
    if let Source::Local { path } = &source {
        let bytes =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        return Ok((bytes, path.clone()));
    }

    // Remote sources go through the resolver with a file-backed cache.
    let cache_root = dirs_cache_root().join("tatara").join("sources");
    let cache = FileCache::new(&cache_root)
        .map_err(|e| format!("open cache {}: {e}", cache_root.display()))?;
    let mut resolver = Resolver::new(cache);

    let resolved = resolver
        .resolve_source(&source)
        .map_err(|e| format!("{e}"))?;

    let text = String::from_utf8(resolved.bytes).map_err(|e| format!("source not utf-8: {e}"))?;

    // Synthesize a canonical pseudo-path under the cache root so
    // `(require ...)` relative resolution behaves predictably for
    // remote sources too.
    let pseudo = cache_root
        .join("rendered")
        .join(format!("{}.tlisp", resolved.blake3));
    Ok((text, pseudo))
}

fn dirs_cache_root() -> PathBuf {
    if let Ok(s) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(s);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    std::env::temp_dir()
}

/// Configure the interpreter's module loader to read .tlisp files
/// from the script's directory + any `$TATARA_PATH` entries. Called
/// from each entry point (run, --test, --repl) so namespaced
/// `(require "lib/foo" :as f)` works uniformly.
fn install_canonical_loader(interp: &mut Interpreter<ScriptCtx>, script_path: &Path) {
    let base = script_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut search_paths: Vec<PathBuf> = Vec::new();
    if let Ok(extra) = std::env::var("TATARA_PATH") {
        for s in extra.split(':') {
            if !s.is_empty() {
                search_paths.push(PathBuf::from(s));
            }
        }
    }
    let loader = tatara_lisp_eval::FilesystemLoader::new(base).with_search_paths(search_paths);
    interp.set_loader(std::sync::Arc::new(loader));
}

fn run_script(script_path: &str, rest: Vec<String>) -> ExitCode {
    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    install_stdlib(&mut interp);

    let (src, path) = match resolve_input(script_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tatara-script: {e}");
            return ExitCode::from(2);
        }
    };

    install_canonical_loader(&mut interp, &path);

    let mut ctx = ScriptCtx::with_argv(rest);
    ctx.current_file = Some(path.clone());

    let forms = match read_spanned(&src) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tatara-script: parse error in {script_path}: {e:?}");
            return ExitCode::from(1);
        }
    };

    match eval_forms_with_require(&mut interp, &src, &forms, &mut ctx, &path) {
        Ok(_) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("tatara-script: {msg}");
            ExitCode::from(1)
        }
    }
}

fn run_test_mode(script_path: &str, rest: Vec<String>) -> ExitCode {
    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    install_stdlib(&mut interp);

    let (src, path) = match resolve_input(script_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tatara-script --test: {e}");
            return ExitCode::from(2);
        }
    };

    install_canonical_loader(&mut interp, &path);

    let mut ctx = ScriptCtx::with_argv(rest);
    ctx.current_file = Some(path.clone());
    let forms = match read_spanned(&src) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("tatara-script --test: parse error in {script_path}: {e:?}");
            return ExitCode::from(1);
        }
    };

    // Evaluate all top-level forms; (deftest …) is treated as a macro
    // that registers into ctx.tests instead of executing immediately.
    if let Err(msg) = eval_forms_with_require(&mut interp, &src, &forms, &mut ctx, &path) {
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
                // Render the test error against the main src — every test
                // body's spans point back into this file.
                eprintln!("  ✘ {}", test.name);
                for line in e.render(&src).lines() {
                    eprintln!("      {line}");
                }
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
                        Err(e) => eprintln!("{}", e.render(&buffer)),
                    }
                }
            }
            Err(e) => eprintln!("parse error: {e:?}"),
        }
    }
}

/// Evaluate a program with top-level (require) + (deftest) handling.
/// Every other form flows to `interp.eval_spanned` as usual. `src` is the
/// source text backing `forms` (so evaluator errors can be rendered with
/// line/column + source snippet against the caller's own file).
fn eval_forms_with_require(
    interp: &mut Interpreter<ScriptCtx>,
    src: &str,
    forms: &[Spanned],
    ctx: &mut ScriptCtx,
    current: &std::path::Path,
) -> Result<Value, String> {
    let prior_file = ctx.current_file.replace(current.to_path_buf());
    let mut last = Value::Nil;
    for form in forms {
        last = dispatch_top_form(interp, src, form, ctx)?;
    }
    ctx.current_file = prior_file;
    Ok(last)
}

fn dispatch_top_form(
    interp: &mut Interpreter<ScriptCtx>,
    src: &str,
    form: &Spanned,
    ctx: &mut ScriptCtx,
) -> Result<Value, String> {
    // (deftest …) is handled here because it's a script-driver
    // concern (collecting tests for --test mode), not an evaluator
    // concern. Everything else — including (require) and (provide)
    // — flows through eval_top_form so the canonical module system
    // (file=module + qualified names + cycle detection) is what
    // runs. The FilesystemLoader is wired via install_canonical_loader.
    if let SpannedForm::List(items) = &form.form {
        if let Some(head) = items.first().and_then(Spanned::as_symbol) {
            if head == "deftest" {
                return dispatch_deftest(items, ctx);
            }
        }
    }
    interp.eval_top_form(form, ctx).map_err(|e| e.render(src))
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

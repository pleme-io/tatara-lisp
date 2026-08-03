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
    // Reclaim OUR OWN scratch left by processes that died without unwinding.
    //
    // ScratchRegistry's Drop covers normal exit, early return and panic — but
    // NOT SIGKILL, an OOM kill, or power loss. On the host that motivated this
    // work the OOM killer fired three times in six hours, so RAII alone would
    // have left a residue that regrows. This is the reconciler for that path:
    // bounded (512 entries), our prefix only, and only entries hours old, so it
    // can never race a live sibling process. Invariant for the normal case, a
    // sweep for the violent one.
    //
    // Best-effort and deliberately unreported: a script's exit status must
    // describe the script, never its housekeeping.
    let _ = tatara_lisp_script::scratch::sweep_stale();

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
        Some("lint") => run_lint(&args[1..]),
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
           tatara-script lint [path ...]               semantic lint (.tlisp); no paths = walk cwd\n  \
           tatara-script lint --unbound <path>         + flag symbols nothing binds (opt-in; see note)\n  \
           tatara-script lint --shapes                 print the unbound-symbol shape catalog\n  \
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

/// Drop a Unix shebang line so `.tlisp` files can be marked executable and
/// invoked directly (`./script.tlisp`, or as a git hook). The line is REPLACED
/// with an empty one rather than removed, so every subsequent line number still
/// matches the original file.
///
/// Shared by the run path and the lint path deliberately. It used to live only
/// in the runner, which meant `lint` parsed `#!/usr/bin/env tatara-script` as
/// two ordinary symbols. No rule noticed until `unbound-symbol` landed and
/// reported both as unbound on the real deployed `commit-msg` hook — a false
/// positive on the single most important shape tlisp ships in. Any future rule
/// would have inherited the same bug from the same missing call, so the fix
/// belongs here, once, rather than as a shebang special-case inside a rule.
fn strip_shebang(raw: String) -> String {
    if !raw.starts_with("#!") {
        return raw;
    }
    let newline = raw.find('\n').map_or(raw.len(), |i| i + 1);
    let mut s = String::with_capacity(raw.len());
    s.push('\n');
    s.push_str(&raw[newline..]);
    s
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

/// `tatara-script lint [path ...]` — run the semantic rule set over each
/// `.tlisp` file. With no paths, walks the current directory recursively.
/// Prints `path:line:col: [rule] message` per violation; exits non-zero if any
/// violation or unparseable file is found, unless `--warn` downgrades to a
/// warning-only pass.
fn run_lint(args: &[String]) -> ExitCode {
    let warn_only = args.iter().any(|a| a == "--warn");
    let check_unbound = args.iter().any(|a| a == "--unbound");

    // Reflect the unbound-symbol shape catalog. GENERATED from the catalog
    // itself, so the documented coverage cannot drift from the implemented
    // coverage — the alternative is a hand-written list in the help text that
    // goes stale the first time a shape is added.
    if args.iter().any(|a| a == "--shapes") {
        print!("{}", tatara_lisp_lint::rules::CatalogListing);
        return ExitCode::SUCCESS;
    }
    let explicit: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .collect();

    // No paths → walk cwd. Otherwise expand each path: a directory is walked
    // recursively, a file is taken as-is (so `lint .`, `lint src/`, and
    // `lint a.tlisp b.tlisp` all behave intuitively).
    let mut files = Vec::new();
    if explicit.is_empty() {
        collect_tlisp_files(Path::new("."), &mut files);
    } else {
        for path in &explicit {
            if path.is_dir() {
                collect_tlisp_files(path, &mut files);
            } else {
                files.push(path.clone());
            }
        }
    }
    files.sort();
    files.dedup();

    let mut rules = tatara_lisp_lint::default_rules();

    // `unbound-symbol` is appended here rather than living in `default_rules()`
    // because it needs THIS binary's actual environment, and the only honest
    // source for that is a real interpreter with the real stdlib installed —
    // the same `install_stdlib` a script run uses. A table of primitive names
    // kept inside the lint crate would drift the first time a primitive landed,
    // which is the failure shape this repo already paid for once with the
    // Co-Authored-By / Claude-Session trailer pair.
    //
    // Closes a measured gap (2026-08-02): a script calling the nonexistent
    // `string-downcase` linted as "0 violations, 0 parse errors" and failed only
    // at runtime. That matters most for the hook shape — the tatara-script
    // `commit-msg` hook that `blackmatter.components.gitconfig` installs via
    // `core.hooksPath` — where one unbound symbol blocks every commit in every
    // repo on the machine, including the commit that would fix it.
    //
    // ★ STILL OPT-IN (`--unbound`), and the reason is now a MEASURED FLOOR
    // rather than a to-do list. Fleet sweep over 27 `.tlisp` files: 422 → 87
    // after handling the `defmacro` three-part shape, lambda-list keywords,
    // generic `def…` heads, the `fn` / `λ` aliases and `catch`.
    //
    // The residual 87 are one irreducible class: binding forms that are
    // themselves user macros (`dolist`, `when-let`, `if-let`, `with-gensyms`)
    // and macro-DSL data symbols (`:losses nothing`). Which forms bind is decided
    // by a `defmacro` that may live in another file, so no syntactic rule can
    // know. Enumerating more heads here would not close it — the set is open by
    // construction.
    //
    // The fix is to run the rule over MACRO-EXPANDED forms
    // (`Interpreter::fully_expand`, already used by the runtime, so the expansion
    // is authoritative rather than a second guess). That is what would let this
    // go default-on. Until then, default-on would flood every existing script and
    // train operators to ignore lint output — strictly worse than no rule, since
    // it still costs a run. Opt-in keeps it useful for its motivating case
    // (pre-flighting a standalone hook: FP-clean, verified against the live
    // `commit-msg` hook) without that cost.
    if check_unbound {
        let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
        let mut ctx = ScriptCtx::with_argv(Vec::<String>::new());
        install_stdlib(&mut interp, &mut ctx);
        let mut known: Vec<String> = interp
            .reserved_head_names()
            .iter()
            .map(|n| n.to_string())
            .collect();
        known.extend(
            interp
                .globals_snapshot()
                .iter_top_level()
                .into_iter()
                .map(|(name, _)| name.to_string()),
        );
        rules.push(Box::new(tatara_lisp_lint::rules::unbound_symbol(known)));
    }

    let mut violations = 0usize;
    let mut errors = 0usize;
    let mut scanned = 0usize;

    for file in &files {
        let src = match std::fs::read_to_string(file) {
            // Same shebang handling as the run path — an executable hook or
            // script must lint as the interpreter will actually see it.
            Ok(s) => strip_shebang(s),
            Err(e) => {
                eprintln!("tatara-script lint: read {}: {e}", file.display());
                errors += 1;
                continue;
            }
        };
        scanned += 1;
        match tatara_lisp_lint::lint_source(&src, &rules) {
            Ok(found) => {
                for v in found {
                    violations += 1;
                    println!(
                        "{}:{}:{}: [{}] {}",
                        file.display(),
                        v.line,
                        v.col,
                        v.rule,
                        v.message
                    );
                }
            }
            Err(e) => {
                eprintln!("{}: parse error: {e}", file.display());
                errors += 1;
            }
        }
    }

    eprintln!(
        "tatara-script lint: {scanned} file(s) scanned, {violations} violation(s), {errors} parse error(s)"
    );

    if (violations > 0 || errors > 0) && !warn_only {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Recursively collect `*.tlisp` files under `dir`, skipping VCS / build dirs.
fn collect_tlisp_files(dir: &Path, acc: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                ".git" | "target" | "node_modules" | ".direnv"
            ) {
                continue;
            }
            collect_tlisp_files(&path, acc);
        } else if path.extension().is_some_and(|e| e == "tlisp") {
            acc.push(path);
        }
    }
}

fn run_script(script_path: &str, rest: Vec<String>) -> ExitCode {
    let mut interp: Interpreter<ScriptCtx> = Interpreter::new();
    let mut ctx = ScriptCtx::with_argv(rest);
    install_stdlib(&mut interp, &mut ctx);

    let (raw_src, path) = match resolve_input(script_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tatara-script: {e}");
            return ExitCode::from(2);
        }
    };

    install_canonical_loader(&mut interp, &path);
    ctx.current_file = Some(path.clone());

    let src = strip_shebang(raw_src);

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
    let mut ctx = ScriptCtx::with_argv(rest);
    install_stdlib(&mut interp, &mut ctx);

    let (src, path) = match resolve_input(script_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tatara-script --test: {e}");
            return ExitCode::from(2);
        }
    };

    install_canonical_loader(&mut interp, &path);
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
    let mut ctx = ScriptCtx::with_argv(Vec::<String>::new());
    install_stdlib(&mut interp, &mut ctx);

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

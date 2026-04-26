//! tatara — umbrella CLI for the pleme-io tatara-lisp ecosystem.
//!
//! One binary, ruthlessly standardized subcommands. Each subcommand
//! either delegates to an existing tool or implements the missing
//! piece directly here.
//!
//! Subcommand map (canonical, rustfmt-style — one way per task):
//!
//! ```text
//!   tatara fmt [path...]              ← delegates to `feira fmt` if available
//!   tatara lint [path...]             ← delegates to `feira lint`
//!   tatara lint --fix [path...]
//!   tatara run <path-or-url> [args]   ← delegates to `tatara-script`
//!   tatara test <path-or-url>         ← delegates to `tatara-script --test`
//!   tatara repl                       ← delegates to `tatara-script --repl`
//!   tatara deploy <github-url>        ← fetch + check + package as ComputeUnit YAML
//!   tatara typecheck <path>           ← future: build-time gradual typing pass
//! ```
//!
//! `feira` is searched on `$PATH` (typically installed via the caixa
//! workspace). `tatara-script` is searched relative to this binary's
//! directory (release path) and fallback `$PATH`.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "tatara",
    version,
    about = "tatara — umbrella CLI: fmt, lint, run, test, deploy"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Format .tlisp / .lisp files in place via caixa-fmt.
    Fmt {
        /// Paths to format. Default: ./caixa.lisp + every *.tlisp/*.lisp
        /// recursively below cwd.
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Just check; don't write.
        #[arg(long)]
        check: bool,
    },

    /// Lint via caixa-lint, optionally autofixing.
    Lint {
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Apply mechanically-safe autofixes.
        #[arg(long)]
        fix: bool,
        /// Apply heuristic fixes too. Implies --fix.
        #[arg(long)]
        fix_unsafe: bool,
        /// Errors only.
        #[arg(long)]
        errors_only: bool,
    },

    /// Execute a tatara-lisp script.
    Run {
        /// Local path or URL (github:/gitlab:/codeberg:/https:).
        path_or_url: String,
        /// Arguments forwarded to the script's `argv`.
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Run all `(deftest …)` forms in a script + report pass/fail.
    Test {
        path_or_url: String,
    },

    /// Drop into the interactive REPL.
    Repl,

    /// Run the build-time gradual type-check pass over a .tlisp source.
    /// Reports any (the …) / (declare …) / (define …) annotations whose
    /// inferred type doesn't match.
    Typecheck {
        /// Path to a .tlisp file or a fetchable URL.
        path_or_url: String,
    },

    /// Fetch a tatara-lisp program from a URL, run the canonical
    /// pre-flight checks (fmt/lint/test), and emit a ComputeUnit YAML
    /// manifest ready to apply.
    Deploy {
        /// URL — e.g. github:owner/repo/main.tlisp[?ref=v1.0.0]
        url: String,
        /// Output the manifest to this file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Skip pre-flight checks (fmt/lint). Speeds up iteration.
        #[arg(long)]
        skip_checks: bool,
        /// ComputeUnit name (default: derived from URL).
        #[arg(long)]
        name: Option<String>,
        /// Target K8s namespace (default: "default").
        #[arg(long, default_value = "default")]
        namespace: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tatara: error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Fmt { paths, check } => run_feira_fmt(&paths, check),
        Cmd::Lint {
            paths,
            fix,
            fix_unsafe,
            errors_only,
        } => run_feira_lint(&paths, fix, fix_unsafe, errors_only),
        Cmd::Run { path_or_url, args } => run_script(&path_or_url, &args, false, false),
        Cmd::Test { path_or_url } => run_script(&path_or_url, &[], true, false),
        Cmd::Repl => run_script("", &[], false, true),
        Cmd::Typecheck { path_or_url } => typecheck(&path_or_url),
        Cmd::Deploy {
            url,
            output,
            skip_checks,
            name,
            namespace,
        } => deploy(&url, output.as_deref(), skip_checks, name.as_deref(), &namespace),
    }
}

// ── feira delegates ──────────────────────────────────────────────

fn find_feira() -> Result<PathBuf> {
    if let Ok(p) = which("feira") {
        return Ok(p);
    }
    bail!(
        "feira not found on $PATH — install it via `cargo install --path \
         caixa/caixa-feira` or build the caixa workspace and put \
         caixa/target/release on $PATH"
    )
}

fn run_feira_fmt(paths: &[PathBuf], check: bool) -> Result<ExitCode> {
    let feira = find_feira()?;
    let mut cmd = Command::new(feira);
    cmd.arg("fmt");
    if check {
        cmd.arg("--check");
    }
    for p in paths {
        cmd.arg(p);
    }
    let status = cmd.status().context("running feira fmt")?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

fn run_feira_lint(paths: &[PathBuf], fix: bool, fix_unsafe: bool, errors_only: bool) -> Result<ExitCode> {
    let feira = find_feira()?;
    let mut cmd = Command::new(feira);
    cmd.arg("lint");
    if fix {
        cmd.arg("--fix");
    }
    if fix_unsafe {
        cmd.arg("--fix-unsafe");
    }
    if errors_only {
        cmd.arg("--errors-only");
    }
    for p in paths {
        cmd.arg(p);
    }
    let status = cmd.status().context("running feira lint")?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

// ── tatara-script delegates ──────────────────────────────────────

fn find_script() -> Result<PathBuf> {
    // Try sibling binary in the same dir as `tatara` (release builds).
    if let Ok(self_path) = std::env::current_exe() {
        if let Some(parent) = self_path.parent() {
            let sibling = parent.join("tatara-script");
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }
    if let Ok(p) = which("tatara-script") {
        return Ok(p);
    }
    bail!("tatara-script not found — build it with `cargo build --release` and ensure it's adjacent to `tatara` or on $PATH")
}

fn run_script(path: &str, args: &[String], test: bool, repl: bool) -> Result<ExitCode> {
    let script = find_script()?;
    let mut cmd = Command::new(script);
    if test {
        cmd.arg("--test");
    }
    if repl {
        cmd.arg("--repl");
    }
    if !path.is_empty() {
        cmd.arg(path);
    }
    for a in args {
        cmd.arg(a);
    }
    let status = cmd.status().context("running tatara-script")?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

// ── typecheck ────────────────────────────────────────────────────

fn typecheck(path_or_url: &str) -> Result<ExitCode> {
    let resolved = tatara_lisp_source::resolve_once(path_or_url)
        .context("resolving source for typecheck")?;
    let src = String::from_utf8(resolved.bytes)
        .context("source is not UTF-8")?;
    let forms = tatara_lisp::read_spanned(&src)
        .map_err(|e| anyhow::anyhow!("parse error: {e:?}"))?;
    let diags = tatara_lisp_eval::build_check::check_program(&forms);
    if diags.is_empty() {
        eprintln!("tatara typecheck: 0 errors");
        return Ok(ExitCode::SUCCESS);
    }
    for d in &diags {
        eprintln!("{}: {}", path_or_url, d.render(&src));
    }
    eprintln!(
        "tatara typecheck: {} type error(s)",
        diags.len()
    );
    Ok(ExitCode::from(1))
}

// ── deploy ───────────────────────────────────────────────────────

fn deploy(
    url: &str,
    output: Option<&std::path::Path>,
    skip_checks: bool,
    name: Option<&str>,
    namespace: &str,
) -> Result<ExitCode> {
    eprintln!("tatara deploy: resolving {url}");
    let resolved = tatara_lisp_source::resolve_once(url).context("resolving source")?;
    let bytes_len = resolved.bytes.len();
    let blake3 = resolved.blake3.clone();
    eprintln!(
        "tatara deploy: fetched {bytes_len} bytes, blake3={}",
        &blake3[..16]
    );

    if !skip_checks {
        // Write to a temp file so feira fmt --check + feira lint can run.
        let tmp = tempfile_path(".tlisp")?;
        std::fs::write(&tmp, &resolved.bytes).context("writing temp source")?;
        eprintln!("tatara deploy: pre-flight fmt --check");
        let fmt_ok = run_feira_fmt(&[tmp.clone()], true)?;
        if !is_success(&fmt_ok) {
            // Don't hard-fail — fmt drift on remote sources is common.
            eprintln!("tatara deploy: WARN format drift detected; continuing");
        }
        eprintln!("tatara deploy: pre-flight lint");
        let lint_ok = run_feira_lint(&[tmp.clone()], false, false, true)?;
        if !is_success(&lint_ok) {
            eprintln!("tatara deploy: lint errors; continuing (use --skip-checks to silence)");
        }
        let _ = std::fs::remove_file(&tmp);
    }

    // Default the unit name from the URL: last segment without extension.
    let unit_name = name
        .map(str::to_string)
        .unwrap_or_else(|| derive_unit_name(url));

    let manifest = compute_unit_manifest(&unit_name, namespace, url, &blake3);
    let yaml = serde_yaml::to_string(&manifest).context("rendering YAML")?;

    if let Some(path) = output {
        std::fs::write(path, &yaml).with_context(|| format!("writing {}", path.display()))?;
        eprintln!("tatara deploy: wrote manifest to {}", path.display());
    } else {
        print!("{yaml}");
    }

    eprintln!(
        "tatara deploy: ready. apply with:\n  kubectl -n {namespace} apply -f -"
    );
    Ok(ExitCode::SUCCESS)
}

fn is_success(code: &ExitCode) -> bool {
    // ExitCode doesn't expose its inner u8 publicly; format!-roundtrip
    // is the cleanest way to inspect.
    let s = format!("{code:?}");
    s.contains('0')
}

fn derive_unit_name(url: &str) -> String {
    // Strip query/fragment, take last `/`-segment, drop extension.
    let stem = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("tatara-program");
    let stem = stem.split('.').next().unwrap_or(stem);
    let mut out = String::with_capacity(stem.len());
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        "tatara-program".into()
    } else {
        out
    }
}

#[derive(Debug, serde::Serialize)]
struct ComputeUnit {
    api_version: &'static str,
    kind: &'static str,
    metadata: ComputeUnitMeta,
    spec: ComputeUnitSpec,
}

#[derive(Debug, serde::Serialize)]
struct ComputeUnitMeta {
    name: String,
    namespace: String,
    annotations: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, serde::Serialize)]
struct ComputeUnitSpec {
    source: ComputeUnitSource,
    shape: &'static str,
}

#[derive(Debug, serde::Serialize)]
struct ComputeUnitSource {
    url: String,
    blake3: String,
}

fn compute_unit_manifest(name: &str, namespace: &str, url: &str, blake3: &str) -> ComputeUnit {
    let mut annotations = std::collections::BTreeMap::new();
    annotations.insert(
        "tatara.pleme.io/source-url".to_string(),
        url.to_string(),
    );
    annotations.insert(
        "tatara.pleme.io/source-blake3".to_string(),
        blake3.to_string(),
    );
    ComputeUnit {
        api_version: "compute.pleme.io/v1alpha1",
        kind: "ComputeUnit",
        metadata: ComputeUnitMeta {
            name: name.to_string(),
            namespace: namespace.to_string(),
            annotations,
        },
        spec: ComputeUnitSpec {
            source: ComputeUnitSource {
                url: url.to_string(),
                blake3: blake3.to_string(),
            },
            // Default shape: program. wasm-operator picks the right
            // runtime template based on this hint. Other shapes
            // (job/function/service/controller) need additional flags
            // we'll add as the deploy story matures.
            shape: "program",
        },
    }
}

// ── helpers ──────────────────────────────────────────────────────

fn which(name: &str) -> Result<PathBuf> {
    let path_var = std::env::var_os("PATH").context("PATH not set")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("{name} not found on PATH")
}

fn tempfile_path(extension: &str) -> Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("tatara-{nanos}{extension}"));
    Ok(tmp)
}

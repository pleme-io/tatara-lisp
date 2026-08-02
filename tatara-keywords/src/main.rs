//! `tatara-keywords` — census and check the tatara-lisp keyword namespace of a
//! source tree.
//!
//! ```text
//! tatara-keywords census [--tree DIR] [--emit-ledger]
//! tatara-keywords check  [--tree DIR] [--ledger FILE]
//! ```
//!
//! `check` exits 1 on any collision or trespass and prints every one. It is
//! meant to run unconditionally in CI: an opt-in namespace check is how the
//! twenty recorded collisions accumulated in the first place.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tatara_keywords::{collisions, scan, trespasses, Reservas};

const USAGE: &str = "\
tatara-keywords — the tatara-lisp keyword namespace, checked

USAGE:
    tatara-keywords census [--tree DIR] [--emit-ledger] [--measured-on DATE]
                           [--arvore LABEL]
    tatara-keywords check  [--tree DIR] [--ledger FILE] [--repo NAME]

    census        list every `#[tatara(keyword = \"…\")]` in the tree
      --emit-ledger  print the tree's census as a keywords.tlisp ledger
      --measured-on  ISO date to stamp the emitted ledger with (default: today,
                     which this tool cannot read without a clock dependency, so
                     it is REQUIRED when --emit-ledger is given)
      --arvore       label to record as the measured tree (default: --tree's
                     value). Give one when --tree is an absolute local path:
                     the ledger is committed, and an absolute path records
                     whose machine ran the census

    check         exit 1 if two types in the tree claim one keyword, or if the
                  tree claims a keyword the ledger reserves elsewhere

    --tree DIR    root to scan (default: .)
    --ledger FILE reservation ledger (default: <tree>/keywords.tlisp, skipped
                  when absent — an absent ledger disables only the cross-repo
                  half; the intra-tree half always runs)
    --repo NAME   the tree's identity in the ledger's namespace, i.e. the
                  prefix the ledger's paths carry (default: the scanned
                  directory's own name)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("tatara-keywords: {msg}");
            ExitCode::from(2)
        }
    }
}

fn flag_value(args: &[String], name: &str) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("{name} needs a value")),
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let Some(cmd) = args.first() else {
        print!("{USAGE}");
        return Ok(ExitCode::from(2));
    };

    let tree = PathBuf::from(flag_value(args, "--tree")?.unwrap_or_else(|| ".".into()));
    let decls = scan(&tree).map_err(|e| format!("scanning {}: {e}", tree.display()))?;

    match cmd.as_str() {
        "census" => {
            if args.iter().any(|a| a == "--emit-ledger") {
                let measured_on = flag_value(args, "--measured-on")?.ok_or(
                    "--emit-ledger requires --measured-on YYYY-MM-DD: an undated ledger \
                     cannot be told apart from a stale one",
                )?;
                let mut by_keyword: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                for d in &decls {
                    by_keyword
                        .entry(d.keyword.clone())
                        .or_default()
                        .push(d.path.to_string_lossy().into_owned());
                }
                // The tree LABEL, not the scan path. They differ on purpose:
                // the scan path is an absolute path on whoever ran it, and
                // this ledger is committed to a public repo. Recording
                // `/Users/<someone>/…` would put an operator's identity in
                // crates.io output.
                let arvore =
                    flag_value(args, "--arvore")?.unwrap_or_else(|| tree.display().to_string());
                let ledger = Reservas {
                    medido_em: measured_on,
                    arvore,
                    denominador: u32::try_from(decls.len()).unwrap_or(u32::MAX),
                    palavras: by_keyword
                        .into_iter()
                        .map(|(palavra, mut donos)| {
                            donos.sort();
                            donos.dedup();
                            tatara_keywords::Reserva { palavra, donos }
                        })
                        .collect(),
                };
                print!("{}", ledger.to_lisp());
            } else {
                for d in &decls {
                    println!("{}\t{}:{}", d.keyword, d.path.display(), d.line);
                }
                eprintln!(
                    "tatara-keywords census: {} declaration(s) over {}",
                    decls.len(),
                    tree.display()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        "check" => {
            // Calibration, not decoration. A scan that found nothing is
            // reported as UNKNOWABLE rather than clean: the overwhelmingly
            // likelier cause of a zero here is a mis-pointed --tree or an
            // ignore rule, and a check that greens on a tree it never read is
            // the vacuous guard this whole exercise exists to remove.
            if decls.is_empty() {
                eprintln!(
                    "tatara-keywords check: 0 declarations found under {} — \
                     refusing to report a keyword namespace clean when the scan \
                     found nothing to check. Point --tree at a tree containing \
                     `#[tatara(keyword = \"…\")]`, or drop this step.",
                    tree.display()
                );
                return Ok(ExitCode::from(2));
            }

            let mut bad = 0usize;

            let found = collisions(&decls);
            for c in &found {
                eprint!("tatara-keywords: {c}");
                bad += 1;
            }

            let ledger_path = flag_value(args, "--ledger")?
                .map(PathBuf::from)
                .unwrap_or_else(|| tree.join("keywords.tlisp"));
            if ledger_path.exists() {
                let ledger = load_ledger(&ledger_path)?;
                // The tree's identity in the ledger's namespace. Defaults to
                // the scanned directory's own name, which is what a CI
                // checkout of `pleme-io/<repo>` gives.
                let repo = flag_value(args, "--repo")?.unwrap_or_else(|| {
                    tree.canonicalize()
                        .ok()
                        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .unwrap_or_default()
                });
                for t in trespasses(&decls, &ledger, &repo) {
                    eprintln!("tatara-keywords: {t}");
                    bad += 1;
                }
                eprintln!(
                    "tatara-keywords check: {} declaration(s) over {}, against {} \
                     reservation(s) measured {} — {} violation(s)",
                    decls.len(),
                    tree.display(),
                    ledger.palavras.len(),
                    ledger.medido_em,
                    bad
                );
            } else {
                eprintln!(
                    "tatara-keywords check: {} declaration(s) over {}, NO ledger at {} \
                     (cross-repo half not run) — {} violation(s)",
                    decls.len(),
                    tree.display(),
                    ledger_path.display(),
                    bad
                );
            }

            Ok(if bad == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

fn load_ledger(path: &Path) -> Result<Reservas, String> {
    let src =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Reservas::from_lisp(&src).map_err(|e| format!("parsing {}: {e}", path.display()))
}

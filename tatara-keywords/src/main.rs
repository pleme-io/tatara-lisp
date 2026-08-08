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

use tatara_keywords::{collisions, names, scan, trespasses, Reservas};

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

    names  --root DIR [--max-phantom-repos N]
                  the SECOND namespace: pleme-io primitive names, gathered
                  from repo directories, theory/ filenames, VOCABULARY.md rows
                  and NAMING.md family columns. Same collision engine.

                  GATES on the three rules a machine can decide with no false
                  positives: a word both claimed and advertised in a family's
                  Room-to-grow column; one word with two `## Naming` rows; a
                  doc citing `pleme-io/<name>` where no such repo exists.
      --max-phantom-repos N
                  cap the third rule instead of demanding zero — a plan doc
                  may legitimately name repos it proposes to create. Below the
                  cap the tool says so and asks for it to be lowered, because
                  a ratchet that never tightens is a cap nobody keeps.

                  REPORTS, never gates, the two that need a judgement call —
                  which theory docs lack a naming row, and which named things
                  have nothing behind them. Telling a minted primitive from a
                  stated concept is not decidable without classifying words,
                  so it stays with the human at step 6.

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

    // Dispatched before the keyword scan on purpose: `names` reads directory
    // entries and two markdown files, while `scan` walks every .rs file under
    // --tree. Pointing the latter at an org root is minutes of IO for a result
    // this command never looks at.
    if cmd == "names" {
        return run_names(args);
    }

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

/// Render a long advisory list without burying the gating verdict under it.
///
/// The count is always exact and always printed by the caller; only the
/// enumeration is elided, and the elision says how much it dropped — a list
/// silently cut to its first dozen reads as complete.
fn elide(items: &[String], verbose: bool) -> String {
    const SHOWN: usize = 12;
    if verbose || items.len() <= SHOWN {
        return items.join(" ");
    }
    format!(
        "{} … and {} more (--verbose for all)",
        items[..SHOWN].join(" "),
        items.len() - SHOWN
    )
}

/// The `names` arm — the primitive-name namespace.
///
/// Three tiers, stated in the output rather than blurred together:
/// **gating** (a word both claimed and advertised free; one word with two
/// registry rows), **ratcheted** (unregistered count, capped by flag), and
/// **advisory** (phantoms — a registered word with no doc and no repo, which
/// a human must classify because a doctrine legitimately has neither).
fn run_names(args: &[String]) -> Result<ExitCode, String> {
    let root = PathBuf::from(flag_value(args, "--root")?.unwrap_or_else(|| ".".into()));
    let corpus =
        names::scan_corpus(&root).map_err(|e| format!("scanning {}: {e}", root.display()))?;

    // Same vacuity refusal as `check`. A names sweep that read nothing is the
    // exact failure this tool exists to replace — the fleet-root `.gitignore`
    // is `*`, and the sweep it broke printed a clean zero.
    if corpus.sightings.is_empty() {
        eprintln!(
            "tatara-keywords names: 0 names found under {} — refusing to report a \
             name namespace clean when the scan found nothing. Point --root at an \
             org checkout containing repo directories and a theory/ directory.",
            root.display()
        );
        return Ok(ExitCode::from(2));
    }

    let mut bad = 0usize;

    // GATING 1 — the check that would have caught the 2026-08-08 rename.
    for s in names::stale_reservations(&corpus) {
        eprintln!(
            "tatara-keywords names: `{}` is advertised as untaken in a NAMING.md \
             Room-to-grow column but is already claimed by {:?} — move it into \
             Members, or the next minter gets the same false all-clear",
            s.word, s.claimed_by
        );
        bad += 1;
    }

    // GATING 2 — one word, two rows in the `## Naming` registry.
    if let Ok(src) = std::fs::read_to_string(root.join("theory").join("VOCABULARY.md")) {
        let mut seen: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (r, surface) in names::names_in_vocabulary(&src) {
            if surface == names::Surface::Registry {
                seen.entry(names::fold(&r)).or_default().push(r);
            }
        }
        for (folded, spellings) in seen.iter().filter(|(_, v)| v.len() > 1) {
            eprintln!(
                "tatara-keywords names: `{folded}` has {} `## Naming` rows ({}) — \
                 one word, one row; two rows are two glosses free to disagree",
                spellings.len(),
                spellings.join(", ")
            );
            bad += 1;
        }
    }

    // GATING 3 — a doc cites `pleme-io/<name>` and no such repo exists.
    //
    // Ratcheted rather than absolute, because a *plan* document legitimately
    // names repos it proposes to create: five of the thirty-eight findings on
    // 2026-08-08 are MESH-EXECUTION-PLAN.md's. The count is exact and
    // decidable, so a cap is sound here in a way it is not for `unregistered`
    // — pass today's number, then lower it as docs are corrected.
    //
    // The rule needs an ORG checkout to answer "does this repo exist", and a
    // CI job that checked out `theory` alone would find no sibling repos and
    // flag every citation. So the oracle is calibrated first: below a floor of
    // sibling repos it is not trusted, and the rule reports as UNEVALUATED
    // rather than clean or red. Asking for a cap the run cannot honour is an
    // error, not a silent pass — that is the whole vacuous-guard failure mode.
    const ORG_FLOOR: usize = 50;
    let repo_count = corpus
        .sightings
        .values()
        .filter(|s| s.iter().any(|(sf, _)| *sf == names::Surface::Repo))
        .count();
    let cap = flag_value(args, "--max-phantom-repos")?
        .map(|v| v.parse::<usize>().map_err(|e| format!("--max-phantom-repos: {e}")))
        .transpose()?;
    if repo_count < ORG_FLOOR {
        if cap.is_some() {
            return Err(format!(
                "--max-phantom-repos was given, but only {repo_count} sibling repo(s) \
                 exist under {} (floor {ORG_FLOOR}). The phantom-repo rule needs a \
                 full org checkout to tell a missing repo from an unchecked-out one; \
                 running it here would flag every citation. Drop the flag to skip \
                 the rule, or point --root at an org checkout.",
                root.display()
            ));
        }
        eprintln!(
            "tatara-keywords names: phantom-repo rule UNEVALUATED — {repo_count} \
             sibling repo(s) under {} is below the floor of {ORG_FLOOR}, so \
             \"no such directory\" would not mean \"no such repo\"",
            root.display()
        );
        return finish_names(&corpus, &root, bad, args);
    }

    // GATING 3a — a dead LINK. Zero tolerance: unlike a bare prose mention,
    // a `github.com/pleme-io/<name>` URL either resolves or 404s, so there is
    // no proposal reading of it. Code fences are excluded by the scanner.
    for (name, doc) in names::dead_links(&root)
        .map_err(|e| format!("reading theory/ under {}: {e}", root.display()))?
    {
        eprintln!(
            "tatara-keywords names: `{doc}` LINKS to \
             https://github.com/pleme-io/{name}, which does not exist — a dead \
             link, not a proposal"
        );
        bad += 1;
    }

    let phantom = names::phantom_repos(&root)
        .map_err(|e| format!("reading theory/ under {}: {e}", root.display()))?;
    for (name, doc) in &phantom {
        eprintln!(
            "tatara-keywords names: `{doc}` cites `pleme-io/{name}` as a repo, and \
             no such directory exists — the citation mints a primitive by \
             implication and burns the word while protecting nothing"
        );
    }
    match cap {
        Some(n) if phantom.len() > n => {
            eprintln!(
                "tatara-keywords names: {} phantom repo citation(s), over the stated \
                 cap of {n}",
                phantom.len()
            );
            bad += 1;
        }
        // A stated cap the corpus has fallen below is a cap to LOWER, not a
        // quiet pass. Silence here is how a ratchet stops ratcheting.
        Some(n) if phantom.len() < n => eprintln!(
            "tatara-keywords names: {} phantom repo citation(s), under the stated \
             cap of {n} — lower --max-phantom-repos to {} to hold the ground",
            phantom.len(),
            phantom.len()
        ),
        None if !phantom.is_empty() => eprintln!(
            "tatara-keywords names: {} phantom repo citation(s), cap UNSTATED so \
             not gating — pass --max-phantom-repos {} to freeze the count",
            phantom.len(),
            phantom.len()
        ),
        _ => {}
    }

    // ADVISORY — the two rules that need a judgement no lint can make.
    finish_names(&corpus, &root, bad, args)
}

/// The two advisories plus the verdict line, shared by both exit paths.
///
/// Factored out precisely so the UNEVALUATED path cannot quietly print a
/// different, rosier summary than the fully-evaluated one.
fn finish_names(
    corpus: &names::Corpus,
    root: &Path,
    bad: usize,
    args: &[String],
) -> Result<ExitCode, String> {
    let verbose = args.iter().any(|a| a == "--verbose");
    let unreg = names::unregistered(corpus);
    if !unreg.is_empty() {
        eprintln!(
            "tatara-keywords names: ADVISORY — {} theory doc(s) with no `## Naming` \
             row: {}",
            unreg.len(),
            elide(&unreg, verbose)
        );
    }
    let ph = names::phantoms(corpus);
    if !ph.is_empty() {
        eprintln!(
            "tatara-keywords names: ADVISORY — {} registered/family name(s) with no \
             doc, repo or glossary term: {}",
            ph.len(),
            elide(&ph, verbose)
        );
    }

    let registered = corpus
        .sightings
        .values()
        .filter(|s| s.iter().any(|(sf, _)| *sf == names::Surface::Registry))
        .count();
    eprintln!(
        "tatara-keywords names: {} word(s) claimed over {}, {registered} with a \
         `## Naming` row, {} theory doc(s) without one (ADVISORY: the denominator \
         is docs, not minted names — no lint can tell those apart) — \
         {bad} gating violation(s)",
        corpus.sightings.len(),
        root.display(),
        unreg.len(),
    );

    Ok(if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn load_ledger(path: &Path) -> Result<Reservas, String> {
    let src =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Reservas::from_lisp(&src).map_err(|e| format!("parsing {}: {e}", path.display()))
}

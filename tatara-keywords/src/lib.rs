//! `tatara-keywords` — the tatara-lisp keyword namespace, checked.
//!
//! # Why this crate exists
//!
//! `TataraDomain::KEYWORD` is an unnamespaced `&'static str`. Two structs in
//! two crates may declare `#[tatara(keyword = "defplugin")]` and nothing
//! anywhere notices — not the derive (it sees one struct), not `cargo`, not
//! CI. [`tatara_lisp::domain::register`] now refuses the second claimant, but
//! it can only see what one linked process contains: if the two crates never
//! end up in one binary, no `register` call ever meets the other, and the
//! divergence grows unobserved until something links them.
//!
//! This crate is the other half. It reads *source*, so it sees a collision
//! that no process would.
//!
//! # The two checks
//!
//! 1. **Intra-tree.** Two declarations of one keyword by different types
//!    inside the scanned tree. Always wrong; nothing to configure.
//! 2. **Against the ledger.** A keyword this tree declares that
//!    [`keywords.tlisp`](../keywords.tlisp) records as reserved by a *different*
//!    owner. This is the cross-repo case, and it is the one that produced the
//!    twenty live collisions the ledger records.
//!
//! # What the ledger is, and what it is not
//!
//! It is a **dated snapshot of one checkout**, authored as tatara-lisp and
//! parsed by tatara-lisp's own derive. It carries its own measurement date,
//! the tree it was taken over, and the denominator, because a coverage claim
//! without those rots silently and downward — a stale ledger under-reports,
//! and under-reporting reads as modesty rather than as error. It is NOT a
//! guarantee that the fleet holds no other keyword. Re-measure with
//! `tatara-keywords census --emit-ledger` before quoting it.

pub mod names;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// One `#[tatara(keyword = "…")]` found in a source tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Declaration {
    /// The claimed keyword, without parentheses — `"defcaixa"`.
    pub keyword: String,
    /// Path to the `.rs` file, relative to the scan root.
    pub path: PathBuf,
    /// 1-based line of the attribute.
    pub line: usize,
}

/// Two or more declarations of one keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub keyword: String,
    pub declarations: Vec<Declaration>,
}

impl fmt::Display for Collision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "keyword `{}` is declared {} times:",
            self.keyword,
            self.declarations.len()
        )?;
        for d in &self.declarations {
            writeln!(f, "    {}:{}", d.path.display(), d.line)?;
        }
        Ok(())
    }
}

/// A keyword that this tree declares and the ledger reserves to someone else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trespass {
    pub keyword: String,
    /// Where this tree declares it.
    pub here: Declaration,
    /// Who the ledger says holds it.
    pub reserved_to: Vec<String>,
}

impl fmt::Display for Trespass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "keyword `{}` declared at {}:{} is reserved to {}",
            self.keyword,
            self.here.path.display(),
            self.here.line,
            self.reserved_to.join(", ")
        )
    }
}

// ── The ledger, authored as tatara-lisp ────────────────────────────

/// One reserved keyword and every source path measured declaring it.
///
/// A `:donos` list longer than one is a RECORDED collision — a row that is
/// already wrong on the day it was written, kept visible rather than rounded
/// away. `check` never treats a recorded collision as permission: a tree that
/// re-declares such a keyword still trespasses.
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defreserva")]
pub struct Reserva {
    /// The reserved keyword, e.g. `"defcaixa"`.
    pub palavra: String,
    /// Every source path measured declaring it, relative to the tree root.
    #[serde(default)]
    pub donos: Vec<String>,
}

/// The whole reservation table, with the provenance that keeps it honest.
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defreservas")]
pub struct Reservas {
    /// ISO date the census was taken. A ledger without one is unusable: the
    /// reader cannot tell modesty from staleness.
    pub medido_em: String,
    /// The tree the census was taken over.
    pub arvore: String,
    /// How many declarations were counted — the denominator behind the row
    /// count, so a shrunken ledger is visibly a shrunken measurement rather
    /// than a shrunken fleet.
    pub denominador: u32,
    /// One row per reserved keyword.
    #[serde(default)]
    pub palavras: Vec<Reserva>,
}

impl Reservas {
    /// Parse a ledger from tatara-lisp source.
    pub fn from_lisp(src: &str) -> Result<Self, tatara_lisp::LispError> {
        use tatara_lisp::domain::TataraDomain;
        let forms = tatara_lisp::read(src)?;
        let first = forms
            .first()
            .ok_or_else(|| tatara_lisp::LispError::Compile {
                form: "defreservas".into(),
                message: "empty ledger".into(),
            })?;
        Self::compile_from_sexp(first)
    }

    /// Keyword → owners, for lookup.
    #[must_use]
    pub fn index(&self) -> BTreeMap<&str, &[String]> {
        self.palavras
            .iter()
            .map(|r| (r.palavra.as_str(), r.donos.as_slice()))
            .collect()
    }

    /// Render back to tatara-lisp source. Round-trips `from_lisp`.
    #[must_use]
    pub fn to_lisp(&self) -> String {
        let mut out = String::new();
        out.push_str(
            ";; keywords.tlisp — the tatara-lisp keyword reservation ledger.\n\
             ;;\n\
             ;; GENERATED by `tatara-keywords census --emit-ledger`. Re-measure\n\
             ;; rather than hand-edit: a hand-added row asserts a fact nobody took.\n\
             ;;\n\
             ;; A `:donos` list with more than one entry is a collision that already\n\
             ;; existed when the census ran. It is recorded, not blessed — `check`\n\
             ;; still refuses a tree that adds another claimant.\n\n",
        );
        out.push_str("(defreservas\n");
        out.push_str(&lisp_kv("  :medido-em", &self.medido_em));
        out.push_str(&lisp_kv("  :arvore", &self.arvore));
        out.push_str("  :denominador ");
        out.push_str(&self.denominador.to_string());
        out.push('\n');
        out.push_str("  :palavras (\n");
        for r in &self.palavras {
            out.push_str("    (:palavra ");
            out.push_str(&quote(&r.palavra));
            out.push_str(" :donos (");
            for (i, d) in r.donos.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(&quote(d));
            }
            out.push_str("))\n");
        }
        out.push_str("  ))\n");
        out
    }
}

fn lisp_kv(key: &str, value: &str) -> String {
    let mut s = String::from(key);
    s.push(' ');
    s.push_str(&quote(value));
    s.push('\n');
    s
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── Scanning ───────────────────────────────────────────────────────

/// Walk `root` for `.rs` files and collect every `#[tatara(keyword = "…")]`.
///
/// Skips `target/`, `.git/`, and any directory named `vendor` — build output
/// and vendored third-party trees are not this repo's namespace claims, and
/// counting them would make the census report collisions between a crate and
/// its own build artifact.
///
/// # Errors
/// Propagates any filesystem error from walking or reading the tree. A scan
/// that swallowed an unreadable directory would report a zero it did not earn.
pub fn scan(root: &Path) -> std::io::Result<Vec<Declaration>> {
    let mut found = Vec::new();
    walk(root, root, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Declaration>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if name == "target" || name == ".git" || name == "vendor" || name == "node_modules" {
                continue;
            }
            walk(root, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            let src = std::fs::read_to_string(&path)?;
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            for (idx, line) in src.lines().enumerate() {
                if let Some(kw) = keyword_in_attribute(line) {
                    out.push(Declaration {
                        keyword: kw,
                        path: rel.clone(),
                        line: idx + 1,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Extract the keyword from a `#[tatara(keyword = "…")]` attribute line.
///
/// Deliberately literal rather than a full attribute parse: the attribute is
/// emitted by the derive's own documented spelling, and a scanner that tried to
/// be clever here would start finding `keyword = "…"` in unrelated attributes.
/// The one shape it must not miss is whitespace variation around `(` and `=`.
#[must_use]
pub fn keyword_in_attribute(line: &str) -> Option<String> {
    let after_hash = line.trim_start().strip_prefix("#[")?;
    let rest = after_hash.trim_start().strip_prefix("tatara")?;
    let rest = rest.trim_start().strip_prefix('(')?;
    let rest = rest.trim_start().strip_prefix("keyword")?;
    let rest = rest.trim_start().strip_prefix('=')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Group declarations by keyword and return every keyword claimed by more than
/// one distinct file.
///
/// Two declarations at different lines of the SAME file are not a collision:
/// that is how a `#[cfg]`-gated pair or a doc example reads on disk, and
/// neither can produce two competing `register` calls for one type.
#[must_use]
pub fn collisions(decls: &[Declaration]) -> Vec<Collision> {
    let mut by_keyword: BTreeMap<&str, Vec<&Declaration>> = BTreeMap::new();
    for d in decls {
        by_keyword.entry(d.keyword.as_str()).or_default().push(d);
    }
    by_keyword
        .into_iter()
        .filter_map(|(kw, ds)| {
            let mut files: Vec<&PathBuf> = ds.iter().map(|d| &d.path).collect();
            files.sort();
            files.dedup();
            if files.len() > 1 {
                Some(Collision {
                    keyword: kw.to_string(),
                    declarations: ds.into_iter().cloned().collect(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Every keyword this tree declares that the ledger reserves to a path outside
/// the tree.
///
/// `repo` is the tree's identity in the ledger's namespace — the ledger records
/// `caixa/caixa-core/src/manifest.rs` because it was measured over the whole
/// org checkout, while a scan of the caixa repo alone yields
/// `caixa-core/src/manifest.rs`. A declaration is this tree's own iff some
/// `:donos` entry equals `<repo>/<path>` or `<path>` exactly.
///
/// Exact, not suffix. Suffix matching looks right and is wrong the moment two
/// repos both have `src/manifest.rs` — which is common enough that the first
/// draft of this function reported a real trespass as self-ownership, caught by
/// [`tests::declaring_a_keyword_another_repo_reserves_is_a_trespass`].
#[must_use]
pub fn trespasses(decls: &[Declaration], ledger: &Reservas, repo: &str) -> Vec<Trespass> {
    let index = ledger.index();
    let mut out = Vec::new();
    for d in decls {
        let Some(owners) = index.get(d.keyword.as_str()) else {
            continue;
        };
        let here = d.path.to_string_lossy().replace('\\', "/");
        let qualified = format!("{repo}/{here}");
        let mine = owners.iter().any(|o| o == &here || o == &qualified);
        if !mine {
            out.push(Trespass {
                keyword: d.keyword.clone(),
                here: d.clone(),
                reserved_to: (*owners).to_vec(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_shapes_the_derive_actually_emits_are_all_recognised() {
        assert_eq!(
            keyword_in_attribute(r#"#[tatara(keyword = "defcaixa")]"#).as_deref(),
            Some("defcaixa")
        );
        assert_eq!(
            keyword_in_attribute(r#"    #[tatara(keyword="defplugin")]"#).as_deref(),
            Some("defplugin")
        );
        assert_eq!(
            keyword_in_attribute(r#"#[ tatara ( keyword = "defenv" ) ]"#).as_deref(),
            Some("defenv")
        );
    }

    #[test]
    fn unrelated_attributes_are_not_keyword_declarations() {
        // The failure mode this guards: a scanner loose enough to match any
        // `keyword = "…"` would count serde and clap attributes as namespace
        // claims and report collisions that do not exist.
        assert_eq!(keyword_in_attribute(r#"#[serde(rename = "nome")]"#), None);
        assert_eq!(keyword_in_attribute(r#"#[clap(keyword = "x")]"#), None);
        assert_eq!(keyword_in_attribute(r#"// #[tatara(keyword = "x")]"#), None);
        assert_eq!(keyword_in_attribute(r#"let keyword = "defcaixa";"#), None);
    }

    fn decl(kw: &str, path: &str) -> Declaration {
        Declaration {
            keyword: kw.into(),
            path: PathBuf::from(path),
            line: 1,
        }
    }

    #[test]
    fn two_files_claiming_one_keyword_is_a_collision() {
        let decls = vec![
            decl("defplugin", "escriba-config/src/lib.rs"),
            decl("defplugin", "escriba-lisp/src/plugin.rs"),
            decl("defcaixa", "caixa-core/src/manifest.rs"),
        ];
        let found = collisions(&decls);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keyword, "defplugin");
        assert_eq!(found[0].declarations.len(), 2);
    }

    #[test]
    fn one_file_declaring_a_keyword_twice_is_not_a_collision() {
        let decls = vec![
            Declaration {
                keyword: "deffoo".into(),
                path: PathBuf::from("a/src/lib.rs"),
                line: 10,
            },
            Declaration {
                keyword: "deffoo".into(),
                path: PathBuf::from("a/src/lib.rs"),
                line: 90,
            },
        ];
        assert!(collisions(&decls).is_empty());
    }

    fn ledger() -> Reservas {
        Reservas {
            medido_em: "2026-07-31".into(),
            arvore: "~/code/github/pleme-io".into(),
            denominador: 2,
            palavras: vec![Reserva {
                palavra: "defcaixa".into(),
                donos: vec!["caixa/caixa-core/src/manifest.rs".into()],
            }],
        }
    }

    #[test]
    fn declaring_a_keyword_another_repo_reserves_is_a_trespass() {
        // `escriba/src/manifest.rs` shares its tail with the ledger's
        // `caixa/caixa-core/src/manifest.rs`. Under the suffix match this
        // function first shipped with, that read as self-ownership and the
        // trespass vanished.
        let decls = vec![decl("defcaixa", "src/manifest.rs")];
        let found = trespasses(&decls, &ledger(), "escriba");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keyword, "defcaixa");
    }

    #[test]
    fn the_reserving_tree_does_not_trespass_on_itself() {
        let decls = vec![decl("defcaixa", "caixa-core/src/manifest.rs")];
        assert!(trespasses(&decls, &ledger(), "caixa").is_empty());
    }

    #[test]
    fn a_keyword_absent_from_the_ledger_is_free() {
        let decls = vec![decl("defmolde", "src/lib.rs")];
        assert!(trespasses(&decls, &ledger(), "pleme-doc-gen").is_empty());
    }

    #[test]
    fn the_ledger_round_trips_through_tatara_lisp() {
        // The ledger is authored in the language this repo ships. If the
        // round-trip breaks, the ledger stops being readable by the tool that
        // enforces it, which is exactly the split-brain this crate exists to
        // prevent — so it is pinned rather than assumed.
        let original = Reservas {
            medido_em: "2026-07-31".into(),
            arvore: "~/code/github/pleme-io".into(),
            denominador: 349,
            palavras: vec![
                Reserva {
                    palavra: "defcaixa".into(),
                    donos: vec!["caixa/caixa-core/src/manifest.rs".into()],
                },
                Reserva {
                    palavra: "defplugin".into(),
                    donos: vec![
                        "escriba/escriba-config/src/lib.rs".into(),
                        "escriba/escriba-lisp/src/plugin.rs".into(),
                        "kura/kura-core/src/plugin.rs".into(),
                    ],
                },
            ],
        };
        let parsed = Reservas::from_lisp(&original.to_lisp()).expect("ledger must re-read");
        assert_eq!(parsed, original);
    }
}

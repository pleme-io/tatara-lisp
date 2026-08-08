//! The PRIMITIVE-NAME namespace — the same census, one level up.
//!
//! # Why this module is here and not in a new crate
//!
//! This crate's engine was never keyword-specific and nobody had noticed.
//! [`collisions`](crate::collisions) and [`trespasses`](crate::trespasses) read
//! exactly one field of a [`Declaration`](crate::Declaration) — the claimed
//! word. Everything else about them is namespace-agnostic. The only
//! keyword-shaped code in the crate is [`scan`](crate::scan), which greps
//! `#[tatara(keyword = "…")]` out of `.rs` files.
//!
//! So a second namespace costs a scanner, not a tool. This module is that
//! scanner, for the namespace one level above `(def…)` keywords: the fleet's
//! **primitive names** — `eclusa`, `magma`, `bancada`, `ceu`.
//!
//! # The failure that produced it (measured 2026-08-08)
//!
//! `bancada` was minted as a node name while already registered to banken's
//! `(defbancada)` session domain. Three independent surfaces each said "free":
//!
//! 1. **`VOCABULARY.md` is not the index.** 34 of 82 single-token minted names
//!    had a row — 41%. A clean grep of the registry proves nothing, because the
//!    registry is a lagging subset of the corpus.
//! 2. **`NAMING.md` said the word was UNTAKEN.** It sat in Craft/Making 匠's
//!    *Room to grow* column while `VOCABULARY.md` registered it. Two surfaces
//!    disagreed, and the one an author consults when picking a word was the
//!    wrong one. [`stale_reservations`] is that check.
//! 3. **The obvious sweep returns a silent zero.** `~/code/github/pleme-io` is
//!    itself a git repo whose `.gitignore` is `*` with a handful of exceptions,
//!    so `rg` at the fleet root reads those few files rather than ~992 repos —
//!    and prints nothing, which is indistinguishable from "free". This scanner
//!    walks the tree directly and never consults an ignore file.
//!
//! # What a machine can and cannot decide
//!
//! Gateable, because they are set arithmetic: a name claimed twice; a doc with
//! no registry row; a registry row naming nothing; a reservation that is
//! already taken.
//!
//! **Not gateable: adjacent SENSE.** `bancada`-the-machine and
//! `bancada`-the-session collided on *meaning*, and no lint proves two glosses
//! differ without ML, which is doctrine-banned. That judgement stays with the
//! human at the naming skill's step 6. This module deliberately does not
//! pretend otherwise.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Declaration;

/// Fold a word to its comparison key: lowercase, diacritics stripped.
///
/// Load-bearing, not cosmetic. The fleet's own surfaces disagree on accents —
/// the registry row is `alfândega` while the file is `ALFANDEGA.md`, and
/// `mutirao` finds 2 mentions where `mutirão` finds 12. A comparison that
/// respects diacritics reports two free words where one taken word exists.
#[must_use]
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'Á' | 'À' | 'Â' | 'Ã' | 'Ä' => vec!['a'],
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => vec!['e'],
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => vec!['i'],
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' => vec!['o'],
            'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => vec!['u'],
            'ç' | 'Ç' => vec!['c'],
            'ñ' | 'Ñ' => vec!['n'],
            other => other.to_lowercase().collect(),
        })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

/// Where a name was found. The provenance that makes a verdict checkable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Surface {
    /// A directory under the org root — a repo, whether or not any doc knows.
    Repo,
    /// A `theory/<NAME>.md` filename. Often the real name-of-record.
    TheoryDoc,
    /// A per-word gloss row in `VOCABULARY.md`'s **`## Naming`** section — the
    /// name-of-record. This, and only this, is what "registered" means.
    Registry,
    /// A bolded row in any OTHER `VOCABULARY.md` section.
    ///
    /// Measured 2026-08-08: 336 bolded rows, of which 40 are in `## Naming`.
    /// The other ~296 are types, concepts and English terms (`HelmRelease`,
    /// `typescape`, `spot`). They claim the word — so they block a minter —
    /// but they are not a name registration, and conflating the two is what
    /// made the first run of this tool report 209 "registered" names.
    Term,
    /// A word listed as a MEMBER of a family in `NAMING.md`.
    FamilyMember,
    /// A word parked in a family's *Room to grow* column. RESERVED, not free.
    RoomToGrow,
}

impl Surface {
    /// Does presence here mean the word is claimed by a real thing?
    ///
    /// `RoomToGrow` is the interesting `false`: it is a promise about the
    /// future, so it must block a *minter* without counting as a *thing*.
    #[must_use]
    pub const fn is_a_thing(self) -> bool {
        !matches!(self, Self::RoomToGrow)
    }
}

/// Everything the corpus says about names, gathered once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Corpus {
    /// Folded word → the surfaces it appears on, with raw spelling preserved.
    pub sightings: BTreeMap<String, BTreeSet<(Surface, String)>>,
}

impl Corpus {
    /// Record one sighting.
    pub fn see(&mut self, word: &str, surface: Surface, raw: &str) {
        self.sightings
            .entry(fold(word))
            .or_default()
            .insert((surface, raw.to_string()));
    }

    /// Surfaces a folded word appears on.
    #[must_use]
    pub fn surfaces(&self, word: &str) -> BTreeSet<Surface> {
        self.sightings
            .get(&fold(word))
            .map(|s| s.iter().map(|(sf, _)| *sf).collect())
            .unwrap_or_default()
    }

    /// Is this word free to mint? The question step 4 actually asks.
    ///
    /// A word parked in *Room to grow* is NOT free — that is the whole point of
    /// the column, and treating it as free is how a family's growth reserve
    /// gets spent on an unrelated primitive.
    #[must_use]
    pub fn is_free(&self, word: &str) -> bool {
        !self.sightings.contains_key(&fold(word))
    }

    /// Every declaration, so the shared engine can run over this namespace too.
    ///
    /// This is the payoff: [`crate::collisions`] and [`crate::trespasses`] need
    /// no change to police primitive names.
    #[must_use]
    pub fn declarations(&self) -> Vec<Declaration> {
        let mut out: Vec<Declaration> = self
            .sightings
            .iter()
            .flat_map(|(word, seen)| {
                seen.iter().map(move |(surface, raw)| Declaration {
                    keyword: word.clone(),
                    path: Path::new(&format!("{surface:?}")).join(raw),
                    line: 0,
                })
            })
            .collect();
        out.sort();
        out
    }
}

/// A word a family still advertises as available that something already holds.
///
/// **This is the check that would have prevented the 2026-08-08 rename.**
/// `bancada` sat in Craft/Making 匠's *Room to grow* column while
/// `VOCABULARY.md` registered it to banken. An author consulting the family
/// table — the correct place to look for an untaken word — got a false
/// all-clear from the registry's own pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleReservation {
    pub word: String,
    /// The surfaces that prove it is taken.
    pub claimed_by: Vec<Surface>,
}

/// Words advertised as untaken that are already claimed.
#[must_use]
pub fn stale_reservations(c: &Corpus) -> Vec<StaleReservation> {
    c.sightings
        .iter()
        .filter_map(|(word, seen)| {
            let surfaces: BTreeSet<Surface> = seen.iter().map(|(s, _)| *s).collect();
            if !surfaces.contains(&Surface::RoomToGrow) {
                return None;
            }
            let claimed: Vec<Surface> = surfaces.iter().copied().filter(|s| s.is_a_thing()).collect();
            (!claimed.is_empty()).then(|| StaleReservation {
                word: word.clone(),
                claimed_by: claimed,
            })
        })
        .collect()
}

/// A theory doc with no `## Naming` row — the backfill queue.
///
/// **The denominator is theory docs, not minted names, and the difference is
/// not a rounding.** Whether `BUILD.md` or `MIRAGEM.md` names a *minted
/// primitive* or states a *concept* is a judgement no lint can make without
/// classifying words, so this counts the decidable thing and says which one it
/// counted. Repos are deliberately excluded for the same reason at larger
/// scale: 992 directories are mostly generated providers and vendor mirrors,
/// and demanding a naming row for `akeyless-go-sdk` would bury the real queue.
///
/// Advisory by construction — see `run_names`. A gate on this number would be
/// a gate on that judgement call.
#[must_use]
pub fn unregistered(c: &Corpus) -> Vec<String> {
    c.sightings
        .iter()
        .filter(|(_, seen)| {
            let s: BTreeSet<Surface> = seen.iter().map(|(sf, _)| *sf).collect();
            !s.contains(&Surface::Registry) && s.contains(&Surface::TheoryDoc)
        })
        .map(|(w, _)| w.clone())
        .collect()
}

/// A name the docs treat as real that names nothing — *free but poisoned*.
///
/// `camelot-incept` is cited by two documents as an existing repo. It does not
/// exist (`gh` resolves nothing, control-tested against a private repo that
/// does). The word is therefore unusable-by-confusion while protecting nothing.
/// `fiada` was briefly this too: minted into a `references:` edge before it
/// named anything.
#[must_use]
pub fn phantoms(c: &Corpus) -> Vec<String> {
    c.sightings
        .iter()
        .filter(|(_, seen)| {
            let s: BTreeSet<Surface> = seen.iter().map(|(sf, _)| *sf).collect();
            // Named by a family or the naming registry, with nothing behind it.
            // `Term` counts as something: a type defined in the glossary is a
            // real referent even with no repo and no doc of its own.
            (s.contains(&Surface::Registry) || s.contains(&Surface::FamilyMember))
                && !s.contains(&Surface::TheoryDoc)
                && !s.contains(&Surface::Repo)
                && !s.contains(&Surface::Term)
        })
        .map(|(w, _)| w.clone())
        .collect()
}

// ── The scanners ───────────────────────────────────────────────────
//
// Parsing is PURE over `&str` and the directory walk is thin, so every
// interesting case is testable without a filesystem — the same seam the
// TYPED-SPEC triplet asks for.

/// Bolded first-cell names from `VOCABULARY.md`, tagged by section.
///
/// Matches `| **word** | …`. Trailing parenthetical glosses are dropped, so
/// `| **banken** (番犬) |` yields `banken`. A row inside `## Naming` is a
/// [`Surface::Registry`] entry; anywhere else it is a [`Surface::Term`].
///
/// Two kinds of row are dropped as prose, not names:
/// multi-word terms, and **capitalized** ones. `## Naming` mixes per-word
/// glosses (`terreiro`, `saguão`) with meta-rows *about* naming (`Naming law`,
/// `Mnemonic law`, `Hebrew (akeyless-facing) names`) — and that last one is
/// why capitalization has to be checked after the paren is stripped, since it
/// otherwise reduces to the single token `Hebrew`.
#[must_use]
pub fn names_in_vocabulary(src: &str) -> Vec<(String, Surface)> {
    let mut section_is_naming = false;
    let mut out = Vec::new();
    for line in src.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section_is_naming = h.trim() == "Naming";
            continue;
        }
        let Some(rest) = line.trim_start().strip_prefix("| **") else { continue };
        let Some((word, _)) = rest.split_once("**") else { continue };
        let word = word.split('(').next().unwrap_or(word).trim();
        if word.is_empty() || word.contains(' ') {
            continue;
        }
        if word.chars().next().is_some_and(char::is_uppercase) {
            continue;
        }
        out.push((
            word.to_string(),
            if section_is_naming { Surface::Registry } else { Surface::Term },
        ));
    }
    out
}

/// Backticked words from `NAMING.md`'s family table, tagged by column.
///
/// **Columns bind by HEADER NAME, never by position.** `NAMING.md` holds more
/// than one four-column table, and the law table
/// (`| Language | Layer | The word denotes… | Examples |`) has *shipped
/// examples* exactly where the family table has *Room to grow*. Reading by
/// index made all nineteen of the first live run's stale-reservation findings
/// false positives — `mado`, `tatara`, `shikumi` and friends reported as
/// unclaimed reserves because they are cited as examples of Japanese naming.
/// Binding by header also means a reordered column cannot silently invert the
/// claimed/free verdict, which is the same failure with a slower fuse.
///
/// Over-collecting from Members is harmless — those words ARE taken. And a
/// word annotated *rejected, do not re-propose* in the Room-to-grow cell is
/// deliberately still collected: rejected is not free either, so `is_free`
/// should keep returning false for it.
#[must_use]
pub fn names_in_naming_families(src: &str) -> Vec<(String, Surface)> {
    let mut out = Vec::new();
    // Column indices for the table currently being read; None outside one.
    let mut members: Option<usize> = None;
    let mut room: Option<usize> = None;

    for line in src.lines() {
        let t = line.trim_start();
        if !t.starts_with('|') {
            // Any non-table line ends the table's scope.
            members = None;
            room = None;
            continue;
        }
        let cells: Vec<&str> = t.split('|').collect();

        // A header row names its columns; adopt it and move on.
        let header_members = cells.iter().position(|c| c.trim().starts_with("Members"));
        let header_room = cells.iter().position(|c| c.trim().starts_with("Room to grow"));
        if header_members.is_some() || header_room.is_some() {
            members = header_members;
            room = header_room;
            continue;
        }
        if t.starts_with("|---") || t.starts_with("| ---") {
            continue;
        }

        if let Some(cell) = members.and_then(|i| cells.get(i)) {
            out.extend(
                backticked(cell)
                    .into_iter()
                    .map(|(w, _)| (w, Surface::FamilyMember)),
            );
        }
        // A RESERVATION is a backticked word carrying a parenthetical gloss —
        // `kanna` (plane), `alicerce` (foundation footing). That convention
        // holds in every cell, and it is the only thing separating a reserve
        // from a word the cell merely mentions in prose: the Craft cell
        // explains that "an oficina is the room that CONTAINS a `bancada`",
        // which reserves nothing. Reading those as reserves is what made
        // `bancada` report as a stale reservation after it was already fixed.
        //
        // Dropping them is safe in BOTH directions, which is why this is not a
        // precision-for-recall trade: a word mentioned only in prose is not
        // reserved, so `is_free` returning true for it is the right answer.
        if let Some(cell) = room.and_then(|i| cells.get(i)) {
            out.extend(
                backticked(cell)
                    .into_iter()
                    .filter(|(_, glossed)| *glossed)
                    .map(|(w, _)| (w, Surface::RoomToGrow)),
            );
        }
    }
    out
}

/// Every `` `word` `` in a cell, with whether a parenthetical gloss follows it.
fn backticked(cell: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = cell;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else { break };
        let word = &rest[..close];
        rest = &rest[close + 1..];
        let glossed = rest.trim_start().starts_with('(');
        // A family cell also backticks paths and prose fragments; a name is one
        // token with no path separator and no spaces.
        if !word.is_empty()
            && !word.contains(' ')
            && !word.contains('/')
            && !word.contains('.')
            && word.chars().all(|c| c.is_alphanumeric() || "-_áàâãéêíóôõúüçñ".contains(c))
        {
            out.push((word.to_string(), glossed));
        }
    }
    out
}

/// Every `pleme-io/<name>` a document cites **as a repo**.
///
/// The citation asserts a repo exists, and that is checkable. Four shapes are
/// excluded because they are not repo citations, each measured against the
/// live corpus rather than guessed:
///
/// - **An OCI image path.** `ghcr.io/pleme-io/mysql` names an image in the
///   org's registry, not a repository. Four of CAMELOT.md's findings were this.
/// - **A truncated placeholder.** `pleme-io/crossplane-<provider>` stops at the
///   `<`, leaving a trailing hyphen. Six findings, all of that shape.
/// - **A capital letter.** Repo names here are lowercase, so `pleme-io/CLAUDE`
///   (really `CLAUDE.md`), `pleme-io/Camelot-owned` (prose) and
///   `pleme-io/${V}-go` (a template) are not citations.
/// - **Digits alone.** A commit-adjacent number, never a name.
///
/// `github:pleme-io/checkout` deliberately survives all four — it is a flake
/// input naming a repo that does not exist, which is exactly the finding.
#[must_use]
pub fn cited_repos(src: &str) -> Vec<String> {
    const MARKER: &str = "pleme-io/";
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(i) = src[at..].find(MARKER) {
        let start = at + i;
        at = start + MARKER.len();
        // An OCI reference: the host immediately precedes the org segment.
        let before = &src[..start];
        if ["ghcr.io/", "docker.io/", "quay.io/", "registry.io/"]
            .iter()
            .any(|h| before.ends_with(h))
            || before.ends_with(".amazonaws.com/")
        {
            continue;
        }
        let name: String = src[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if name.is_empty()
            || name.ends_with('-')
            || name.chars().any(|c| c.is_ascii_uppercase())
            || name.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        out.push(name);
    }
    out.sort();
    out.dedup();
    out
}

/// Cited repos that do not exist under `root`.
///
/// **The oracle is the local checkout, so a repo that exists on GitHub but is
/// not cloned reads as a phantom.** That is a real limit, not a theoretical
/// one, and it is why this gate belongs in a CI job that clones the org rather
/// than on a workstation. Spot-checked 2026-08-08 on three findings
/// (`utsuroi`, `veneziana`, `checkout`): all three return 404 from the GitHub
/// API, against a control (`theory`) that resolves — so on today's checkout of
/// 978 repos the two oracles agree.
///
/// # Errors
/// Propagates filesystem errors from reading `root/theory`.
pub fn phantom_repos(root: &Path) -> std::io::Result<Vec<(String, String)>> {
    let theory = root.join("theory");
    if !theory.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in std::fs::read_dir(&theory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else { continue };
        let doc = entry.file_name().to_string_lossy().into_owned();
        for name in cited_repos(&src) {
            // A path like `pleme-io/theory/blob/main/X.md` cites a real repo
            // whose name is `theory`; the walk above already handles that by
            // stopping at the `/`.
            if root.join(&name).is_dir() {
                continue;
            }
            if seen.insert(name.clone()) {
                out.push((name, doc.clone()));
            }
        }
    }
    Ok(out)
}

/// Gather the whole corpus from an org checkout.
///
/// `root` is the org directory (`~/code/github/pleme-io`). Walks only what it
/// needs — repo directory names, `theory/` filenames, and the two registry
/// documents — and **never consults a `.gitignore`**, which is the specific
/// reason the prescribed `rg` sweep returned a silent zero.
///
/// # Errors
/// Propagates filesystem errors. A scan that swallowed an unreadable directory
/// would report a zero it did not earn — the same rule [`crate::scan`] follows.
pub fn scan_corpus(root: &Path) -> std::io::Result<Corpus> {
    let mut c = Corpus::default();

    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        c.see(&name, Surface::Repo, &name);
    }

    let theory = root.join("theory");
    if theory.is_dir() {
        // Docs whose stem is a heading, not a minted name.
        const NOT_NAMES: &[&str] = &[
            "readme", "vocabulary", "naming", "theory", "index", "claude", "license",
        ];
        for entry in std::fs::read_dir(&theory)? {
            let entry = entry?;
            let f = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = f.strip_suffix(".md") else { continue };
            if NOT_NAMES.contains(&fold(stem).as_str()) {
                continue;
            }
            c.see(stem, Surface::TheoryDoc, stem);
        }
        if let Ok(src) = std::fs::read_to_string(theory.join("VOCABULARY.md")) {
            for (w, surface) in names_in_vocabulary(&src) {
                c.see(&w, surface, &w);
            }
        }
        if let Ok(src) = std::fs::read_to_string(theory.join("NAMING.md")) {
            for (w, surface) in names_in_naming_families(&src) {
                c.see(&w, surface, &w);
            }
        }
    }

    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(rows: &[(&str, Surface)]) -> Corpus {
        let mut c = Corpus::default();
        for (w, s) in rows {
            c.see(w, *s, w);
        }
        c
    }

    /// Diacritics must not split one word into two verdicts.
    #[test]
    fn folding_unifies_the_fleets_own_disagreeing_spellings() {
        assert_eq!(fold("alfândega"), fold("ALFANDEGA"));
        assert_eq!(fold("mutirão"), fold("mutirao"));
        assert_eq!(fold("Lançadeira"), "lancadeira");
    }

    /// THE REGRESSION. bancada was in Room-to-grow AND registered; an author
    /// consulting the family table was told it was available.
    #[test]
    fn a_reserved_word_that_is_already_taken_is_reported() {
        let c = corpus(&[("bancada", Surface::RoomToGrow), ("bancada", Surface::Registry)]);
        let stale = stale_reservations(&c);
        assert_eq!(stale.len(), 1, "the bancada drift must be caught");
        assert_eq!(stale[0].word, "bancada");
        assert!(stale[0].claimed_by.contains(&Surface::Registry));
    }

    /// A genuinely untaken reservation is not a finding — otherwise every
    /// Room-to-grow column would be permanently red and the gate ignored.
    #[test]
    fn an_untaken_reservation_is_not_a_finding() {
        let c = corpus(&[("andaime", Surface::RoomToGrow)]);
        assert!(stale_reservations(&c).is_empty());
    }

    /// Room-to-grow blocks a MINTER without counting as a thing. Both halves
    /// matter: free=false, but it is not evidence the primitive exists.
    #[test]
    fn room_to_grow_is_reserved_but_is_not_a_thing() {
        let c = corpus(&[("alicerce", Surface::RoomToGrow)]);
        assert!(!c.is_free("alicerce"), "a reserved word is not free to mint");
        assert!(!Surface::RoomToGrow.is_a_thing());
        assert!(phantoms(&c).is_empty(), "a reservation is not a phantom");
    }

    /// The 48: a doc or repo exists, the registry does not know.
    #[test]
    fn a_doc_without_a_registry_row_is_unregistered() {
        let c = corpus(&[
            ("eclusa", Surface::TheoryDoc),
            ("postigo", Surface::TheoryDoc),
            ("postigo", Surface::Registry),
        ]);
        assert_eq!(unregistered(&c), vec!["eclusa".to_string()]);
    }

    /// camelot-incept: cited as a repo, is not one.
    #[test]
    fn a_named_thing_that_does_not_exist_is_a_phantom() {
        let c = corpus(&[
            ("camelot-incept", Surface::FamilyMember),
            ("magma", Surface::FamilyMember),
            ("magma", Surface::TheoryDoc),
        ]);
        assert_eq!(phantoms(&c), vec!["camelot-incept".to_string()]);
    }

    /// The payoff: the shared engine polices this namespace unchanged.
    #[test]
    fn the_shared_collision_engine_runs_over_names_unchanged() {
        let c = corpus(&[("bancada", Surface::Registry), ("bancada", Surface::Repo)]);
        let found = crate::collisions(&c.declarations());
        assert_eq!(found.len(), 1, "two surfaces claiming one word is a collision");
        assert_eq!(found[0].keyword, "bancada");
    }

    /// is_free answers step 4's actual question across every surface.
    #[test]
    fn is_free_consults_the_whole_corpus_not_just_the_registry() {
        let c = corpus(&[("suri", Surface::Repo)]);
        assert!(!c.is_free("suri"), "a repo with no doc still claims its name");
        assert!(c.is_free("nomes"), "an untouched word is free");
    }

    // ── the scanners ───────────────────────────────────────────────

    #[test]
    fn vocabulary_rows_keep_diacritics_and_drop_glosses() {
        let src = "\
## Naming

| Term | Meaning |
|---|---|
| **alfândega** | the receipt-gated registry boundary |
| **banken** (番犬) | pleme-io-native k9s |
| not bolded | ignored |
| **two words here** | ignored: a name is one token |
";
        assert_eq!(
            names_in_vocabulary(src),
            vec![
                ("alfândega".to_string(), Surface::Registry),
                ("banken".to_string(), Surface::Registry),
            ]
        );
    }

    /// The distinction that made the first live run report 209 registered
    /// names when the real figure was a fraction of that.
    #[test]
    fn only_the_naming_section_registers_a_name() {
        let src = "\
## Structure

| **typescape** | the typed primitive surface |
| **HelmRelease** | a Flux kind |

## Naming

| **terreiro** | Arena / enclosed Lisp VM. |
| **Naming law** | The generative doctrine … |
| **Mnemonic law** | A pleme-io name teaches its job. |
| **Hebrew (akeyless-facing) names** | Applied to methodologies. |
";
        assert_eq!(
            names_in_vocabulary(src),
            vec![
                // A glossary type claims its word without registering a name.
                ("typescape".to_string(), Surface::Term),
                ("terreiro".to_string(), Surface::Registry),
            ],
            "capitalized meta-rows are prose ABOUT naming, not names — and \
             `Hebrew (akeyless-facing) names` reduces to the single token \
             `Hebrew`, so the case must be checked after the paren is stripped"
        );
    }

    /// The diacritic split is the whole reason `fold` exists: the registry row
    /// is `alfândega` and the file is `ALFANDEGA.md`, and they must meet.
    #[test]
    fn the_registry_row_and_the_ascii_filename_fold_together() {
        let mut c = Corpus::default();
        for (w, s) in names_in_vocabulary("## Naming\n| **alfândega** | … |") {
            c.see(&w, s, &w);
        }
        c.see("ALFANDEGA", Surface::TheoryDoc, "ALFANDEGA");
        assert_eq!(c.sightings.len(), 1, "one name, not two");
        assert!(unregistered(&c).is_empty(), "the row DOES cover the doc");
    }

    /// A glossary type row does NOT discharge a doc's naming registration.
    #[test]
    fn a_term_row_does_not_count_as_registration() {
        let mut c = Corpus::default();
        c.see("bacia", Surface::Term, "bacia");
        c.see("BACIA", Surface::TheoryDoc, "BACIA");
        assert_eq!(unregistered(&c), vec!["bacia".to_string()]);
        assert!(!c.is_free("bacia"), "but the word is still taken");
    }

    #[test]
    fn cited_repos_are_extracted_from_prose_and_links() {
        let src = "see [`pleme-io/camelot-incept`](https://github.com/pleme-io/camelot-incept) \
                   and pleme-io/theory/blob/main/BUILD.md";
        assert_eq!(
            cited_repos(src),
            vec!["camelot-incept".to_string(), "theory".to_string()],
            "a deep link cites the repo it is rooted at, not the whole path"
        );
    }

    /// Every exclusion below is a false positive the first live run produced.
    #[test]
    fn the_four_non_citation_shapes_are_excluded() {
        for src in [
            "wire `ghcr.io/pleme-io/mysql:8` into the chart", // an image, not a repo
            "emit `pleme-io/crossplane-<provider>` per backend", // truncated placeholder
            "see pleme-io/CLAUDE.md",                         // a file
            "`pleme-io/${V}-go` is generated",                // a template
            "commit pleme-io/9854099",                        // a number
        ] {
            assert!(cited_repos(src).is_empty(), "should not be a citation: {src}");
        }
        // …and the shape that must still be caught: a flake input naming a
        // repo that does not exist.
        assert_eq!(
            cited_repos("inputs.checkout.url = \"github:pleme-io/checkout\";"),
            vec!["checkout".to_string()]
        );
    }

    #[test]
    fn family_columns_split_members_from_room_to_grow() {
        let src = "\
| Family | Domain | Members (shipped) | Room to grow (untaken) |
|---|---|---|---|
| **Craft** 匠 | making | `bancada`, `tatara` | `torno` (lathe), `bigorna` (anvil) |
| **The Loom** 機 | weaving | `urdume` | `fiada` (course/row) |
";
        let got = names_in_naming_families(src);
        assert_eq!(
            got,
            vec![
                ("bancada".into(), Surface::FamilyMember),
                ("tatara".into(), Surface::FamilyMember),
                ("torno".into(), Surface::RoomToGrow),
                ("bigorna".into(), Surface::RoomToGrow),
                ("urdume".into(), Surface::FamilyMember),
                ("fiada".into(), Surface::RoomToGrow),
            ]
        );
    }

    /// The header row must not be read as a family whose "members" are the
    /// words *Members* and *Room to grow*.
    #[test]
    fn the_table_header_is_not_a_family() {
        let src = "| Family | Domain | Members (shipped) | Room to grow |\n|---|---|---|---|\n";
        assert!(names_in_naming_families(src).is_empty());
    }

    /// **The bug that made all 19 of the first live run's findings false.**
    /// NAMING.md's law table has *shipped examples* in the same column
    /// position the family table uses for *Room to grow*.
    #[test]
    fn the_law_tables_examples_column_is_not_room_to_grow() {
        let src = "\
| Language | Layer | The word denotes… | Examples |
|---|---|---|---|
| **Japanese (日本語)** | substrate | the essence | `mado`, `tatara`, `shikumi` |

Some prose between the tables.

| Family | Domain | Members (shipped) | Room to grow (untaken) |
|---|---|---|---|
| **Craft** 匠 | making | `tatara` | `torno` (lathe) |
";
        let got = names_in_naming_families(src);
        assert!(
            !got.contains(&("mado".into(), Surface::RoomToGrow)),
            "a shipped example is not an untaken reserve"
        );
        assert_eq!(
            got,
            vec![
                ("tatara".into(), Surface::FamilyMember),
                ("torno".into(), Surface::RoomToGrow),
            ],
            "the law table contributes nothing; only the family table does"
        );
    }

    /// Position-independence is the point of binding by header.
    #[test]
    fn reordered_columns_do_not_invert_the_verdict() {
        let src = "\
| Family | Room to grow (untaken) | Members (shipped) |
|---|---|---|
| **Craft** 匠 | `torno` (lathe) | `tatara` |
";
        assert_eq!(
            names_in_naming_families(src),
            vec![
                ("tatara".into(), Surface::FamilyMember),
                ("torno".into(), Surface::RoomToGrow),
            ]
        );
    }

    /// A rejected word stays collected. Rejected is not free — re-proposing it
    /// is precisely what the annotation exists to stop.
    #[test]
    fn a_rejected_word_is_still_claimed() {
        let src = "| Family | Domain | Members (shipped) | Room to grow (untaken) |\n\
                   | **Craft** 匠 | making | `bancada` | \
                   *rejected, do not re-propose:* `oficina` (workshop) |\n";
        let got = names_in_naming_families(src);
        assert!(got.contains(&("oficina".into(), Surface::RoomToGrow)));
        let mut c = Corpus::default();
        for (w, s) in got {
            c.see(&w, s, &w);
        }
        assert!(!c.is_free("oficina"));
    }

    /// Cells backtick paths and prose too; those are not candidate names.
    #[test]
    fn backticked_paths_and_phrases_are_not_names() {
        assert_eq!(
            backticked("`caixa` (box) see `theory/CAIXA.md` and `two words`"),
            vec![("caixa".to_string(), true)]
        );
    }

    /// The gloss is what separates a reservation from a prose mention.
    #[test]
    fn only_a_glossed_word_reserves_room_to_grow() {
        let src = "\
| Family | Domain | Members (shipped) | Room to grow (untaken) |
|---|---|---|---|
| **Craft** 匠 | making | `tatara` | `kanna` (plane) — \
*`oficina` (workshop) is rejected: it is the room that contains a `bancada`* |
";
        let got = names_in_naming_families(src);
        assert!(got.contains(&("kanna".into(), Surface::RoomToGrow)));
        assert!(
            got.contains(&("oficina".into(), Surface::RoomToGrow)),
            "a rejected word is still not free"
        );
        assert!(
            !got.contains(&("bancada".into(), Surface::RoomToGrow)),
            "a word the cell merely mentions in prose reserves nothing"
        );
    }

    /// The bug this whole tool replaces, reproduced as a unit: a word in the
    /// untaken column that the registry already spent.
    #[test]
    fn the_bancada_incident_is_caught_end_to_end() {
        let vocab = "## Naming\n| **bancada** | banken's session surface |";
        let naming = "| Family | Domain | Members (shipped) | Room to grow (untaken) |\n\
                      | **Craft** 匠 | making | `tatara` | `bancada` (workbench), `torno` (lathe) |";
        let mut c = Corpus::default();
        for (w, s) in names_in_vocabulary(vocab) {
            c.see(&w, s, &w);
        }
        for (w, s) in names_in_naming_families(naming) {
            c.see(&w, s, &w);
        }
        let stale = stale_reservations(&c);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].word, "bancada");
        assert_eq!(stale[0].claimed_by, vec![Surface::Registry]);
        assert!(!c.is_free("bancada"), "and step 4 now answers correctly");
    }
}

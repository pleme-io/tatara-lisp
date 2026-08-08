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
    /// A bolded first-cell row in `VOCABULARY.md`.
    Registry,
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

/// A minted name with a thing but no registry row — the backfill queue.
///
/// Measured 2026-08-08: 48 of 82. Registration is not habitually skipped so
/// much as recently invented, so this is a backlog rather than negligence —
/// but every one of them is a word the registry cannot warn a minter about.
#[must_use]
pub fn unregistered(c: &Corpus) -> Vec<String> {
    c.sightings
        .iter()
        .filter(|(_, seen)| {
            let s: BTreeSet<Surface> = seen.iter().map(|(sf, _)| *sf).collect();
            !s.contains(&Surface::Registry)
                && (s.contains(&Surface::TheoryDoc) || s.contains(&Surface::Repo))
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
            // Named by a family or the registry, but no doc and no repo behind it.
            (s.contains(&Surface::Registry) || s.contains(&Surface::FamilyMember))
                && !s.contains(&Surface::TheoryDoc)
                && !s.contains(&Surface::Repo)
        })
        .map(|(w, _)| w.clone())
        .collect()
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
}

//! `ScratchRegistry` — the interpreter OWNS every temp path it mints.
//!
//! ## The leak this exists to make unrepresentable (measured 2026-07-31)
//!
//! `tmp-dir` / `tmp-file` used to `create_dir_all` under `std::env::temp_dir()`
//! and hand the path back as a bare `Value::Str`. Nothing owned it, so nothing
//! ever removed it. On `rio` that produced:
//!
//! ```text
//! ls -d /tmp/tatara-script-* | wc -l   ->  21,608
//! du -shc /tmp/tatara-script-*         ->  13 GB
//! oldest 05:02, newest 22:26, uptime 17h19m  =>  ~1,250 dirs/hour, since boot
//! ```
//!
//! That box mounts `/tmp` as a **48 GiB tmpfs on 29 GiB of RAM**, so those 13 GB
//! were not disk — they were memory. Combined with ~25 GB of other scratch it
//! filled RAM *and* all 31.9 GiB of swap (`SwapFree: 176 kB`), drove PSI
//! `memory.full avg60` to 92%, and left the OOM killer as the only reclaim
//! path — it killed `comin`'s `git` mid-deploy. A leaked temp dir is not a
//! tidiness problem on a tmpfs host; it is a memory leak that takes the node
//! down.
//!
//! ## Why a registry rather than "remember to delete it"
//!
//! The old signature made the leak the DEFAULT and correctness opt-in: a script
//! had to remember an explicit delete, on every exit path including error. The
//! registry inverts that. `scratch_dir` / `scratch_file` are the only
//! constructors, they always record, and `Drop` always removes — so "created
//! but never cleaned" has no representation. Correctness is what you get by
//! doing nothing.
//!
//! ## Two failure modes, two mechanisms
//!
//! `Drop` covers normal exit, early `return`, and panic-unwind. It CANNOT cover
//! `SIGKILL`, an OOM kill, or a power loss — and on the very host that
//! motivated this, OOM kills were happening three times in six hours. So RAII
//! alone would have left a residue that regrows. [`sweep_stale`] is the
//! reconciler for that path: a bounded, best-effort sweep of *our own* prefix,
//! old enough that no live process can still hold it. Invariant for the normal
//! case, reconciler for the violent one.
//!
//! Escape hatch: set `TATARA_SCRIPT_KEEP_SCRATCH=1` to retain scratch for
//! debugging. It is deliberately an env var rather than a Lisp argument —
//! keeping is an operator's debugging choice, not a script's contract, and a
//! script that could opt into leaking would reopen the class.

use std::path::{Path, PathBuf};

/// How old one of our scratch entries must be before [`sweep_stale`] will
/// remove it. Generously above any plausible script runtime: the sweep must
/// never race a *live* sibling process's scratch, and the cost of waiting is
/// only a few hours of residue after a kill.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Upper bound on entries removed in one sweep. A sweep runs at interpreter
/// startup, so it must never become the dominant cost of running a script —
/// on the measured host there were 21,608 entries, and unlinking all of them
/// takes minutes. Bounded work per run, repeated across runs, converges
/// without ever making one script pay for the whole backlog.
const SWEEP_BUDGET: usize = 512;

/// The filename prefix every scratch entry carries. Sweeping matches on this,
/// so it must never widen to something another tool could also produce.
const PREFIX: &str = "tatara-script-";

/// Owns the temp paths minted during one interpreter run and removes them on
/// drop.
///
/// Holds `PathBuf`s rather than open handles deliberately: the Lisp side needs
/// a *path string* it can pass to subprocesses, and a script legitimately
/// creates, removes, and recreates files underneath a scratch dir. Ownership
/// here is of the path's lifetime, not of a file descriptor.
#[derive(Debug, Default)]
pub struct ScratchRegistry {
    paths: Vec<PathBuf>,
    /// Distinguishes two scratch paths minted inside the same nanosecond.
    /// `SystemTime::now()` is not guaranteed to advance between two adjacent
    /// calls, and a script doing `(tmp-dir)` twice in a loop is ordinary.
    seq: u64,
}

impl ScratchRegistry {
    /// Mint an owned scratch DIRECTORY and return its path.
    pub fn dir(&mut self) -> std::io::Result<PathBuf> {
        let path = self.mint("");
        std::fs::create_dir_all(&path)?;
        self.paths.push(path.clone());
        Ok(path)
    }

    /// Mint an owned scratch FILE (created empty) and return its path.
    pub fn file(&mut self) -> std::io::Result<PathBuf> {
        let path = self.mint(".tmp");
        std::fs::write(&path, b"")?;
        self.paths.push(path.clone());
        Ok(path)
    }

    /// Build a unique path under the system temp dir.
    ///
    /// The name carries the pid as well as the clock: two concurrent
    /// `tatara-script` processes can otherwise mint the same name from the same
    /// nanosecond, and the loser's `Drop` would delete the winner's live
    /// scratch. That is the same class of bug as the one fixed in ami-forge's
    /// secret var-file (pid-only names colliding within a process) — here the
    /// collision is across processes, so the pid is the fix rather than the
    /// cause.
    fn mint(&mut self, suffix: &str) -> PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let pid = std::process::id();
        let seq = self.seq;
        self.seq += 1;
        std::env::temp_dir().join(format!("{PREFIX}{pid}-{now:x}-{seq}{suffix}"))
    }

    /// Number of live scratch entries. Exposed for tests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the registry currently owns nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// True when the operator asked to retain scratch for debugging.
fn keep_requested() -> bool {
    std::env::var_os("TATARA_SCRIPT_KEEP_SCRATCH").is_some_and(|v| v != "0" && v != "")
}

impl Drop for ScratchRegistry {
    fn drop(&mut self) {
        if keep_requested() {
            return;
        }
        for p in self.paths.drain(..) {
            // Best-effort on every path, and DELIBERATELY not short-circuiting
            // on the first error: one undeletable entry (a busy mount, a
            // permission change made by the script itself) must not strand
            // every remaining entry. A failure here is also never propagated —
            // a cleanup error must not mask the script's own exit status.
            let _ = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
}

/// Remove OUR OWN stale scratch left behind by processes that died without
/// running `Drop` (SIGKILL, OOM kill, power loss).
///
/// Returns the number of entries removed. Best-effort throughout: this runs on
/// the startup path of every script, so it must never fail a run and never
/// dominate its cost.
///
/// Three safety properties, each load-bearing:
/// - **Only our prefix.** Matching is on `tatara-script-`, so the sweep can
///   never touch another tool's scratch — including the operator's own
///   `/tmp/tmp.*` and build artifacts, which on the measured host were far
///   larger than ours and are emphatically not ours to delete.
/// - **Only genuinely old entries.** [`STALE_AFTER`] is hours, so a *live*
///   sibling process's scratch is never eligible. An age check is what makes
///   this safe under concurrency, where a pid check would not be — pids are
///   reused.
/// - **Bounded work.** At most [`SWEEP_BUDGET`] removals per run.
pub fn sweep_stale() -> usize {
    if keep_requested() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    for entry in entries.flatten() {
        if removed >= SWEEP_BUDGET {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(PREFIX) {
            continue;
        }
        if !is_stale(&entry, now) {
            continue;
        }
        let path = entry.path();
        let ok = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if ok.is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether a directory entry is older than [`STALE_AFTER`].
///
/// Uses mtime rather than ctime/atime: a scratch dir being *written to* is
/// evidence it is live, and mtime is the field that tracks that. An entry
/// whose metadata cannot be read is treated as NOT stale — the safe direction,
/// since the cost of skipping is a few leftover bytes and the cost of a false
/// positive is deleting live scratch.
fn is_stale(entry: &std::fs::DirEntry, now: std::time::SystemTime) -> bool {
    let Ok(meta) = entry.metadata() else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    now.duration_since(mtime).is_ok_and(|age| age >= STALE_AFTER)
}

/// Path helper for tests + callers that want to reason about our namespace.
#[must_use]
pub fn is_scratch_path(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point: a minted dir is gone once the registry drops.
    #[test]
    fn a_scratch_dir_is_removed_on_drop() {
        let path = {
            let mut r = ScratchRegistry::default();
            let p = r.dir().expect("mint dir");
            assert!(p.is_dir(), "the dir must exist while the registry lives");
            p
        };
        assert!(
            !path.exists(),
            "a scratch dir must not outlive the interpreter — this is the leak \
             that put 21,608 dirs and 13 GB into rio's tmpfs"
        );
    }

    #[test]
    fn a_scratch_file_is_removed_on_drop() {
        let path = {
            let mut r = ScratchRegistry::default();
            let p = r.file().expect("mint file");
            assert!(p.is_file());
            p
        };
        assert!(!path.exists(), "a scratch file must not outlive the interpreter");
    }

    /// A dir the script filled must still be removable — `remove_dir_all`, not
    /// `remove_dir`. The leaked dirs on rio were not empty; eight were 1.4 GB.
    #[test]
    fn a_non_empty_scratch_dir_is_still_removed() {
        let path = {
            let mut r = ScratchRegistry::default();
            let p = r.dir().expect("mint dir");
            std::fs::create_dir_all(p.join("nested/deeper")).expect("nest");
            std::fs::write(p.join("nested/deeper/file.txt"), b"content").expect("write");
            p
        };
        assert!(!path.exists(), "a non-empty scratch dir must still be removed");
    }

    /// Every entry is cleaned, not just the first — and the registry owns many.
    #[test]
    fn all_entries_are_removed_not_only_the_first() {
        let paths: Vec<PathBuf> = {
            let mut r = ScratchRegistry::default();
            let v = (0..5).map(|_| r.dir().expect("mint")).collect::<Vec<_>>();
            assert_eq!(r.len(), 5);
            v
        };
        for p in paths {
            assert!(!p.exists(), "{} survived", p.display());
        }
    }

    /// Two paths minted back-to-back must differ. `SystemTime::now()` is not
    /// guaranteed to advance between adjacent calls, so the sequence counter —
    /// not the clock — is what guarantees this.
    #[test]
    fn two_paths_minted_in_the_same_instant_are_distinct() {
        let mut r = ScratchRegistry::default();
        let a = r.dir().expect("a");
        let b = r.dir().expect("b");
        assert_ne!(a, b, "a collision would make one script delete another's scratch");
        assert_eq!(r.len(), 2);
    }

    /// The name must carry the pid, so two concurrent processes cannot collide
    /// and delete each other's live scratch.
    #[test]
    fn the_path_is_process_scoped() {
        let mut r = ScratchRegistry::default();
        let p = r.dir().expect("mint");
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.contains(&std::process::id().to_string()),
            "expected pid in {name:?}"
        );
        assert!(is_scratch_path(&p));
    }

    /// The sweep must never touch a path that is not ours. This is the property
    /// that keeps it from deleting the operator's own /tmp work — which on the
    /// measured host was ~25 GB and explicitly not ours to remove.
    #[test]
    fn the_sweep_ignores_paths_that_are_not_ours() {
        let foreign = std::env::temp_dir().join(format!("NOT-OURS-{}", std::process::id()));
        std::fs::create_dir_all(&foreign).expect("create foreign");
        sweep_stale();
        assert!(
            foreign.exists(),
            "the sweep must only ever match its own prefix"
        );
        let _ = std::fs::remove_dir_all(&foreign);
    }

    /// A freshly-created scratch entry is NOT stale — otherwise a sweep would
    /// race a live sibling process and delete scratch still in use.
    #[test]
    fn the_sweep_does_not_remove_fresh_entries() {
        let mut r = ScratchRegistry::default();
        let p = r.dir().expect("mint");
        sweep_stale();
        assert!(
            p.exists(),
            "a live process's scratch must survive another process's sweep"
        );
    }
}

//! Capability profiles — what an interpreter is *able* to reach.
//!
//! ## The mechanism is absence, not a check
//!
//! A profile does not gate calls at runtime. It decides which `install`
//! functions run at all, so a form outside the profile is never bound
//! and calling it is an unbound-symbol error raised by the evaluator
//! itself. There is no policy object to consult, no branch to get wrong,
//! and nothing for a clever script to talk its way past — the same
//! shape wasm-platform's capability mapper uses, where "capabilities not
//! present in the set are simply not granted", and the same property
//! blue's wasm tests assert as `imports: 0`.
//!
//! ## Why this exists
//!
//! `install_stdlib` was all-or-nothing: 26 unconditional `install()`
//! calls. Every embedder got all 56 native functions, including
//! `sh-exec` (a literal `sh -c` with full metacharacter interpretation),
//! `rm-rf`, `env-set`, `kube-bearer-token` (the pod's ServiceAccount
//! token as a string), `sops-extract`, and `dns/upsert`+`dns/delete`
//! which mutate live DNS. That set is correct for an operator running a
//! deployment script on a workstation. It is wrong for anything
//! evaluating source it did not write — a controller reconciling a CR, a
//! renderer inside a compliance boundary — and there was previously no
//! way to say so.
//!
//! ## Honest limits
//!
//! This bounds *reach*, not *time or memory*. A sealed profile can still
//! spin forever or allocate without limit; that is what
//! `tatara-lisp-eval`'s `Budget` is for, and the two compose rather than
//! substitute. And a profile says nothing about what the embedder
//! registers itself afterwards — `install_with` is the floor, not a
//! ceiling.

use tatara_lisp_eval::Interpreter;

use super::ScriptCtx;

/// What a family of primitives can reach outside the process.
///
/// Ordered by blast radius, and deliberately coarse: a finer taxonomy
/// invites arguments about which bucket a form belongs in, and the whole
/// value here is that the answer is obvious.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Computation only. No syscall reaches outside this process.
    Pure,
    /// Reads the clock or the random pool. Non-deterministic, but
    /// observes nothing it could not have been told.
    Ambient,
    /// Reads the filesystem.
    FsRead,
    /// Writes or deletes on the filesystem.
    FsWrite,
    /// Reads or writes process environment variables.
    Env,
    /// Reads host identity — hostname, username, platform.
    HostInfo,
    /// Opens network connections, or listens.
    Net,
    /// Reads Kubernetes service-account credentials.
    ClusterCredentials,
    /// Decrypts secrets.
    Secrets,
    /// Starts a subprocess.
    Subprocess,
    /// Loads and evaluates another module.
    ModuleLoad,
}

impl Capability {
    /// Does this capability reach outside the process at all?
    ///
    /// The gate in this crate's tests keys on exactly this, so a new
    /// capability variant is classified once, here, rather than in every
    /// place that asks the question.
    #[must_use]
    pub fn escapes_process(self) -> bool {
        !matches!(self, Self::Pure | Self::Ambient)
    }
}

/// One stdlib family and what it can reach.
pub struct Family {
    pub name: &'static str,
    pub capability: Capability,
    install: fn(&mut Interpreter<ScriptCtx>),
}

/// Every FFI family, with its capability.
///
/// This is the ENUMERABLE catalogue: a reviewer reads one list rather
/// than 26 files, and the test below reads the same list rather than a
/// hand-maintained copy that could drift from it.
#[must_use]
pub fn families() -> Vec<Family> {
    use super::{
        cli, crypto_extra, dns, encoding, env, fs, hash, http, http_server, io, json, kube,
        list_ext, log, module, os, process, regex, sops, string, string_ext, time, toml, uuid,
        yaml,
    };
    vec![
        // ── pure ────────────────────────────────────────────────────
        Family { name: "cli", capability: Capability::Pure, install: cli::install },
        Family { name: "encoding", capability: Capability::Pure, install: encoding::install },
        Family { name: "hash", capability: Capability::Pure, install: hash::install },
        Family { name: "json", capability: Capability::Pure, install: json::install },
        Family { name: "list_ext", capability: Capability::Pure, install: list_ext::install },
        Family { name: "log", capability: Capability::Pure, install: log::install },
        Family { name: "regex", capability: Capability::Pure, install: regex::install },
        Family { name: "string", capability: Capability::Pure, install: string::install },
        Family { name: "string_ext", capability: Capability::Pure, install: string_ext::install },
        Family { name: "toml", capability: Capability::Pure, install: toml::install },
        Family { name: "yaml", capability: Capability::Pure, install: yaml::install },
        // ── ambient nondeterminism ──────────────────────────────────
        // Not "pure": a sealed build that must be reproducible wants
        // these absent too, which is why they are their own tier rather
        // than being folded in above.
        Family { name: "crypto_extra", capability: Capability::Ambient, install: crypto_extra::install },
        Family { name: "time", capability: Capability::Ambient, install: time::install },
        Family { name: "uuid", capability: Capability::Ambient, install: uuid::install },
        // ── reaches outside the process ─────────────────────────────
        Family { name: "fs", capability: Capability::FsWrite, install: fs::install },
        // io carries read-file AND write-file/exit; classified by its
        // strongest member, which is the only safe direction.
        Family { name: "io", capability: Capability::FsWrite, install: io::install },
        Family { name: "env", capability: Capability::Env, install: env::install },
        Family { name: "os", capability: Capability::HostInfo, install: os::install },
        Family { name: "http", capability: Capability::Net, install: http::install },
        Family { name: "http_server", capability: Capability::Net, install: http_server::install },
        Family { name: "dns", capability: Capability::Net, install: dns::install },
        Family { name: "kube", capability: Capability::ClusterCredentials, install: kube::install },
        Family { name: "sops", capability: Capability::Secrets, install: sops::install },
        Family { name: "process", capability: Capability::Subprocess, install: process::install },
        Family { name: "module", capability: Capability::ModuleLoad, install: module::install },
    ]
}

/// Which capabilities an interpreter is allowed to be given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: &'static str,
    allowed: Vec<Capability>,
}

impl Profile {
    /// Everything. The historical behaviour of `install_stdlib`, kept as
    /// the explicit default so no existing embedder changes shape.
    #[must_use]
    pub fn ambient() -> Self {
        Self {
            name: "ambient",
            allowed: vec![
                Capability::Pure,
                Capability::Ambient,
                Capability::FsRead,
                Capability::FsWrite,
                Capability::Env,
                Capability::HostInfo,
                Capability::Net,
                Capability::ClusterCredentials,
                Capability::Secrets,
                Capability::Subprocess,
                Capability::ModuleLoad,
            ],
        }
    }

    /// Computation only — nothing that reaches outside the process, and
    /// nothing that observes the clock or the random pool.
    ///
    /// This is the profile for evaluating source you did not write.
    #[must_use]
    pub fn sealed() -> Self {
        Self {
            name: "sealed",
            allowed: vec![Capability::Pure],
        }
    }

    /// Sealed, plus clock and randomness. For a renderer that may stamp
    /// a timestamp but must not reach the filesystem or the network.
    #[must_use]
    pub fn sealed_nondeterministic() -> Self {
        Self {
            name: "sealed-nondeterministic",
            allowed: vec![Capability::Pure, Capability::Ambient],
        }
    }

    #[must_use]
    pub fn allows(&self, c: Capability) -> bool {
        self.allowed.contains(&c)
    }

    /// The families this profile installs, in catalogue order.
    #[must_use]
    pub fn granted(&self) -> Vec<&'static str> {
        families()
            .into_iter()
            .filter(|f| self.allows(f.capability))
            .map(|f| f.name)
            .collect()
    }

    /// The families this profile withholds.
    #[must_use]
    pub fn withheld(&self) -> Vec<&'static str> {
        families()
            .into_iter()
            .filter(|f| !self.allows(f.capability))
            .map(|f| f.name)
            .collect()
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::ambient()
    }
}

/// Install the FFI families a profile permits. Families outside it are
/// never installed, so their names are simply unbound.
pub fn install_families(interp: &mut Interpreter<ScriptCtx>, profile: &Profile) {
    for f in families() {
        if profile.allows(f.capability) {
            (f.install)(interp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue must cover every stdlib family, or a profile silently
    /// stops constraining the one that was left out.
    ///
    /// Counted rather than named: a count carries its own denominator, so
    /// a catalogue that stopped discovering families fails here instead of
    /// passing vacuously with an empty list.
    #[test]
    fn the_catalogue_covers_every_ffi_family() {
        let names: Vec<&str> = families().iter().map(|f| f.name).collect();
        assert_eq!(
            names.len(),
            25,
            "catalogue has {} families: {names:?}",
            names.len()
        );
        for expected in [
            "fs", "io", "env", "os", "process", "http", "http_server", "dns", "kube", "sops",
            "module",
        ] {
            assert!(names.contains(&expected), "{expected} missing from the catalogue");
        }
    }

    /// THE GATE. A sealed profile must grant nothing that reaches outside
    /// the process.
    ///
    /// Carries its denominator deliberately: asserting only "no dangerous
    /// family is granted" would pass just as happily against an empty
    /// catalogue, which is the failure mode that makes a security gate
    /// worthless. So the count of families actually examined is asserted
    /// too.
    #[test]
    fn a_sealed_profile_grants_nothing_that_escapes_the_process() {
        let sealed = Profile::sealed();
        let all = families();
        assert!(!all.is_empty(), "empty catalogue — the gate would be vacuous");

        let mut examined = 0;
        for f in &all {
            examined += 1;
            if f.capability.escapes_process() {
                assert!(
                    !sealed.allows(f.capability),
                    "sealed profile grants {} ({:?}), which reaches outside the process",
                    f.name,
                    f.capability
                );
            }
        }
        assert_eq!(examined, 25, "examined {examined} families, expected 25");

        // And the named worst offenders are specifically withheld.
        let withheld = sealed.withheld();
        for dangerous in ["process", "fs", "io", "env", "kube", "sops", "http", "dns"] {
            assert!(
                withheld.contains(&dangerous),
                "sealed profile must withhold {dangerous}; withheld = {withheld:?}"
            );
        }
    }

    #[test]
    fn the_ambient_profile_grants_everything_so_no_existing_embedder_changes() {
        let ambient = Profile::ambient();
        assert!(
            ambient.withheld().is_empty(),
            "ambient must withhold nothing, withheld = {:?}",
            ambient.withheld()
        );
        assert_eq!(ambient.granted().len(), families().len());
    }

    #[test]
    fn sealed_nondeterministic_adds_only_the_clock_and_the_random_pool() {
        let p = Profile::sealed_nondeterministic();
        assert!(p.granted().contains(&"time"));
        assert!(p.granted().contains(&"uuid"));
        // …and still nothing that escapes.
        for f in families() {
            if f.capability.escapes_process() {
                assert!(!p.allows(f.capability), "{} leaked", f.name);
            }
        }
    }
}

# Tatara-Lisp

pending-vacuous-guard: tatara-lisp-flake-check
pending-closed-set-carrier-coupling: phase-2-step-2 blocked on an operator ruling

> **★★ PHASE 2 STEP 2 IS BLOCKED, and the blocker is structural, not
> effortful. Read this before attempting the error-variant port (2026-07-29).**

`theory/TATARA-LISP-CONSOLIDATION.md` phase 2 orders step 1 (split
`tatara-closed-set` out; **landed**, `011e8d0`) before step 2 (port A's ~45
structural `LispError` variants + their payload carriers). The two steps carry a
constraint the plan states as absolute —

> "`tatara-lisp` never carries it and never re-exports it. This is what makes
> the phase-3 facade legal … without importing 44,582 lines into the crate
> whose virtue is that it is small."

— and step 2 **cannot be satisfied while that constraint holds.** Measured, not
inferred:

**1. Seven of A's error-module carriers ARE closed-set implementors.**
`tatara/tatara-lisp/src/error.rs`: `CompilerSpecIoStage` :2738, `MacroDefHead`
:3760, `UnquoteForm` :4693, `KwargPathKind` :6518, `ExpectedKwargShape` :6742,
`SexpShape` :7090, `StructuralKind` :8424 — each `#[derive(…
tatara_lisp_derive::ClosedSet)]` with a `#[closed_set(…)]` attribute. The derive
generates their `FromStr`, their `Display`, and their `Unknown*`
parse-rejection carrier, and A's `lib.rs` re-exports all six `Unknown*` types
publicly.

**2. The 45 variants genuinely need them.** Reference counts inside
`LispError` (error.rs:807-2737): `SexpWitness` 57, `SexpShape` 34, `KwargPath`
22, `MacroDefHead` 18, `CompilerSpecIoStage` 13, `UnquoteForm` 9,
`TemplateInvariantKind` 9, `ExpectedKwargShape` 5,
`OptionalParamMalformedReason` 3. Not a leaf coupling — the payload alphabet.

**3. So `tatara-lisp` would have to depend on `tatara-closed-set`, and that is
a cargo cycle today.** Reproduced by adding the one line and building:

```
error: cyclic package dependency: package `tatara-lisp v0.3.3` depends on itself. Cycle:
package `tatara-lisp v0.3.3 (…/tatara-lisp)`
    ... which satisfies path dependency `tatara-lisp` of package `tatara-closed-set v0.3.3`
    ... which satisfies path dependency `tatara-closed-set` of package `tatara-lisp v0.3.3`
```

The cycle exists because `ClosedSet::suggest_closest` composes the substrate's
one bounded-edit-distance metric, which lives in `tatara-lisp`'s `domain`
module — so `tatara-closed-set` → `tatara-lisp` already. A had no cycle only
because both halves sat in ONE crate; the split is what forces a direction.

### The three resolutions, costed. Pick one; do not pick silently.

| # | shape | keeps carriers' ClosedSet surface | `tatara-lisp` closure | cost |
|---|---|---|---|---|
| **E** | port the carriers **without** the `ClosedSet` derive; hand-write `Unknown*` + `FromStr` + `Display` per enum | **no** — loses `ALL`/`labels`/`parse_label`/`suggest_closest` on 7 enums | unchanged (small) | ~96 hand-written lines the derive exists to eliminate; a knowing PRIME-DIRECTIVE regression, and a capability regression vs A |
| **H** | third leaf crate owns `suggest`; `tatara-closed-set` becomes a leaf; `tatara-lisp` → `tatara-closed-set` | yes, byte-faithfully | **grows** by tatara-closed-set (~9k real lines under ~53k of doc comment) | one net-new crate; **breaks the plan's stated constraint** — needs an explicit ruling that a dependency edge is not "carrying" |
| **I** | H, plus `tatara-closed-set` optional behind `feature = "closed-set"` (default off) + `cfg_attr` on the 7 enums | yes, for opt-in consumers | unchanged by default | a feature flag papering over a primitive's limitation, which the org CLAUDE.md names as an anti-pattern; 7 `cfg_attr` sites |

Note what is NOT a resolution: duplicating `suggest` into `tatara-closed-set`
to break the cycle. It breaks the cycle and still leaves H's closure cost,
buying a second Levenshtein for nothing.

**This is a hard, hard-to-reverse architectural call across a published crate
boundary — `/twin-reasoning` territory, not an implementer's judgement call.**
Step 1 is landed and green (2,061-test parity with A, measured). Steps 3 and 4
are ordered behind step 2 and depend on its payload carriers, so they are
blocked transitively, not independently.

> **The `ci` job has been red since 2026-05-30 and its `nix build` + smoke-test
> steps have been `skipped` on every run since. Do not read a green badge or a
> passing `nix flake check` on this repo as verification until this row closes.**

`.github/workflows/ci.yml:22` runs `nix flake check --no-build`. That flag builds
**nothing**: `nix flake check` only ever builds `checks.<system>.*`, and everything
else — packages, devShells, apps — it merely evaluates and prints `(build skipped)`.
Measured on `6f5349f`, same tree, one flag apart:

| command | exit | verdict |
|---|---|---|
| `nix flake check --no-build` | 0 | `✅ checks.aarch64-darwin.build` · `✅ …gen-confirm` |
| `nix flake check` | 1 | `❌ …gen-confirm` — `error: unrecognized subcommand 'confirm'` |

Removing the flag surfaced **two** defects it had hidden, plus one it had not caused:

1. **`checks.gen-confirm` invoked a phantom `gen confirm` subcommand** and therefore
   could never pass. Fixed upstream in substrate `57c3948` (2026-07-24), which this
   repo's pin predates. Verified **passing** on cold x86_64-linux CI in run
   `30411442349` once substrate is bumped.
2. **`checks.build` == `packages.x86_64-linux.default`** — verified identical
   `.drv` path, and the reason this row matters. On Linux that is a **static-musl**
   derivation, so **the artifact consumers get from `nix build` and the artifact the
   release path ships has never been built by this CI job, and does not build.**
   Step 5 (`nix build .#tatara-script`) builds `tatara-lisp-script`, a *different*,
   glibc derivation — which is why this went unseen. The failure is
   `llvm-static-x86_64-unknown-linux-musl-21.1.8` failing to link:
   `crtbeginT.o: relocation R_X86_64_32 against hidden symbol '__TMC_END__' can not
   be used when making a shared object`. A deterministic upstream `pkgsStatic` musl
   toolchain bug — **not** resource exhaustion: reproduced identically on a 2-core/7 GB
   GitHub runner and on rio (32 cores / 29 GB), at the same link step. Pre-existing;
   the derivation is byte-identical on `main` and on the bumped branch, so the bump
   did not cause it. (`rustc-static-…-1.95.0`, so the Rust 1.96 move is exonerated
   too.) The GitHub log's top frame, `ninja: build stopped: subcommand failed`, hides
   this one frame down — read to the bottom frame.

**`checks.build` is proven on darwin, blocked on linux.** Deliberate-break run on
`aarch64-darwin`, `compile_error!` added to `tatara-lisp/src/lib.rs`, same tree:

```
A) nix flake check --no-build   NO_BUILD_EXIT=0   ✅ checks.aarch64-darwin.build
B) nix flake check              BUILD_EXIT=1      ❌ checks.aarch64-darwin.build
   error: Cannot build '…-rust_tatara-lisp-0.2.5.drv'.
     > error: vacuity proof: checks.build must go red on this
     > 70 | compile_error!("vacuity proof: checks.build must go red on this");
```

So the guard has a demonstrated pass **and** a demonstrated true-negative on darwin.
The **linux half remains unproven** — it has never built green there, blocked behind
the toolchain bug above. Do not claim the guard is proven fleet-wide.

**Branch `ci/flake-check-actually-builds` @ `4d13e0e`** holds the flag removal plus
the substrate bump (`3c0a338` 2026-06-03 → `3e02a29`, 266 commits). It is **not
merged**: it fixes defect 1 and the cold-store eval failure, but cannot go green
while defect 2 stands. **Do not merge until defect 2 is resolved, and do not "fix"
CI by re-adding `--no-build`** — that flag is what hid all of this for two months.
The cost it was buying is real (56s vs 87 min) and the lever for that is a shared
binary cache, never the flag.

> **★★★ CSE / Knowable Construction.** This repo operates under **Constructive Substrate Engineering** — canonical specification at [`pleme-io/theory/CONSTRUCTIVE-SUBSTRATE-ENGINEERING.md`](https://github.com/pleme-io/theory/blob/main/CONSTRUCTIVE-SUBSTRATE-ENGINEERING.md). The Compounding Directive (operational rules: solve once, load-bearing fixes only, idiom-first, models stay current, direction beats velocity) is in the org-level pleme-io/CLAUDE.md ★★★ section. Read both before non-trivial changes.

Homoiconic S-expression reader + macroexpander + `#[derive(TataraDomain)]` proc macro — the pleme-io Lisp authoring surface, extracted from the `pleme-io/tatara` mono-workspace so downstream consumers (e.g. `cordel`) can git-dep it hermetically inside a Nix sandbox.

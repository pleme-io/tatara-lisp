# Tatara-Lisp

pending-vacuous-guard: tatara-lisp-flake-check

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

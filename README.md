# tatara-lisp

Homoiconic S-expression reader + macroexpander + `#[derive(TataraDomain)]`
proc macro — the pleme-io Lisp authoring surface, extracted from the
`pleme-io/tatara` mono-workspace so downstream consumers (e.g. `cordel`)
can git-dep it hermetically inside a Nix sandbox.

## Why standalone

The `pleme-io/tatara` workspace has 25+ crates with deep path-dep graphs;
`crate2nix generate` on any consumer that pulls a single member would
follow paths that escape the Nix sandbox. This repo contains exactly
what cordel needs:

  * `tatara-lisp`      — reader, `Sexp` AST, expander, domain registry
  * `tatara-lisp-derive` — `#[derive(TataraDomain)]` proc macro

Both kept in lockstep with the mono-workspace versions; when the API
evolves over there, re-sync here.

## Build

```sh
nix build         # via substrate rust-workspace-release
cargo build       # native toolchain
cargo test        # unit tests
```

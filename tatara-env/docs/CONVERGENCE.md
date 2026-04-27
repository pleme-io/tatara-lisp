# Convergence and the Env Lattice

> "Software is a declaration of desired state, a proof that the
> declaration is well-formed, a rendering of the declaration into
> an executable artifact, and a convergence of the running world
> toward the declared state."
>
> — `theory/THEORY.md` §I.1

This document is the operational counterpart to that theory, in
the small. `tatara-env` makes the **lattice view of convergence**
first-class; `tatara-rollout` makes the **process view of
convergence** first-class. Together they cover both halves of
THEORY §IV.1 and the eight-phase loop of §IV.3.

## The two views

| View | Question | Where it lives |
|---|---|---|
| **Lattice** (static) | What states can the system inhabit? | `tatara_env::lattice::Env::{meet, join, leq, bottom}` |
| **Process** (dynamic) | How does the system move between them? | `tatara_rollout::diff_envs` + the synthesizer pipeline |

Both views agree on the data structure — `tatara_env::Env`. Two
envs are "equal" iff their resource sets are byte-canonical
equivalent. Two envs are "comparable" iff one is `⊑` the other.
The motion from one to the other is a `Plan`.

## The lattice is structural

`Env` is a point in a powerset lattice over typed resources. The
algebra:

```
                    join(a, b)         (least upper bound)
                       /\
                      /  \
                     a    b           (the points)
                      \  /
                       \/
                    meet(a, b)         (greatest lower bound)
                       |
                       ⊥               (bottom — empty env)
```

Every operation in pleme-io that touches "the set of declared
resources" can be re-expressed in this algebra:

- **Drift detection**: `!(observed ⊑ declared)` — the running
  world has resources or content the declaration doesn't.
- **Compliance subset**: `baseline ⊑ env` — every required
  control is in the env. (`Env::satisfies_baseline` reads as
  exactly this.)
- **Region merging**: `production-base ⊔ region-us-east =
  production-us-east` — multi-region orchestration is algebraic.
- **Shared invariants**: `app-a ⊓ app-b` — resources both apps
  require, that must hold in every join taking them.
- **Diff** (rollout-side): `(adds, removes, changes)` is exactly
  the symmetric difference — adds = `new \ old`, removes = `old
  \ new`, changes = `(old ⊓ new) where content differs`.

## The process moves between lattice points

The eight-phase loop (THEORY §IV.3) maps onto the algebra:

| Phase | What | Lattice expression |
|---|---|---|
| DECLARE | Express desired state | `tatara_env::compile_into_env` produces `declared: Env` |
| SIMULATE | Zero-cost dry-run | `tatara_env::validate(&declared)` — type-level coherence |
| PROVE | Verify invariants | property tests assert lattice laws hold |
| REMEDIATE | Auto-fix violations | `declared ⊔ remediations` (right-biased) |
| RENDER | Produce artifacts | morphism `Env → FluxCD/Helm/Pangea` (next phase) |
| DEPLOY | Apply to infrastructure | `tatara_rollout::diff_envs(old, new)` → `Plan` |
| VERIFY | Confirm convergence | `observed ⊑ declared` (drift = `!leq`) |
| RECONVERGE | Detect drift, loop | when `drifts_from(observed, declared)`, GOTO DECLARE |

A controller is a fixed-point operator: `f(x_n, declared) = x_{n+1}`
until `f(x*, declared) = x*` and `x* ⊑ declared`. The convergence
property is **a strict assertion in the lattice**, not a fuzzy
notion.

## Lattice laws — the proof discipline

THEORY §I.3 belief 4: "every declaration has a proof." For the
env lattice, the proofs are the lattice laws. Every law is a
unit test in `lattice::tests::laws`, and the test file is the
canonical proof statement. If a future change to `meet` or
`join` violates a law, the test fails loudly, and the change
either fixes the regression or extends the algebra.

The ten laws asserted today:

1. `a ⊑ a` (reflexive)
2. `a ⊑ b ∧ b ⊑ a → a ≅ b` (antisymmetric)
3. `a ⊑ b ∧ b ⊑ c → a ⊑ c` (transitive)
4. `meet(a, a) = a`, `join(a, a) = a` (idempotent)
5. `meet(a, b) = meet(b, a)`, dual for `join` (commutative)
6. `meet(meet(a, b), c) = meet(a, meet(b, c))`, dual for `join`
   (associative)
7. `meet(a, join(a, b)) = a` (absorption)
8. `meet(⊥, a) = ⊥`, `join(⊥, a) = a` (bottom identity/absorber)
9. `meet(a, b) ⊑ a`, `meet(a, b) ⊑ b` (meet is lower bound)
10. `a ⊑ join(a, b)`, `b ⊑ join(a, b)` (join is upper bound)

These together establish that `Env` forms a **bounded lattice**
with `⊥` as the bottom. (We don't define `⊤` because the type
of "every possible resource" doesn't have a finite
representation, but every finite collection of envs has a join,
which is the finitary upper bound we need.)

## What's still to wire

- **Lisp keyword forms**: `(env-meet a b)`, `(env-join a b)`,
  `(env-leq? a b)`, `(env-drifts? observed declared)`. Pure
  syntactic sugar — the underlying algebra is fully implemented.
- **Per-domain `meet` / `join`**: today the algebra operates on
  resources as opaque JSON. A future phase lets each domain
  register its own structural meet (e.g. for `defbpf-policy`,
  the meet of two policies is a policy whose programs and maps
  are the meet of each).
- **Compliance lattice integration**: the
  `arch-synthesizer::ComplianceLattice` and this env lattice are
  the same shape but on different elements. Wiring them so
  `env ⊑ compliance-baseline` is a single check across both
  lattices is the natural unification.
- **Topological sorting of plan**: `tatara-rollout` orders
  removes-then-adds-then-changes, but per-resource dependencies
  (a `defciliumnetworkpolicy` references a `defservice`) need
  a real topological sort so deploys land in the right order.

Each of these is additive — none changes the foundation, all
extend the algebra.

//! Rule `git-mutation-discard`: a discarded `(exec-check …)` / `(exec-capture …)`
//! whose command is a `git` mutation is a silent failure.
//!
//! `exec-check` returns the child's exit code and only *raises* on a spawn
//! failure (tatara-lisp-script `process.rs`); a non-zero exit is *returned*.
//! So a bare, result-discarding call to a state-changing git command silently
//! ignores failure — the engine of the 2026-06-03 auto-release version-skew
//! incident (an empty `git commit` / colliding `git tag` whose non-zero exit
//! was dropped, leaving a tag on an un-bumped HEAD). The remedy is the
//! `exec-or-die` stdlib wrapper or an explicit status check.

use tatara_lisp::{Spanned, SpannedForm};

use crate::{head_symbol, line_col, string_lit, suppressed, walk_consumption, Rule, Violation};

const RULE: &str = "git-mutation-discard";

/// git subcommands that change refs / worktree / remote state and whose silent
/// failure corrupts a release or repository. Read-only subcommands (`status`,
/// `rev-parse`, `describe`, `log`, `config`) and `add` are intentionally
/// excluded: discarding their result is common and benign, and per-path `add`
/// tolerance is the documented norm.
const GIT_MUTATIONS: &[&str] = &[
    "commit",
    "tag",
    "push",
    "merge",
    "rebase",
    "reset",
    "cherry-pick",
    "revert",
    "am",
    "stash",
];

/// Flags discarded results of state-changing git commands. See module docs.
pub struct GitMutationResultDiscarded;

impl Rule for GitMutationResultDiscarded {
    fn name(&self) -> &'static str {
        RULE
    }

    fn description(&self) -> &'static str {
        "a discarded exec-check/exec-capture of a git mutation (commit/tag/push/…) silently ignores failure"
    }

    fn check(&self, forms: &[Spanned], src: &str) -> Vec<Violation> {
        let mut out = Vec::new();
        for form in forms {
            walk_consumption(form, false, &mut |node, consumed| {
                if consumed {
                    return;
                }
                let SpannedForm::List(items) = &node.form else {
                    return;
                };
                let Some(sub) = git_mutation_subcommand(items) else {
                    return;
                };
                if suppressed(src, node.span.start, RULE) {
                    return;
                }
                let (line, col) = line_col(src, node.span.start);
                out.push(Violation {
                    rule: RULE,
                    line,
                    col,
                    message: discarded_message(sub),
                });
            });
        }
        out
    }
}

/// `Some(subcommand)` iff `items` is an `(exec-check|exec-capture "git" "<mut>" …)`
/// call whose subcommand mutates repository state.
fn git_mutation_subcommand(items: &[Spanned]) -> Option<&'static str> {
    let head = head_symbol(items)?;
    if head != "exec-check" && head != "exec-capture" {
        return None;
    }
    if string_lit(items.get(1)?)? != "git" {
        return None;
    }
    let sub = string_lit(items.get(2)?)?;
    GIT_MUTATIONS.iter().copied().find(|m| *m == sub)
}

fn discarded_message(sub: &str) -> String {
    let mut m = String::from("discarded result of `git ");
    m.push_str(sub);
    m.push_str("` — exec-check/exec-capture returns the exit code (it never raises on a non-zero child), so this silently ignores failure (the auto-release version-skew class). Bind + check the status, or use the `exec-or-die` stdlib wrapper. If the discard is intentional, annotate it with `;; lint:allow git-mutation-discard <reason>`.");
    m
}

//! Rule family `<command>-mutation-discard`: a discarded `(exec-check …)` /
//! `(exec-capture …)` of a state-changing CLI command silently ignores failure.
//!
//! `exec-check` returns the child's exit code and only *raises* on a spawn
//! failure (tatara-lisp-script `process.rs`); a non-zero exit is *returned*.
//! So a bare, result-discarding call to a state-changing command silently
//! ignores failure — the engine of the 2026-06-03 auto-release version-skew
//! incident (an empty `git commit` / colliding `git tag` whose non-zero exit
//! was dropped) and of every "the PR/release/secret silently never happened"
//! class. The remedy is the `exec-or-die` stdlib wrapper or a status check.
//!
//! One [`CommandMutationDiscarded`] handles every command family: it is
//! parameterized over the command (`git`, `gh`, …) and the set of mutating
//! subcommand token-sequences, so adding a family is one constructor + one
//! line in [`crate::default_rules`].

use tatara_lisp::{Spanned, SpannedForm};

use crate::{head_symbol, line_col, string_lit, suppressed, walk_consumption, Rule, Violation};

/// A discarded result of a state-changing `command <subcommand…>` invocation
/// via `exec-check` / `exec-capture`.
pub struct CommandMutationDiscarded {
    name: &'static str,
    description: &'static str,
    command: &'static str,
    /// Each entry is a sequence of literal arg tokens that must immediately
    /// follow the command for the call to count as a mutation — `["commit"]`
    /// for `git commit`, `["pr", "create"]` for `gh pr create`.
    mutations: &'static [&'static [&'static str]],
}

/// git subcommands that change refs / worktree / remote state. Read-only
/// subcommands (`status`, `rev-parse`, `describe`, `log`, `config`) and `add`
/// are excluded: discarding their result is common and benign, and per-path
/// `add` tolerance is the documented norm.
const GIT_MUTATIONS: &[&[&str]] = &[
    &["commit"],
    &["tag"],
    &["push"],
    &["merge"],
    &["rebase"],
    &["reset"],
    &["cherry-pick"],
    &["revert"],
    &["am"],
    &["stash"],
];

/// `gh` operations whose primary effect is a creation / state change — a
/// failed one silently means "the PR/release/secret never happened". Read-only
/// (`gh pr view`, `gh issue list`, `gh api` GETs) and secondary edits
/// (`gh pr comment/edit/close`) are intentionally excluded for now to keep the
/// rule high-signal; promote them when the fleet is clean for the create set.
const GH_MUTATIONS: &[&[&str]] = &[
    &["pr", "create"],
    &["pr", "merge"],
    &["release", "create"],
    &["release", "upload"],
    &["release", "edit"],
    &["release", "delete"],
    &["issue", "create"],
    &["repo", "create"],
    &["secret", "set"],
    &["variable", "set"],
    &["workflow", "run"],
];

/// Rule: discarded result of a state-changing `git` command.
#[must_use]
pub fn git_mutation_discarded() -> CommandMutationDiscarded {
    CommandMutationDiscarded {
        name: "git-mutation-discard",
        description: "a discarded exec-check/exec-capture of a git mutation (commit/tag/push/…) silently ignores failure",
        command: "git",
        mutations: GIT_MUTATIONS,
    }
}

/// Rule: discarded result of a state-changing `gh` command.
#[must_use]
pub fn gh_mutation_discarded() -> CommandMutationDiscarded {
    CommandMutationDiscarded {
        name: "gh-mutation-discard",
        description: "a discarded exec-check/exec-capture of a gh mutation (pr create, release create, secret set, …) silently ignores failure",
        command: "gh",
        mutations: GH_MUTATIONS,
    }
}

impl Rule for CommandMutationDiscarded {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
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
                let Some(seq) = self.matched_mutation(items) else {
                    return;
                };
                if suppressed(src, node.span.start, self.name) {
                    return;
                }
                let (line, col) = line_col(src, node.span.start);
                out.push(Violation {
                    rule: self.name,
                    line,
                    col,
                    message: self.message(&seq),
                });
            });
        }
        out
    }
}

impl CommandMutationDiscarded {
    /// `Some("subcommand tokens")` iff `items` is an
    /// `(exec-check|exec-capture "<command>" <mutation tokens…> …)` call.
    fn matched_mutation(&self, items: &[Spanned]) -> Option<String> {
        let head = head_symbol(items)?;
        if head != "exec-check" && head != "exec-capture" {
            return None;
        }
        if string_lit(items.get(1)?)? != self.command {
            return None;
        }
        for seq in self.mutations {
            let all_match = seq
                .iter()
                .enumerate()
                .all(|(i, tok)| items.get(2 + i).and_then(string_lit) == Some(*tok));
            if all_match {
                return Some(seq.join(" "));
            }
        }
        None
    }

    fn message(&self, seq: &str) -> String {
        let mut m = String::from("discarded result of `");
        m.push_str(self.command);
        m.push(' ');
        m.push_str(seq);
        m.push_str("` — exec-check/exec-capture returns the exit code (it never raises on a non-zero child), so this silently ignores failure. Bind + check the status, or use the `exec-or-die` stdlib wrapper. If the discard is intentional, annotate it with `;; lint:allow ");
        m.push_str(self.name);
        m.push_str(" <reason>`.");
        m
    }
}

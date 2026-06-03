//! Lint rules. Each rule is one submodule + one `impl Rule`. To ship a new
//! check fleet-wide: add `mod my_rule;`, `pub use my_rule::MyRule;`, and one
//! line in [`crate::default_rules`].

mod git_mutation_discard;

pub use git_mutation_discard::GitMutationResultDiscarded;

//! Lint rules. Most rules are instances of a parameterized rule type; to ship
//! a new check fleet-wide, add a constructor (or a new module) and one line in
//! [`crate::default_rules`].

mod mutation_discard;

pub use mutation_discard::{
    gh_mutation_discarded, git_mutation_discarded, CommandMutationDiscarded,
};

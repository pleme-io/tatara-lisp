//! Lint rules. Most rules are instances of a parameterized rule type; to ship
//! a new check fleet-wide, add a constructor (or a new module) and one line in
//! [`crate::default_rules`].

mod mutation_discard;
mod unbound_symbol;

pub use mutation_discard::{
    gh_mutation_discarded, git_mutation_discarded, CommandMutationDiscarded,
};
// NOT in `crate::default_rules`, deliberately: this rule needs the caller's
// interpreter environment injected (see `unbound_symbol`'s module docs — the
// source of truth must be the live interpreter, never a table in this crate),
// and `default_rules()` takes no arguments. The binary constructs it.
pub use unbound_symbol::{
    CatalogListing, special_cased_heads, unbound_symbol, Prescription, Shape, UnboundSymbol,
    SHAPES,
};

//! Built-in primitive procedures.
//!
//! Phase 2.2 scaffold: public list of names only. Implementation lands in
//! Phase 2.3 once the evaluator is wired up.
//!
//! The split between special forms (`special.rs`) and primitives (this
//! module) is that special forms receive their operands unevaluated and
//! control their own evaluation; primitives receive evaluated arguments
//! like any other function.

/// Names of primitive procedures the embedder will register in every
/// fresh interpreter. This list is the public contract — adding entries
/// is backwards-compatible; removing or renaming is not.
pub const PRIMITIVE_NAMES: &[&str] = &[
    // arithmetic
    "+",
    "-",
    "*",
    "/",
    "modulo",
    "quotient",
    "remainder",
    "abs",
    "min",
    "max",
    // comparison
    "=",
    "<",
    ">",
    "<=",
    ">=",
    // type predicates
    "null?",
    "pair?",
    "list?",
    "symbol?",
    "string?",
    "integer?",
    "number?",
    "boolean?",
    "procedure?",
    "foreign?",
    // lists
    "car",
    "cdr",
    "cons",
    "list",
    "length",
    "reverse",
    "append",
    "map",
    "filter",
    "fold",
    "for-each",
    // equality
    "eq?",
    "eqv?",
    "equal?",
    // strings
    "string-length",
    "string-append",
    "substring",
    "string->symbol",
    "symbol->string",
    "string->number",
    "number->string",
    // IO (sandboxable via embedder substitution)
    "display",
    "newline",
    "print",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for n in PRIMITIVE_NAMES {
            assert!(seen.insert(*n), "duplicate primitive: {n}");
        }
    }
}

//! Early reuse: a parameter read for the last time moves out of its frame.
//!
//! A copy-on-write primitive (`hash-map-set`, `cons`, …) updates its argument
//! in place only when it holds the sole reference. A parameter the body is
//! still bound to is a second reference, so outside tail position every update
//! copies. [`last_uses`] finds, for each parameter, the one symbol node that
//! reads it last; the evaluator moves the value out of the frame at that node
//! (`Env::take_marked`) instead of cloning it.
//!
//! The pass is deliberately small. It accepts a body made only of calls,
//! symbols, literals and quoted data, which the evaluator runs head first,
//! then arguments left to right, each subtree to completion. In such a body
//! the last occurrence in reading order is the last read at run time. Any
//! special form (a binding, a branch, a lambda that could capture the frame,
//! `set!`) declines the whole body, and a parameter used as a call head is
//! declined alone. Declining only ever keeps today's behaviour: the value is
//! cloned as before.
//!
//! Off unless the host turns it on (`Interpreter::set_early_reuse`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use tatara_lisp::{Atom, Spanned, SpannedForm};

use crate::special::SpecialForm;
use crate::value::Closure;

/// The symbol nodes of one closure body that move their parameter out.
#[derive(Debug, Default)]
pub struct Moves {
    nodes: HashSet<usize>,
}

impl Moves {
    #[inline]
    pub(crate) fn marks(&self, form: &Spanned) -> bool {
        self.nodes.contains(&node(form))
    }

    /// How many parameters move out somewhere in the body.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

fn node(form: &Spanned) -> usize {
    std::ptr::from_ref(form) as usize
}

/// Each parameter's last read in `body`, or `None` when the body is outside
/// what the pass reads or no parameter is read at all.
#[must_use]
pub fn last_uses(params: &[Arc<str>], body: &[Spanned]) -> Option<Moves> {
    let mut reads: Vec<(&str, usize)> = Vec::new();
    let mut heads: HashSet<&str> = HashSet::new();
    for form in body {
        walk(form, false, &mut reads, &mut heads)?;
    }
    let mut last: HashMap<&str, usize> = HashMap::new();
    for (name, at) in reads {
        last.insert(name, at);
    }
    let nodes: HashSet<usize> = params
        .iter()
        .map(|p| &**p)
        .filter(|p| !heads.contains(p))
        .filter_map(|p| last.get(p).copied())
        .collect();
    (!nodes.is_empty()).then_some(Moves { nodes })
}

fn walk<'a>(
    form: &'a Spanned,
    head: bool,
    reads: &mut Vec<(&'a str, usize)>,
    heads: &mut HashSet<&'a str>,
) -> Option<()> {
    match &form.form {
        SpannedForm::Nil | SpannedForm::Quote(_) => Some(()),
        SpannedForm::Atom(Atom::Symbol(name)) => {
            if head {
                heads.insert(name.as_str());
            }
            reads.push((name.as_str(), node(form)));
            Some(())
        }
        SpannedForm::Atom(_) => Some(()),
        SpannedForm::Quasiquote(_) | SpannedForm::Unquote(_) | SpannedForm::UnquoteSplice(_) => {
            None
        }
        SpannedForm::List(items) => {
            let Some((first, rest)) = items.split_first() else {
                return Some(());
            };
            if first
                .as_symbol()
                .is_some_and(|s| SpecialForm::from_symbol(s).is_some())
            {
                return None;
            }
            walk(first, true, reads, heads)?;
            for item in rest {
                walk(item, false, reads, heads)?;
            }
            Some(())
        }
    }
}

/// The move sets of the closures one interpreter has run, computed once per
/// closure. Keyed on the closure's address and checked against a weak handle,
/// so an address reused by a later closure is never answered from a dead one.
#[derive(Default)]
pub(crate) struct Cache {
    seen: Mutex<HashMap<usize, (Weak<Closure>, Option<Arc<Moves>>)>>,
}

const SWEEP_AT: usize = 4096;

impl Cache {
    pub(crate) fn moves_for(&self, closure: &Arc<Closure>) -> Option<Arc<Moves>> {
        let key = Arc::as_ptr(closure) as usize;
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((held, moves)) = seen.get(&key) {
            if held.upgrade().is_some_and(|c| Arc::ptr_eq(&c, closure)) {
                return moves.clone();
            }
        }
        if seen.len() >= SWEEP_AT {
            seen.retain(|_, (held, _)| held.strong_count() > 0);
        }
        let moves = last_uses(&closure.params, &closure.body).map(Arc::new);
        seen.insert(key, (Arc::downgrade(closure), moves.clone()));
        moves
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(src: &str) -> Vec<Spanned> {
        tatara_lisp::read_spanned(src).expect("reads")
    }

    fn params(names: &[&str]) -> Vec<Arc<str>> {
        names.iter().map(|n| Arc::from(*n)).collect()
    }

    #[test]
    fn the_last_read_of_each_parameter_is_marked() {
        let b = body("(list (hash-map-set m :a 1) m x)");
        let moves = last_uses(&params(&["m", "x"]), &b).expect("a call body");
        assert_eq!(moves.len(), 2);
    }

    #[test]
    fn a_special_form_anywhere_declines_the_body() {
        let b = body("(f (if c m 0))");
        assert!(last_uses(&params(&["m", "c"]), &b).is_none());
        let b = body("(f (lambda () m))");
        assert!(last_uses(&params(&["m"]), &b).is_none());
    }

    #[test]
    fn a_parameter_called_as_a_function_is_declined_alone() {
        let b = body("(g (k m))");
        let moves = last_uses(&params(&["m", "k"]), &b).expect("m still moves");
        assert_eq!(moves.len(), 1);
    }

    #[test]
    fn a_body_that_reads_no_parameter_has_no_moves() {
        let b = body("(f 1 2)");
        assert!(last_uses(&params(&["m"]), &b).is_none());
    }
}

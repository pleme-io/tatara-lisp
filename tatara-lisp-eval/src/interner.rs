//! Thread-local symbol interner.
//!
//! Lisp programs read the same names over and over — every use of
//! `+`, `if`, `cons`, every reference to a user-defined name. Without
//! interning, each occurrence allocates a fresh `Arc<str>`. With
//! interning, the first occurrence allocates and every subsequent
//! occurrence is a hash + Arc-clone. Equality of `Value::Symbol(a) ==
//! Value::Symbol(b)` becomes `Arc::ptr_eq(a, b)` for interned names —
//! O(1) instead of O(min(|a|, |b|)) byte comparison.
//!
//! Scope: thread-local. Each thread has its own interner. Interpreters
//! sharing a thread share the interner; cross-thread Lisp values stay
//! correct because `Arc<str>` is `Send + Sync` and equality falls back
//! to byte comparison when pointers differ.
//!
//! Memory: the interner only grows. Long-running embedders that load
//! and unload many programs can call `clear()` to release the table.
//! In practice tatara-lisp programs are bounded in symbol diversity,
//! so the table plateaus quickly.
//!
//! Threading: `RefCell<HashMap>` inside the thread-local. We're inside
//! a single Lisp eval at a time per thread, so re-entrant access (one
//! `intern` mid-flight when another fires) is impossible — the borrow
//! is tight, single-call. If we ever go to true multi-fiber on one
//! thread we'll revisit the cell type.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

thread_local! {
    static SYMBOL_INTERNER: RefCell<HashMap<Box<str>, Arc<str>>> = RefCell::new(HashMap::new());
}

/// Look up `s` in the thread-local interner; insert if missing.
/// Returns an `Arc<str>` shared with every other call interning the
/// same string. Use this in place of `Arc::from(s)` for any name
/// that's likely to recur — symbols, keywords, fn names, global
/// keys.
#[must_use]
pub fn intern(s: &str) -> Arc<str> {
    SYMBOL_INTERNER.with(|cell| {
        let mut table = cell.borrow_mut();
        if let Some(arc) = table.get(s) {
            return Arc::clone(arc);
        }
        let arc: Arc<str> = Arc::from(s);
        table.insert(s.to_owned().into_boxed_str(), Arc::clone(&arc));
        arc
    })
}

/// Clear the thread-local interner. Releases all interned `Arc<str>`
/// pointers from the table — but live `Value::Symbol(arc)` references
/// keep their content via the Arc refcount, so values created before
/// `clear()` remain valid. Useful for embedders running many
/// short-lived programs that don't want unbounded growth.
pub fn clear() {
    SYMBOL_INTERNER.with(|cell| cell.borrow_mut().clear());
}

/// Current size of the interner table. Diagnostic only.
#[must_use]
pub fn size() -> usize {
    SYMBOL_INTERNER.with(|cell| cell.borrow().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_shared_arc() {
        let a = intern("foo");
        let b = intern("foo");
        assert!(Arc::ptr_eq(&a, &b), "same name must share storage");
    }

    #[test]
    fn intern_distinguishes_distinct_names() {
        let a = intern("foo");
        let b = intern("bar");
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(&*a, "foo");
        assert_eq!(&*b, "bar");
    }

    #[test]
    fn intern_grows_table() {
        clear();
        assert_eq!(size(), 0);
        let _ = intern("alpha");
        let _ = intern("beta");
        let _ = intern("alpha"); // dedupe
        assert_eq!(size(), 2);
    }

    #[test]
    fn clear_drops_table_but_arcs_survive() {
        let a = intern("persists");
        clear();
        // The Arc returned earlier is still valid; the table is gone.
        assert_eq!(&*a, "persists");
        // A fresh intern of the same name returns a different Arc
        // (the old Arc isn't in the table anymore).
        let b = intern("persists");
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(&*a, &*b);
    }
}

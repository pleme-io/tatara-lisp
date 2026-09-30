//! The persistent collections behind `Value::List` and `Value::Map`.
//!
//! **Why.** Both used to be `Arc<Vec<Value>>` / `Arc<HashMap<MapKey, Value>>`
//! with copy-on-write, and the `&[Value]` calling convention means an argument
//! is almost never uniquely held — the caller's binding is a second reference —
//! so every "update" copied the whole collection. Building a list with `append`
//! or a map with `hash-map-set` in a fold was quadratic: 1.04 / 2.49 / 10.13 s
//! for 10k / 20k / 40k list elements, 1.00 / 3.78 / 15.05 s for map entries
//! (tatara-script at 0.3.64, release). An RRB vector and a HAMT share
//! structure between versions, so the copy an update makes is O(log n) nodes,
//! not O(n) elements: 0.05 / 0.09 / 0.18 s and 0.05 / 0.11 / 0.23 s after.
//!
//! **Value semantics are unchanged.** Each version is immutable once another
//! reference can see it; an update produces a new version and leaves the old
//! one intact, exactly as the copy did. `tests/persistent.rs` holds that
//! against a model of the old representation on a seeded corpus.
//!
//! **A typed surface over `imbl`**, so no consumer names the third-party type:
//! the representation can change again without touching them. The outer `Arc`
//! in `Value` stays, because the interpreter reads a collection's uniqueness
//! (`Value::is_unique`, the frame-release walk in `env.rs`) and an in-place
//! update of a uniquely held version is still the cheapest update of all.

use std::fmt;
use std::ops::Index;

use crate::value::{MapKey, Value};

/// A persistent vector: O(1) clone, amortized O(1) `push_back`, O(log n)
/// indexed read, split and concatenation.
#[derive(Clone, Default)]
pub struct List(imbl::Vector<Value>);

impl List {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> imbl::vector::Iter<'_, Value, imbl::shared_ptr::DefaultSharedPtr> {
        self.0.iter()
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.0.get(index)
    }

    #[must_use]
    pub fn first(&self) -> Option<&Value> {
        self.0.front()
    }

    #[must_use]
    pub fn last(&self) -> Option<&Value> {
        self.0.back()
    }

    pub fn push_back(&mut self, value: Value) {
        self.0.push_back(value);
    }

    pub fn push_front(&mut self, value: Value) {
        self.0.push_front(value);
    }

    /// Append every element of `other`, sharing its structure.
    ///
    /// A short `other` is pushed element by element: RRB concatenation is
    /// O(log n) but with a large constant, and `(append acc (list x))` — the
    /// shape a fold builds with — appends one element at a time. Measured on
    /// that fold at 40k elements (release): 1.48 s concatenating every
    /// time, 0.18 s with this path.
    pub fn append(&mut self, other: List) {
        if self.0.is_empty() {
            self.0 = other.0;
        } else if other.0.len() <= 32 {
            self.0.extend(other.0);
        } else {
            self.0.append(other.0);
        }
    }

    /// The elements from `start` on, sharing structure with `self`.
    #[must_use]
    pub fn skip(&self, start: usize) -> List {
        List(self.0.skip(start.min(self.0.len())))
    }

    /// The first `count` elements, sharing structure with `self`.
    #[must_use]
    pub fn take(&self, count: usize) -> List {
        List(self.0.take(count.min(self.0.len())))
    }

    #[must_use]
    pub fn to_vec(&self) -> Vec<Value> {
        self.0.iter().cloned().collect()
    }

    /// Whether the two share one representation — the cheap identity test
    /// `eq?` answers with.
    #[must_use]
    pub fn ptr_eq(&self, other: &List) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl Index<usize> for List {
    type Output = Value;
    fn index(&self, index: usize) -> &Value {
        &self.0[index]
    }
}

impl From<Vec<Value>> for List {
    fn from(v: Vec<Value>) -> Self {
        List(v.into_iter().collect())
    }
}

impl FromIterator<Value> for List {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        List(iter.into_iter().collect())
    }
}

impl Extend<Value> for List {
    fn extend<I: IntoIterator<Item = Value>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl<'a> IntoIterator for &'a List {
    type Item = &'a Value;
    type IntoIter = imbl::vector::Iter<'a, Value, imbl::shared_ptr::DefaultSharedPtr>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl IntoIterator for List {
    type Item = Value;
    type IntoIter = imbl::vector::ConsumingIter<Value, imbl::shared_ptr::DefaultSharedPtr>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl fmt::Debug for List {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.iter()).finish()
    }
}

/// A persistent hash map (HAMT): O(1) clone, O(log n) insert, remove and
/// lookup. Iteration order is unspecified, as it was for the `HashMap` it
/// replaces.
#[derive(Clone, Default)]
pub struct Map(imbl::HashMap<MapKey, Value>);

impl Map {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn get(&self, key: &MapKey) -> Option<&Value> {
        self.0.get(key)
    }

    #[must_use]
    pub fn contains_key(&self, key: &MapKey) -> bool {
        self.0.contains_key(key)
    }

    pub fn insert(&mut self, key: MapKey, value: Value) -> Option<Value> {
        self.0.insert(key, value)
    }

    pub fn remove(&mut self, key: &MapKey) -> Option<Value> {
        self.0.remove(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&MapKey, &Value)> + '_ {
        self.0.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &MapKey> + '_ {
        self.0.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> + '_ {
        self.0.values()
    }
}

impl IntoIterator for Map {
    type Item = (MapKey, Value);
    type IntoIter =
        imbl::hashmap::ConsumingIter<(MapKey, Value), imbl::shared_ptr::DefaultSharedPtr>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl FromIterator<(MapKey, Value)> for Map {
    fn from_iter<I: IntoIterator<Item = (MapKey, Value)>>(iter: I) -> Self {
        Map(iter.into_iter().collect())
    }
}

impl Extend<(MapKey, Value)> for Map {
    fn extend<I: IntoIterator<Item = (MapKey, Value)>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl From<std::collections::HashMap<MapKey, Value>> for Map {
    fn from(m: std::collections::HashMap<MapKey, Value>) -> Self {
        m.into_iter().collect()
    }
}

impl fmt::Debug for Map {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

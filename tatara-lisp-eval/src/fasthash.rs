//! The hasher behind every environment frame: a multiply-rotate hash over the
//! name's bytes, eight at a time.
//!
//! A frame is looked up by a binding's name on every variable read, and the
//! standard `RandomState` (SipHash-1-3) was a quarter of a call-heavy
//! program's time. Names are short and come from the program itself, so a
//! keyed hash buys nothing here: a program that chose colliding names could
//! only slow itself down.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

/// A multiply-rotate hasher (the shape rustc's own interner uses).
#[derive(Clone, Copy, Debug, Default)]
pub struct FastHasher {
    hash: u64,
}

impl FastHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (words, tail) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        if let Some(len) = u8::try_from(tail.len()).ok().filter(|&n| n > 0) {
            let mut word = [0u8; 8];
            word[..tail.len()].copy_from_slice(tail);
            word[7] = len;
            self.add(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// A `HashMap` hashed with [`FastHasher`].
pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    const GOLDEN: [u64; 5] = [
        9_963_731_552_681_677_918,
        7_520_213_095_400_374_777,
        4_208_978_898_528_913_939,
        4_208_978_898_528_913_939,
        5_623_186_593_915_025_326,
    ];

    fn hash_of(s: &str) -> u64 {
        let mut h = FastHasher::default();
        s.hash(&mut h);
        h.finish()
    }

    #[test]
    fn names_that_differ_hash_apart() {
        let names = [
            "",
            "a",
            "b",
            "ab",
            "ba",
            "fib",
            "fib1",
            "x_0",
            "x_1",
            "abcdefgh",
            "abcdefghi",
            "abcdefgi",
        ];
        let hashes: std::collections::HashSet<u64> = names.iter().map(|n| hash_of(n)).collect();
        assert_eq!(hashes.len(), names.len());
    }

    fn words(f: impl FnOnce(&mut FastHasher)) -> u64 {
        let mut h = FastHasher::default();
        f(&mut h);
        h.finish()
    }

    #[test]
    fn the_hash_is_pinned() {
        assert_eq!(words(|h| h.write(b"fib")), GOLDEN[0]);
        assert_eq!(words(|h| h.write(b"abcdefghij")), GOLDEN[1]);
        assert_eq!(words(|h| h.write_u8(7)), GOLDEN[2]);
        assert_eq!(words(|h| h.write_usize(7)), GOLDEN[3]);
        assert_eq!(hash_of("lookup"), GOLDEN[4]);
        assert_eq!(words(|h| h.write(b"abcdefgh")), 16_295_004_723_668_454_517);
        assert_eq!(words(|h| h.write(b"")), 0);
    }

    #[test]
    fn a_short_tail_is_told_from_the_same_bytes_padded() {
        assert_ne!(words(|h| h.write(b"a")), words(|h| h.write(b"a\0")));
        assert_ne!(words(|h| h.write_u8(1)), words(|h| h.write_u8(2)));
        assert_ne!(words(|h| h.write_usize(1)), words(|h| h.write_usize(2)));
    }

    #[test]
    fn the_same_name_hashes_the_same() {
        assert_eq!(hash_of("lookup"), hash_of("lookup"));
    }

    #[test]
    fn a_map_finds_what_it_holds() {
        let mut m: FastMap<std::sync::Arc<str>, i32> = FastMap::default();
        m.insert("one".into(), 1);
        m.insert("two".into(), 2);
        assert_eq!(m.get("one"), Some(&1));
        assert_eq!(m.get("two"), Some(&2));
        assert_eq!(m.get("three"), None);
    }
}

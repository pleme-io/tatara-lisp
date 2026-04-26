//! Cache abstraction — separates "we already fetched this URL" from the
//! fetcher itself. Hosts can plug an in-memory, file-backed, or
//! cluster-wide cache.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Cache contract. Keys are stable per-URL strings (see
/// [`crate::Source::cache_key`]).
pub trait Cache {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn put(&mut self, key: String, value: Vec<u8>);
}

/// In-memory cache — useful for one-off scripts and tests.
#[derive(Debug, Default)]
pub struct MemoryCache {
    inner: Mutex<HashMap<String, Vec<u8>>>,
}

impl Cache for MemoryCache {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.inner.lock().ok()?.get(key).cloned()
    }

    fn put(&mut self, key: String, value: Vec<u8>) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(key, value);
        }
    }
}

/// File-system-backed cache — used by `tatara-script` host-side.
///
/// Layout:
/// ```text
/// <root>/
/// ├── manifest.json         { url-cache-key → blake3-hex }
/// └── sources/<blake3>/data raw bytes
/// ```
///
/// On lookup, hash the URL → key → blake3, then read `sources/<blake3>/data`.
/// On insert, write the bytes to `sources/<blake3>/data` and record the
/// mapping in `manifest.json`.
pub struct FileCache {
    pub root: PathBuf,
    in_memory: HashMap<String, Vec<u8>>, // hot cache for the current process
}

impl FileCache {
    /// Construct a FileCache backed by `<root>/sources/`.
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("sources"))?;
        Ok(Self { root, in_memory: HashMap::new() })
    }

    fn data_path(&self, blake3: &str) -> PathBuf {
        self.root.join("sources").join(blake3).join("data")
    }
}

impl Cache for FileCache {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        if let Some(bytes) = self.in_memory.get(key) {
            return Some(bytes.clone());
        }
        // Fall back to disk: the manifest maps URL→blake3, but for
        // simplicity FileCache hashes the key itself rather than a
        // separate manifest. blake3 of the key acts as the file-name.
        let key_hash = crate::blake3_hex(key.as_bytes());
        let p = self.data_path(&key_hash);
        std::fs::read(p).ok()
    }

    fn put(&mut self, key: String, value: Vec<u8>) {
        self.in_memory.insert(key.clone(), value.clone());
        let key_hash = crate::blake3_hex(key.as_bytes());
        let p = self.data_path(&key_hash);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_cache_round_trip() {
        let mut c = MemoryCache::default();
        assert_eq!(c.get("k"), None);
        c.put("k".into(), b"hello".to_vec());
        assert_eq!(c.get("k").as_deref(), Some(b"hello".as_ref()));
    }

    #[test]
    fn file_cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = FileCache::new(dir.path()).unwrap();
        assert_eq!(c.get("github:foo"), None);
        c.put("github:foo".into(), b"sexp bytes".to_vec());
        assert_eq!(c.get("github:foo").as_deref(), Some(b"sexp bytes".as_ref()));
    }

    #[test]
    fn file_cache_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut c = FileCache::new(dir.path()).unwrap();
            c.put("github:bar".into(), b"hello".to_vec());
        }
        // New instance — should hit the on-disk file.
        let c2 = FileCache::new(dir.path()).unwrap();
        assert_eq!(c2.get("github:bar").as_deref(), Some(b"hello".as_ref()));
    }
}

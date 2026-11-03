//! Facts cached per model under .derive/cache, keyed by a hash of the compiled SQL,
//! the upstream facts, the config and the Derive version, so an edit re-analyzes only
//! the affected subgraph. One file per model: a changed key overwrites the old entry.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};

pub struct Cache {
    dir: PathBuf,
    enabled: bool,
}

#[derive(Serialize, Deserialize)]
struct Entry<T> {
    key: String,
    value: T,
}

/// Incremental hasher for cache keys.
pub struct KeyBuilder(blake3::Hasher);

impl Default for KeyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyBuilder {
    pub fn new() -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"derive-cache-v1\0");
        h.update(env!("CARGO_PKG_VERSION").as_bytes());
        KeyBuilder(h)
    }
    pub fn add(&mut self, part: &[u8]) -> &mut Self {
        self.0.update(&(part.len() as u64).to_le_bytes());
        self.0.update(part);
        self
    }
    pub fn add_json<T: Serialize>(&mut self, v: &T) -> &mut Self {
        let bytes = serde_json::to_vec(v).unwrap_or_default();
        self.add(&bytes)
    }
    pub fn finish(&self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

impl Cache {
    pub fn new(project_root: &Path, enabled: bool) -> Self {
        Cache {
            dir: project_root.join(".derive").join("cache"),
            enabled,
        }
    }

    pub fn disabled() -> Self {
        Cache {
            dir: PathBuf::new(),
            enabled: false,
        }
    }

    fn path(&self, id: &str) -> PathBuf {
        let name = blake3::hash(id.as_bytes()).to_hex();
        self.dir.join(format!("{}.json", &name[..32]))
    }

    pub fn get<T: DeserializeOwned>(&self, id: &str, key: &str) -> Option<T> {
        if !self.enabled {
            return None;
        }
        let bytes = std::fs::read(self.path(id)).ok()?;
        let e: Entry<T> = serde_json::from_slice(&bytes).ok()?;
        (e.key == key).then_some(e.value)
    }

    pub fn put<T: Serialize>(&self, id: &str, key: &str, value: &T) {
        if !self.enabled {
            return;
        }
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let entry = Entry {
            key: key.to_string(),
            value,
        };
        if let Ok(bytes) = serde_json::to_vec(&entry) {
            // Write then rename, so a crash never leaves a torn entry.
            let path = self.path(id);
            let tmp = path.with_extension(format!("tmp{}", std::process::id()));
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_key_mismatch() {
        let dir = std::env::temp_dir().join(format!("derive-cache-test-{}", std::process::id()));
        let c = Cache::new(&dir, true);
        c.put("model.a", "k1", &vec![1, 2, 3]);
        assert_eq!(c.get::<Vec<i32>>("model.a", "k1"), Some(vec![1, 2, 3]));
        assert_eq!(c.get::<Vec<i32>>("model.a", "k2"), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}

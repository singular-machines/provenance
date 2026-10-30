//! Small JSON caches under `~/.cache/provenance/`. A missing, unreadable or
//! outdated cache is ignored and rebuilt, so deleting the folder is always safe.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Bump when any cached shape changes, so old files are ignored.
const VERSION: &str = "v1";

pub fn dir(home: &Path) -> PathBuf {
    home.join(".cache/provenance")
}

pub fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}-{VERSION}.json"))
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Write then rename, so a concurrent reader never sees half a file. Errors
/// are ignored: a cache that can't be written only costs speed.
pub fn write<T: Serialize>(path: &Path, value: &T) {
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_vec(value) else {
        return;
    };
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Stable short key for a string, for cache file names.
pub fn key(s: &str) -> String {
    // FNV-1a: stable across runs and Rust versions, unlike `DefaultHasher`.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ignores_garbage() {
        let dir = std::env::temp_dir().join(format!("prov-cache-{}", std::process::id()));
        let p = path(&dir, "x");
        write(&p, &vec![1, 2, 3]);
        assert_eq!(read::<Vec<i32>>(&p), Some(vec![1, 2, 3]));
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(read::<Vec<i32>>(&p), None);
        assert_eq!(key("author:@me"), key("author:@me"));
        assert_ne!(key("a"), key("b"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

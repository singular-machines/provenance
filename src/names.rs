//! Machine-local exact session bindings. Folder hints never select delivery.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::{send::Sender, sessions::Harness};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub display_name: String,
    pub harness: Harness,
    pub session_id: String,
    pub registered_at: DateTime<Utc>,
    pub cwd_hint: String,
}
impl Binding {
    pub fn identity(&self) -> Sender {
        Sender {
            harness: self.harness,
            session_id: self.session_id.clone(),
        }
    }
}
pub fn identity(s: &Sender) -> String {
    format!(
        "{}/{}",
        serde_json::to_value(s.harness).unwrap().as_str().unwrap(),
        s.session_id
    )
}
pub fn canonical(name: &str) -> Result<String> {
    let mut out = String::new();
    let mut separator = false;
    for c in name.trim().chars() {
        if c == ' ' || c == '-' {
            if !separator {
                out.push('-');
            }
            separator = true;
        } else if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            separator = false;
        } else {
            bail!(
                "unsupported character in name: {c:?}; use ASCII letters, digits, spaces or hyphens"
            );
        }
    }
    if out.is_empty() || out.len() > 64 {
        bail!("name must contain 1 to 64 characters after normalising");
    }
    Ok(out)
}
#[derive(Serialize, Deserialize)]
struct Registry {
    version: u32,
    names: BTreeMap<String, Binding>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            version: 1,
            names: BTreeMap::new(),
        }
    }
}
pub struct Store {
    dir: PathBuf,
}
impl Store {
    pub fn new(home: &Path) -> Self {
        Self {
            dir: crate::state::root(home),
        }
    }
    fn prepare(&self) -> Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        if fs::symlink_metadata(&self.dir)?.file_type().is_symlink() {
            bail!("registry directory must not be a symlink");
        }
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }
    fn lock(&self) -> Result<File> {
        self.prepare()?;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.dir.join("names.lock"))?;
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        f.lock()?;
        Ok(f)
    }
    fn read(&self) -> Result<Registry> {
        let path = self.dir.join("names.json");
        let mut f = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
            Err(e) => return Err(e.into()),
        };
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        let r: Registry = serde_json::from_str(&s)
            .context("reading names registry; refusing to overwrite invalid data")?;
        if r.version != 1 {
            bail!("unsupported names registry version {}", r.version);
        }
        Ok(r)
    }
    fn write(&self, r: &Registry) -> Result<()> {
        let tmp = self
            .dir
            .join(format!("names.{}.tmp", crate::send::new_id()));
        let result = (|| -> Result<()> {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(r)?)?;
            f.sync_all()?;
            fs::rename(&tmp, self.dir.join("names.json"))?;
            File::open(&self.dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
    pub fn list(&self) -> Result<BTreeMap<String, Binding>> {
        let _lock = self.lock()?;
        Ok(self.read()?.names)
    }
    pub fn get(&self, name: &str) -> Result<(String, Binding)> {
        let key = canonical(name)?;
        let binding=self.list()?.remove(&key).with_context(|| format!("name @{key} is unregistered; ask the owner to register the intended session with `provenance register \"{}\"`",name.trim()))?;
        Ok((key, binding))
    }
    pub fn register(
        &self,
        name: &str,
        me: &Sender,
        cwd: &str,
        replace: Option<&str>,
    ) -> Result<(String, Binding)> {
        if !matches!(me.harness, Harness::ClaudeCode | Harness::Codex) {
            bail!("only locally identified Claude Code and Codex sessions can register");
        }
        let key = canonical(name)?;
        let _lock = self.lock()?;
        let mut r = self.read()?;
        if let Some(old) = r.names.get(&key) {
            if let Some(expected) = replace {
                if expected != identity(&old.identity()) {
                    bail!(
                        "@{key} changed: incumbent is {}; replacement refused",
                        identity(&old.identity())
                    );
                }
            } else if old.identity() != *me {
                bail!(
                    "@{key} belongs to {}; use --replace {} only with the owner's explicit request",
                    identity(&old.identity()),
                    identity(&old.identity())
                );
            }
            if old.identity() == *me {
                return Ok((key, old.clone()));
            }
        } else if replace.is_some() {
            bail!("@{key} is no longer registered; replacement refused");
        }
        let b = Binding {
            display_name: name.trim().into(),
            harness: me.harness,
            session_id: me.session_id.clone(),
            registered_at: Utc::now(),
            cwd_hint: cwd.into(),
        };
        r.names.insert(key.clone(), b.clone());
        self.write(&r)?;
        Ok((key, b))
    }
    pub fn remove(&self, name: &str, expected: &Sender) -> Result<()> {
        let key = canonical(name)?;
        let _lock = self.lock()?;
        let mut r = self.read()?;
        let b = r.names.get(&key).context("name is no longer registered")?;
        if b.identity() != *expected {
            bail!(
                "@{key} changed: incumbent is {}; removal refused",
                identity(&b.identity())
            );
        }
        r.names.remove(&key);
        self.write(&r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    fn home() -> PathBuf {
        let p = std::env::temp_dir().join(format!("prov-names-{}", crate::send::new_id()));
        fs::create_dir_all(&p).unwrap();
        p
    }
    fn sender(id: &str) -> Sender {
        Sender {
            harness: Harness::Codex,
            session_id: id.into(),
        }
    }
    #[test]
    fn normalises_without_stripping_punctuation() {
        assert_eq!(canonical("  PR  - Reviewer ").unwrap(), "pr-reviewer");
        for s in ["", "   ", "pr_review", "@name", "é", "pr\treviewer", "a/b"] {
            assert!(canonical(s).is_err(), "{s:?}");
        }
        assert!(canonical(&"a".repeat(65)).is_err());
        assert!(canonical(&"a".repeat(64)).is_ok());
    }
    #[test]
    fn guarded_lifecycle_and_private_versioned_storage() {
        let h = home();
        let store = Store::new(&h);
        let a = sender("a");
        let b = sender("b");
        let (_, first) = store.register("PR Reviewer", &a, "/hint", None).unwrap();
        let (_, again) = store
            .register("pr-reviewer", &a, "/different", None)
            .unwrap();
        assert_eq!(first.registered_at, again.registered_at);
        assert!(store.register("PR Reviewer", &b, "/hint", None).is_err());
        assert!(
            store
                .register("PR Reviewer", &b, "/hint", Some("codex/wrong"))
                .is_err()
        );
        store
            .register("PR Reviewer", &b, "/hint", Some("codex/a"))
            .unwrap();
        assert!(store.remove("PR Reviewer", &a).is_err());
        store.remove("PR Reviewer", &b).unwrap();
        assert!(
            store
                .get("PR Reviewer")
                .unwrap_err()
                .to_string()
                .contains("provenance register")
        );
        assert_eq!(
            fs::metadata(store.dir.join("names.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&store.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::write(store.dir.join("names.json"), r#"{"version":2,"names":{}}"#).unwrap();
        assert!(store.list().is_err());
        fs::remove_dir_all(h).unwrap();
    }
    #[test]
    fn concurrent_claims_and_compare_and_swap_replacements() {
        let h = home();
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let h = h.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    Store::new(&h)
                        .register("reviewer", &sender(&format!("s{i}")), "/", None)
                        .is_ok()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .filter(|b| *b)
                .count(),
            1
        );
        let old = Store::new(&h).get("reviewer").unwrap().1.identity();
        let expected = identity(&old);
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let h = h.clone();
                let expected = expected.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    Store::new(&h)
                        .register(
                            "reviewer",
                            &sender(&format!("new{i}")),
                            "/",
                            Some(&expected),
                        )
                        .is_ok()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .filter(|b| *b)
                .count(),
            1
        );
        fs::remove_dir_all(h).unwrap();
    }
}

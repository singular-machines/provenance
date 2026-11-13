//! The root directory for provenance's persistent state.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// Resolve the state root, honoring `PROVENANCE_STATE_DIR` when it is set.
pub fn root(home: &Path) -> PathBuf {
    root_from(home, override_value())
}

pub fn is_overridden() -> bool {
    override_value().is_some_and(|value| !value.is_empty())
}

fn root_from(home: &Path, value: Option<std::ffi::OsString>) -> PathBuf {
    match value {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".local/state/provenance"),
    }
}

pub fn validate() -> Result<()> {
    validate_value(override_value())
}

fn override_value() -> Option<std::ffi::OsString> {
    #[cfg(test)]
    if std::env::var_os("PROVENANCE_STATE_TEST_USE_ENV").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        // Unit-test fixtures isolate state by supplying HOME. They do not
        // inherit a developer's process-wide state override unless a child
        // test opts into it explicitly.
        return None;
    }
    std::env::var_os("PROVENANCE_STATE_DIR")
}

fn validate_value(value: Option<std::ffi::OsString>) -> Result<()> {
    match value {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                bail!("PROVENANCE_STATE_DIR must be an absolute path");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_state_override_is_rejected() {
        let error = validate_value(Some("relative".into())).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("PROVENANCE_STATE_DIR must be an absolute path")
        );
    }

    #[test]
    fn state_defaults_to_home_when_override_is_unset() {
        assert_eq!(
            root_from(Path::new("/home/test"), None),
            PathBuf::from("/home/test/.local/state/provenance")
        );
        assert_eq!(
            root_from(Path::new("/home/test"), Some("".into())),
            PathBuf::from("/home/test/.local/state/provenance")
        );
    }

    #[test]
    fn real_state_path_builders_use_the_override() {
        const CHILD: &str = "PROVENANCE_STATE_PATHS_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert_real_state_paths();
            return;
        }

        let suffix = crate::send::new_id();
        let state_dir = std::env::temp_dir().join(format!("prov-state-paths-{suffix}"));
        let home = std::env::temp_dir().join(format!("prov-state-home-{suffix}"));
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::tests::real_state_path_builders_use_the_override",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PROVENANCE_STATE_TEST_USE_ENV", "1")
            .env("PROVENANCE_STATE_DIR", &state_dir)
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::remove_dir_all(state_dir).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    fn assert_real_state_paths() {
        let home = Path::new("/home/fixture");
        let root = PathBuf::from(std::env::var_os("PROVENANCE_STATE_DIR").unwrap());
        assert_eq!(crate::live::live_dir(home), root.join("live"));
        let mail_dir = crate::send::mail_dir(home);
        assert_eq!(mail_dir, root.join("mail"));
        assert!(
            crate::send::mailbox(&mail_dir, crate::sessions::Harness::Codex, "test")
                .starts_with(&root)
        );
        let sender = crate::send::Sender {
            harness: crate::sessions::Harness::Codex,
            session_id: "fixture".into(),
        };
        let names = crate::names::Store::new(home);
        names
            .register("fixture", &sender, "/fixture", None)
            .unwrap();
        assert!(root.join("names.json").is_file());
        assert!(root.join("names.lock").is_file());
        assert_eq!(crate::relay::peers_path(home), root.join("peers.json"));
        let _lock = crate::send::forward_lock(&mail_dir).unwrap();
        assert!(root.join("mail/forward.lock").is_file());
        assert_eq!(crate::install::pull_agent_log(home), root.join("pull.log"));
    }
}

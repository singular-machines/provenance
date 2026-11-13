//! The root directory for provenance's persistent state.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// Resolve the state root, honoring `PROVENANCE_STATE_DIR` when it is set.
pub fn root(home: &Path) -> PathBuf {
    root_from(home, std::env::var_os("PROVENANCE_STATE_DIR"))
}

fn root_from(home: &Path, value: Option<std::ffi::OsString>) -> PathBuf {
    match value {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".local/state/provenance"),
    }
}

pub fn validate() -> Result<()> {
    validate_value(std::env::var_os("PROVENANCE_STATE_DIR"))
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
    fn state_override_is_used_for_each_state_kind() {
        let root = root_from(Path::new("/home/test"), Some("/tmp/prov-test".into()));
        for entry in [
            "live",
            "mail",
            "names.json",
            "names.lock",
            "peers.json",
            "pull.log",
        ] {
            assert_eq!(
                root.join(entry),
                PathBuf::from("/tmp/prov-test").join(entry)
            );
        }
    }
}

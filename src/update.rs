//! A once-a-day check for a newer release. The check runs in a detached
//! background process (`provenance update-check`) and saves the newest
//! release tag to the cache; a later run reads it and prints one line on
//! stderr, so a command never waits on the network.

use std::path::Path;
use std::process::{Command, Stdio};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::cache;

const REPO: &str = "https://github.com/singular-machines/provenance";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64);

impl Version {
    /// `1.2.3` or `v1.2.3`; pre-releases and anything else are `None`.
    fn parse(s: &str) -> Option<Self> {
        let mut parts = s.strip_prefix('v').unwrap_or(s).split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        let v = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(v)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Serialize, Deserialize)]
struct Checked {
    at: DateTime<Utc>,
    latest: Option<String>,
}

fn cache_path(home: &Path) -> std::path::PathBuf {
    cache::path(&cache::dir(home), "update-check")
}

fn quiet() -> bool {
    use std::io::IsTerminal;
    std::env::var_os("PROVENANCE_NO_UPDATE_CHECK").is_some()
        || std::env::var_os("CI").is_some()
        || !std::io::stderr().is_terminal()
}

/// The line to print after the command, if a newer release is known. Starts
/// a background check when the last one is a day old or missing.
pub fn notice(home: &Path) -> Option<String> {
    if quiet() {
        return None;
    }
    let path = cache_path(home);
    let checked = cache::read::<Checked>(&path);
    let now = Utc::now();
    if checked.as_ref().is_none_or(|c| due(c.at, now)) {
        // Mark the check as started first, so other runs don't start another.
        let latest = checked.as_ref().and_then(|c| c.latest.clone());
        cache::write(&path, &Checked { at: now, latest });
        spawn_check();
    }
    message(
        env!("CARGO_PKG_VERSION"),
        checked.as_ref()?.latest.as_deref()?,
    )
}

fn due(at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - at >= Duration::hours(24) || at > now
}

fn message(current: &str, latest: &str) -> Option<String> {
    let (cur, new) = (Version::parse(current)?, Version::parse(latest)?);
    (new > cur).then(|| {
        format!(
            "provenance {new} is available (you have {cur}): cargo install --git {REPO} --force"
        )
    })
}

fn spawn_check() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("update-check")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        // Own process group, so Ctrl-C in the terminal doesn't reach it.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}

/// (Run in the background.) Look up the newest release tag and save it. A
/// failed lookup keeps the previous answer; the next check is a day later.
pub fn run_check(home: &Path) {
    let out = Command::new("git")
        .args([
            "-c",
            "http.lowSpeedLimit=1",
            "-c",
            "http.lowSpeedTime=20",
            "ls-remote",
            "--tags",
            "--refs",
            REPO,
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let path = cache_path(home);
    let latest = match out {
        Ok(o) if o.status.success() => {
            newest_tag(&String::from_utf8_lossy(&o.stdout)).map(|v| v.to_string())
        }
        _ => cache::read::<Checked>(&path).and_then(|c| c.latest),
    };
    cache::write(
        &path,
        &Checked {
            at: Utc::now(),
            latest,
        },
    );
}

/// The highest `vX.Y.Z` in `git ls-remote --tags` output.
fn newest_tag(ls_remote: &str) -> Option<Version> {
    ls_remote
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1)?.strip_prefix("refs/tags/"))
        .filter_map(Version::parse)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_versions_only() {
        assert_eq!(Version::parse("v0.2.0"), Some(Version(0, 2, 0)));
        assert_eq!(Version::parse("1.10.3"), Some(Version(1, 10, 3)));
        assert_eq!(Version::parse("v1.0.0-rc.1"), None);
        assert_eq!(Version::parse("v1.0"), None);
        assert_eq!(Version::parse("v1.0.0.1"), None);
        assert_eq!(Version::parse("latest"), None);
    }

    #[test]
    fn picks_the_highest_tag_numerically() {
        let out = "a1\trefs/tags/v0.9.0\nb2\trefs/tags/v0.10.0\nc3\trefs/tags/v1.0.0-rc.1\nd4\trefs/tags/nightly\n";
        assert_eq!(newest_tag(out), Some(Version(0, 10, 0)));
        assert_eq!(newest_tag(""), None);
    }

    #[test]
    fn tells_only_about_newer_releases() {
        assert!(
            message("0.1.0", "0.2.0")
                .unwrap()
                .contains("0.2.0 is available")
        );
        assert_eq!(message("0.2.0", "0.2.0"), None);
        assert_eq!(message("0.3.0", "0.2.0"), None);
        assert_eq!(message("0.1.0", "garbage"), None);
    }

    #[test]
    fn checks_at_most_once_a_day() {
        let at = Utc::now();
        assert!(!due(at, at + Duration::hours(23)));
        assert!(due(at, at + Duration::hours(24)));
        // A clock that went backwards shouldn't stop checks for good.
        assert!(due(at, at - Duration::hours(1)));
    }
}

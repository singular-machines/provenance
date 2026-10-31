//! Session stamps in commit messages: writing them from a git hook, and
//! reading them back from a PR's commits.
//!
//! provenance writes `Agent-Session: <harness>/<id>` and `Agent-Machine: <name>`,
//! where `<harness>` is `claude-code`, `codex`, `opencode` or `pi`.
//! Claude Code on the web already writes `Claude-Session: https://claude.ai/code/session_…`.

use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};

use crate::sessions::Harness;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stamp {
    pub harness: Harness,
    pub session_id: String,
    pub machine: Option<String>,
    /// Link to the session, for sessions that have one (Claude Code on the web).
    pub url: Option<String>,
    pub committed: DateTime<Utc>,
}

/// Stamps found in one commit message.
pub fn parse(message: &str, committed: DateTime<Utc>) -> Vec<Stamp> {
    let mut machine = None;
    let mut found: Vec<Stamp> = Vec::new();
    for line in message.lines().map(str::trim) {
        if let Some(m) = line.strip_prefix("Agent-Machine:") {
            machine = Some(m.trim().to_string()).filter(|m| !m.is_empty());
        } else if let Some(v) = line.strip_prefix("Agent-Session:") {
            let Some((harness, id)) = v.trim().split_once('/') else {
                continue;
            };
            let harness = match harness {
                "claude-code" => Harness::ClaudeCode,
                "codex" => Harness::Codex,
                "opencode" => Harness::OpenCode,
                "pi" => Harness::Pi,
                _ => continue,
            };
            if !id.is_empty() && !id.contains(char::is_whitespace) {
                found.push(Stamp {
                    harness,
                    session_id: id.to_string(),
                    machine: None,
                    url: None,
                    committed,
                });
            }
        } else if let Some(v) = line.strip_prefix("Claude-Session:") {
            let url = v.trim();
            if let Some(id) = url.strip_prefix("https://claude.ai/code/") {
                found.push(Stamp {
                    harness: Harness::ClaudeCode,
                    session_id: id.to_string(),
                    machine: None,
                    url: Some(url.to_string()),
                    committed,
                });
            }
        }
    }
    for s in found.iter_mut().filter(|s| s.url.is_none()) {
        s.machine.clone_from(&machine);
    }
    found
}

/// Session IDs the harnesses pass to every command they run.
pub fn sessions_from_env(get: impl Fn(&str) -> Option<String>) -> Vec<(Harness, String)> {
    [
        (Harness::ClaudeCode, "CLAUDE_CODE_SESSION_ID"),
        (Harness::Codex, "CODEX_THREAD_ID"),
        (Harness::OpenCode, "OPENCODE_SESSION_ID"),
        // Set by the Pi extension that `provenance install` adds.
        (Harness::Pi, "PI_SESSION_ID"),
    ]
    .into_iter()
    .filter_map(|(h, var)| get(var).filter(|v| !v.trim().is_empty()).map(|v| (h, v)))
    .collect()
}

pub fn trailer_value(harness: Harness, id: &str) -> String {
    let prefix = match harness {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::OpenCode => "opencode",
        Harness::Pi => "pi",
    };
    format!("{prefix}/{id}")
}

/// A harness started from another one's shell inherits its session variable
/// (OpenCode run from Claude Code sees `CLAUDE_CODE_SESSION_ID` too), so when
/// several are set, the one whose process is nearest up the tree made the
/// commit. With no harness process found, all are kept.
pub fn innermost(
    sessions: Vec<(Harness, String)>,
    nearest: Option<Harness>,
) -> Vec<(Harness, String)> {
    match nearest {
        Some(h) if sessions.len() > 1 && sessions.iter().any(|(s, _)| *s == h) => {
            sessions.into_iter().filter(|(s, _)| *s == h).collect()
        }
        _ => sessions,
    }
}

/// Name for this machine in stamps. Never the host name, which would publish
/// the owner's name and device on public repositories. In order:
/// `PROVENANCE_MACHINE`, the `provenance.machine` global git setting (an
/// alias, or the random ID stored there), else a new random ID, saved there.
pub fn machine_name() -> Option<String> {
    if let Some(m) = std::env::var("PROVENANCE_MACHINE")
        .ok()
        .filter(|m| valid_alias(m))
    {
        return Some(m);
    }
    if let Some(m) = configured_machine().filter(|m| valid_alias(m)) {
        return Some(m);
    }
    let id = random_id()?;
    // If saving fails the ID still stamps this commit; the next one retries.
    let _ = set_machine(&id);
    Some(id)
}

pub fn configured_machine() -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--global", "--get", "provenance.machine"])
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

pub fn set_machine(name: &str) -> Result<()> {
    if !valid_alias(name) {
        bail!("machine names use letters, digits, '.', '-' and '_' (up to 40 characters)");
    }
    let status = Command::new("git")
        .args(["config", "--global", "provenance.machine", name])
        .status()
        .context("running git config")?;
    if !status.success() {
        bail!("git config --global provenance.machine failed");
    }
    Ok(())
}

/// Aliases end up in commit trailers, so keep them to one safe token.
pub fn valid_alias(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `m-` and eight random hex digits.
pub fn random_id() -> Option<String> {
    use std::io::Read;
    let mut bytes = [0u8; 4];
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut bytes)
        .ok()?;
    Some(format!(
        "m-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

/// `prepare-commit-msg` entry point: add trailers when an agent session is
/// making the commit. Commits made outside a session are left untouched.
pub fn stamp_message_file(msg_file: &str) -> Result<()> {
    let sessions = sessions_from_env(|k| std::env::var(k).ok());
    if sessions.is_empty() {
        return Ok(());
    }
    let sessions = if sessions.len() > 1 {
        innermost(sessions, crate::live::nearest_harness().map(|(h, _, _)| h))
    } else {
        sessions
    };
    let mut cmd = Command::new("git");
    cmd.args([
        "interpret-trailers",
        "--in-place",
        "--if-exists",
        "addIfDifferent",
    ]);
    for (h, id) in &sessions {
        cmd.arg("--trailer")
            .arg(format!("Agent-Session: {}", trailer_value(*h, id)));
    }
    let status = cmd
        .arg(msg_file)
        .status()
        .context("running git interpret-trailers")?;
    if !status.success() {
        bail!("git interpret-trailers failed");
    }
    if let Some(m) = machine_name() {
        // Separate call: the machine replaces any earlier value instead of adding.
        let status = Command::new("git")
            .args([
                "interpret-trailers",
                "--in-place",
                "--if-exists",
                "replace",
                "--trailer",
            ])
            .arg(format!("Agent-Machine: {m}"))
            .arg(msg_file)
            .status()
            .context("running git interpret-trailers")?;
        if !status.success() {
            bail!("git interpret-trailers failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 30, 0, 0, 0).unwrap()
    }

    #[test]
    fn parses_provenance_and_web_stamps() {
        let msg = "Fix thing\n\nbody\n\nAgent-Session: claude-code/92a6\nAgent-Session: codex/01a1\nAgent-Machine: mbp\n";
        let got = parse(msg, t());
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].harness, Harness::ClaudeCode);
        assert_eq!(got[1].session_id, "01a1");
        assert!(got.iter().all(|s| s.machine.as_deref() == Some("mbp")));

        let web = parse(
            "x\n\nClaude-Session: https://claude.ai/code/session_01G9\n",
            t(),
        );
        assert_eq!(web[0].session_id, "session_01G9");
        assert_eq!(
            web[0].url.as_deref(),
            Some("https://claude.ai/code/session_01G9")
        );
        assert_eq!(web[0].machine, None);
    }

    #[test]
    fn ignores_malformed_stamps() {
        assert!(
            parse(
                "Agent-Session: vim/1\nAgent-Session: codex/\nAgent-Session: nope\n",
                t()
            )
            .is_empty()
        );
    }

    #[test]
    fn reads_both_harness_variables() {
        let env = |k: &str| match k {
            "CLAUDE_CODE_SESSION_ID" => Some("c".to_string()),
            "CODEX_THREAD_ID" => Some(" ".to_string()),
            _ => None,
        };
        assert_eq!(
            sessions_from_env(env),
            [(Harness::ClaudeCode, "c".to_string())]
        );
    }

    #[test]
    fn nested_harness_stamps_only_its_own_session() {
        let both = vec![
            (Harness::ClaudeCode, "outer".to_string()),
            (Harness::OpenCode, "ses_inner".to_string()),
        ];
        assert_eq!(
            innermost(both.clone(), Some(Harness::OpenCode)),
            [(Harness::OpenCode, "ses_inner".to_string())]
        );
        // Unknown process tree, or a nearest harness with no variable: keep all.
        assert_eq!(innermost(both.clone(), None), both);
        assert_eq!(innermost(both.clone(), Some(Harness::Codex)), both);
        let parsed = parse("x\n\nAgent-Session: opencode/ses_e72\n", t());
        assert_eq!(parsed[0].harness, Harness::OpenCode);
        assert_eq!(
            trailer_value(Harness::OpenCode, "ses_e72"),
            "opencode/ses_e72"
        );
    }

    #[test]
    fn stamps_a_message_file_once() {
        let dir = std::env::temp_dir().join(format!("prov-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("MSG");
        std::fs::write(&f, "Subject\n\nBody\n").unwrap();
        // SAFETY: tests in this crate don't read these variables concurrently.
        unsafe {
            std::env::set_var("CODEX_THREAD_ID", "t1");
            std::env::set_var("PROVENANCE_MACHINE", "box");
            std::env::remove_var("CLAUDE_CODE_SESSION_ID");
            std::env::remove_var("OPENCODE_SESSION_ID");
            std::env::remove_var("PI_SESSION_ID");
        }
        let p = f.to_str().unwrap();
        stamp_message_file(p).unwrap();
        stamp_message_file(p).unwrap(); // amend in the same session
        let text = std::fs::read_to_string(&f).unwrap();
        unsafe {
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("PROVENANCE_MACHINE");
        }
        assert_eq!(text.matches("Agent-Session: codex/t1").count(), 1, "{text}");
        assert_eq!(text.matches("Agent-Machine: box").count(), 1, "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn machine_ids_are_random_and_safe() {
        let a = random_id().unwrap();
        let b = random_id().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 10);
        assert!(a.starts_with("m-") && valid_alias(&a));
    }

    #[test]
    fn aliases_are_single_safe_tokens() {
        assert!(valid_alias("mbp"));
        assert!(valid_alias("work-laptop_2"));
        assert!(!valid_alias(""));
        assert!(!valid_alias("my mac"));
        assert!(!valid_alias("mbp\nAgent-Session: codex/x"));
        assert!(!valid_alias(&"x".repeat(41)));
    }
}

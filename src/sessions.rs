//! Reads Claude Code and Codex session logs into a flat list of sightings:
//! "session S was in folder F on branch B, last seen at T".

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    ClaudeCode,
    Codex,
}

impl Harness {
    pub fn label(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "Claude Code",
            Harness::Codex => "Codex",
        }
    }
}

/// One session seen on one branch in one folder.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Sighting {
    pub harness: Harness,
    pub session_id: String,
    pub app: String,
    pub cwd: String,
    pub branch: String,
    /// Repository URL recorded by the harness itself, when it records one (Codex).
    pub repo_url: Option<String>,
    pub last_seen: DateTime<Utc>,
    pub log_path: PathBuf,
}

pub fn load_all(home: &Path) -> Vec<Sighting> {
    let mut out = load_claude(&home.join(".claude/projects"));
    out.extend(load_codex(&[
        home.join(".codex/sessions"),
        home.join(".codex/archived_sessions"),
    ]));
    out
}

fn jsonl_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => jsonl_files(&path, out),
            Ok(t) if t.is_file() && path.extension().is_some_and(|e| e == "jsonl") => {
                out.push(path)
            }
            _ => {}
        }
    }
}

// ---------- Claude Code ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeLine {
    session_id: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    timestamp: Option<String>,
    entrypoint: Option<String>,
}

/// Claude Code records `gitBranch` and `cwd` on every message, so a session that
/// switches branches is seen on each of them.
pub fn load_claude(root: &Path) -> Vec<Sighting> {
    let mut files = Vec::new();
    jsonl_files(root, &mut files);
    files
        .par_iter()
        .flat_map_iter(|p| read_claude_file(p))
        .collect()
}

fn read_claude_file(path: &Path) -> Vec<Sighting> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut seen: HashMap<(String, String, String), Sighting> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        // Cheap pre-filter: most lines are large tool output without branch info.
        if !line.contains("\"gitBranch\":\"") {
            continue;
        }
        let Ok(l) = serde_json::from_str::<ClaudeLine>(&line) else {
            continue;
        };
        let (Some(id), Some(cwd), Some(branch), Some(ts)) =
            (l.session_id, l.cwd, l.git_branch, l.timestamp)
        else {
            continue;
        };
        let Ok(ts) = DateTime::parse_from_rfc3339(&ts) else {
            continue;
        };
        let ts = ts.with_timezone(&Utc);
        let app = claude_app(l.entrypoint.as_deref(), &cwd);
        let key = (id.clone(), cwd.clone(), branch.clone());
        seen.entry(key)
            .and_modify(|s| s.last_seen = s.last_seen.max(ts))
            .or_insert(Sighting {
                harness: Harness::ClaudeCode,
                session_id: id,
                app,
                cwd,
                branch,
                repo_url: None,
                last_seen: ts,
                log_path: path.to_path_buf(),
            });
    }
    seen.into_values().collect()
}

/// `entrypoint` separates CLI and Claude desktop; apps built on the SDK all
/// report `sdk-ts`, so fall back to their worktree locations.
pub fn claude_app(entrypoint: Option<&str>, cwd: &str) -> String {
    match entrypoint {
        Some("cli") => "CLI".into(),
        Some("claude-desktop") => "Claude desktop".into(),
        Some("sdk-ts") | None => app_from_path(cwd).unwrap_or("SDK").into(),
        Some(other) => other.into(),
    }
}

fn app_from_path(cwd: &str) -> Option<&'static str> {
    if cwd.contains("/.t3/worktrees/") {
        Some("T3 Code")
    } else if cwd.contains("/conductor/workspaces/") {
        Some("Conductor")
    } else {
        None
    }
}

// ---------- Codex ----------

#[derive(Deserialize)]
struct CodexFirstLine {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    payload: CodexMeta,
}

#[derive(Deserialize)]
struct Timestamped {
    timestamp: String,
}

#[derive(Deserialize)]
struct CodexMeta {
    id: String,
    cwd: Option<String>,
    originator: Option<String>,
    git: Option<CodexGit>,
}

#[derive(Deserialize)]
struct CodexGit {
    branch: Option<String>,
    repository_url: Option<String>,
}

/// Codex records git details once, in the `session_meta` first line, so a
/// session is only seen on the branch it started on.
pub fn load_codex(roots: &[PathBuf]) -> Vec<Sighting> {
    let mut files = Vec::new();
    for root in roots {
        jsonl_files(root, &mut files);
    }
    files
        .par_iter()
        .filter_map(|p| read_codex_file(p))
        .collect()
}

fn read_codex_file(path: &Path) -> Option<Sighting> {
    let mut first = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut first)
        .ok()?;
    let line: CodexFirstLine = serde_json::from_str(&first).ok()?;
    if line.kind != "session_meta" {
        return None;
    }
    let meta = line.payload;
    let git = meta.git?;
    // Last logged event, falling back to the session start; file times move
    // when logs are copied or restored, so they are not activity.
    let started = line.timestamp.as_deref().and_then(parse_ts);
    let last_seen = last_logged_time(path).or(started)?;
    let cwd = meta.cwd.unwrap_or_default();
    Some(Sighting {
        harness: Harness::Codex,
        session_id: meta.id,
        app: codex_app(meta.originator.as_deref(), &cwd),
        cwd,
        branch: git.branch?,
        repo_url: git.repository_url,
        last_seen,
        log_path: path.to_path_buf(),
    })
}

fn parse_ts(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Timestamp of the last complete log line that has one, read from the end
/// of the file in growing chunks so huge logs aren't read in full.
fn last_logged_time(path: &Path) -> Option<DateTime<Utc>> {
    use std::io::{Read, Seek, SeekFrom};
    const MAX_TAIL: u64 = 16 << 20;
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut chunk: u64 = 64 << 10;
    loop {
        let start = len.saturating_sub(chunk);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::new();
        file.by_ref().take(len - start).read_to_end(&mut buf).ok()?;
        let text = String::from_utf8_lossy(&buf);
        let mut lines: Vec<&str> = text.lines().collect();
        if start > 0 && !lines.is_empty() {
            lines.remove(0); // may begin mid-line
        }
        let found = lines.iter().rev().find_map(|l| {
            serde_json::from_str::<Timestamped>(l)
                .ok()
                .and_then(|t| parse_ts(&t.timestamp))
        });
        if found.is_some() || start == 0 || chunk >= MAX_TAIL {
            return found;
        }
        chunk *= 4;
    }
}

pub fn codex_app(originator: Option<&str>, cwd: &str) -> String {
    match originator {
        Some("codex-tui") | Some("codex_cli_rs") => "CLI".into(),
        Some("codex_exec") => "exec".into(),
        Some("codex_sdk_ts") | None => app_from_path(cwd).unwrap_or("SDK").into(),
        Some(other) => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_apps_are_named_from_their_worktree_path() {
        assert_eq!(
            claude_app(Some("sdk-ts"), "/Users/x/.t3/worktrees/app/t3-1"),
            "T3 Code"
        );
        assert_eq!(
            claude_app(Some("sdk-ts"), "/Users/x/conductor/workspaces/app/tianjin"),
            "Conductor"
        );
        assert_eq!(claude_app(Some("sdk-ts"), "/tmp/x"), "SDK");
        assert_eq!(
            claude_app(Some("claude-desktop"), "/tmp/x"),
            "Claude desktop"
        );
        assert_eq!(codex_app(Some("T3 Code"), "/tmp/x"), "T3 Code");
        assert_eq!(codex_app(Some("codex-tui"), "/tmp/x"), "CLI");
    }

    #[test]
    fn copied_codex_log_must_keep_logged_activity_time() {
        let dir = std::env::temp_dir().join(format!("prov-codex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        let big = "x".repeat(200 << 10); // forces more than one tail chunk
        std::fs::write(
            &path,
            format!(
                concat!(
                    r#"{{"timestamp":"2026-10-01T00:00:00Z","type":"session_meta","payload":{{"id":"c1","cwd":"/r","originator":"codex-tui","git":{{"branch":"feat","repository_url":"https://github.com/o/r"}}}}}}"#, "\n",
                    r#"{{"timestamp":"2026-10-02T00:00:00Z","type":"event","payload":{{}}}}"#, "\n",
                    r#"{{"type":"no time","payload":"{}"}}"#, "\n",
                ),
                big
            ),
        )
        .unwrap();
        let s = read_codex_file(&path).unwrap();
        assert_eq!(s.last_seen.to_rfc3339(), "2026-10-02T00:00:00+00:00");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn claude_file_keeps_latest_time_per_branch() {
        let dir = std::env::temp_dir().join(format!("prov-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"sessionId":"s1","cwd":"/r","gitBranch":"feat","timestamp":"2026-10-01T00:00:00Z","entrypoint":"cli"}"#, "\n",
                r#"{"sessionId":"s1","cwd":"/r","gitBranch":"feat","timestamp":"2026-10-02T00:00:00Z","entrypoint":"cli"}"#, "\n",
                r#"{"sessionId":"s1","cwd":"/r","gitBranch":"other","timestamp":"2026-10-03T00:00:00Z","entrypoint":"cli"}"#, "\n",
                r#"{"type":"no branch here"}"#, "\n",
            ),
        )
        .unwrap();
        let mut got = read_claude_file(&path);
        got.sort_by(|a, b| a.branch.cmp(&b.branch));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].branch, "feat");
        assert_eq!(got[0].last_seen.to_rfc3339(), "2026-10-02T00:00:00+00:00");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

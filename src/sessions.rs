//! Reads Claude Code and Codex session logs into a flat list of sightings:
//! "session S was in folder F on branch B, last seen at T".

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    ClaudeCode,
    Codex,
}

/// One session seen on one branch in one folder.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct Sighting {
    pub harness: Harness,
    pub session_id: String,
    pub app: String,
    pub cwd: String,
    pub branch: String,
    /// Repository URL recorded by the harness itself, when it records one (Codex).
    pub repo_url: Option<String>,
    pub last_seen: DateTime<Utc>,
    pub log_path: Option<PathBuf>,
    /// Machine named by a commit stamp, for sessions not on this machine.
    pub machine: Option<String>,
    /// Link to the session, when it has one (Claude Code on the web).
    pub url: Option<String>,
}

/// What was read from one log file, valid while its size and modified time
/// are unchanged.
#[derive(serde::Serialize, Deserialize, Clone)]
struct Indexed {
    len: u64,
    modified_ns: u128,
    sightings: Vec<Sighting>,
}

/// All sightings from both harnesses' logs. With a cache folder, only files
/// whose size or modified time changed since the last run are read again.
pub fn load_all(home: &Path, cache_dir: Option<&Path>) -> Vec<Sighting> {
    let mut files: Vec<(PathBuf, Harness)> = Vec::new();
    let mut claude = Vec::new();
    jsonl_files(&home.join(".claude/projects"), &mut claude);
    files.extend(claude.into_iter().map(|p| (p, Harness::ClaudeCode)));
    let mut codex = Vec::new();
    for root in [
        home.join(".codex/sessions"),
        home.join(".codex/archived_sessions"),
    ] {
        jsonl_files(&root, &mut codex);
    }
    files.extend(codex.into_iter().map(|p| (p, Harness::Codex)));

    let index_path = cache_dir.map(|d| crate::cache::path(d, "logs"));
    let old: HashMap<String, Indexed> = index_path
        .as_deref()
        .and_then(crate::cache::read)
        .unwrap_or_default();

    let entries: Vec<(String, Indexed, bool)> = files
        .par_iter()
        .filter_map(|(path, harness)| {
            let meta = std::fs::metadata(path).ok()?;
            let len = meta.len();
            let modified_ns = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos();
            let key = path.to_string_lossy().into_owned();
            if let Some(hit) = old
                .get(&key)
                .filter(|e| e.len == len && e.modified_ns == modified_ns)
            {
                return Some((key, hit.clone(), false));
            }
            let sightings = match harness {
                Harness::ClaudeCode => read_claude_file(path),
                Harness::Codex => read_codex_file(path).into_iter().collect(),
            };
            Some((
                key,
                Indexed {
                    len,
                    modified_ns,
                    sightings,
                },
                true,
            ))
        })
        .collect();

    let changed = entries.iter().any(|(_, _, fresh)| *fresh) || entries.len() != old.len();
    let out = entries
        .iter()
        .flat_map(|(_, e, _)| e.sightings.iter().cloned())
        .collect();
    if let Some(p) = index_path
        && changed
    {
        let index: HashMap<String, Indexed> = entries.into_iter().map(|(k, e, _)| (k, e)).collect();
        crate::cache::write(&p, &index);
    }
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
                log_path: Some(path.to_path_buf()),
                machine: None,
                url: None,
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

pub fn app_from_path(cwd: &str) -> Option<&'static str> {
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
        log_path: Some(path.to_path_buf()),
        machine: None,
        url: None,
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

    #[test]
    fn index_reuses_unchanged_files_and_rereads_changed_ones() {
        let home = std::env::temp_dir().join(format!("prov-index-{}", std::process::id()));
        let proj = home.join(".claude/projects/p");
        std::fs::create_dir_all(&proj).unwrap();
        let log = proj.join("s.jsonl");
        let line = |b: &str, t: &str| {
            format!(
                r#"{{"sessionId":"s1","cwd":"/r","gitBranch":"{b}","timestamp":"{t}","entrypoint":"cli"}}"#
            )
        };
        std::fs::write(&log, line("feat", "2026-10-01T00:00:00Z") + "\n").unwrap();
        let cache = home.join("cache");
        assert_eq!(load_all(&home, Some(&cache)).len(), 1);

        // Unchanged file: served from the index even if its contents can't be read.
        let saved = std::fs::read(&log).unwrap();
        let modified = std::fs::metadata(&log).unwrap().modified().unwrap();
        std::fs::write(&log, "x".repeat(saved.len())).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&log)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_eq!(load_all(&home, Some(&cache))[0].branch, "feat");

        // Changed file: read again.
        std::fs::write(
            &log,
            line("feat", "2026-10-01T00:00:00Z")
                + "\n"
                + &line("other", "2026-10-02T00:00:00Z")
                + "\n",
        )
        .unwrap();
        assert_eq!(load_all(&home, Some(&cache)).len(), 2);
        std::fs::remove_dir_all(&home).unwrap();
    }
}

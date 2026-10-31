//! Reads Claude Code, Codex and Pi session logs, and OpenCode's session database,
//! into a flat list of sightings: "session S was in folder F on branch B, last
//! seen at T".

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
    #[serde(rename = "opencode")]
    OpenCode,
    Pi,
}

impl Harness {
    /// The harness a binary name belongs to, for walking up the process tree.
    pub fn from_binary(name: &str) -> Option<Harness> {
        match name {
            "claude" => Some(Harness::ClaudeCode),
            "codex" => Some(Harness::Codex),
            "opencode" => Some(Harness::OpenCode),
            "pi" => Some(Harness::Pi),
            _ => None,
        }
    }
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
    let mut pi = Vec::new();
    jsonl_files(&pi_sessions_dir(home), &mut pi);
    files.extend(pi.into_iter().map(|p| (p, Harness::Pi)));

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
                Harness::Pi => read_pi_file(path).into_iter().collect(),
                Harness::OpenCode => Vec::new(),
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

// ---------- Pi ----------

/// Pi's config folder: `PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
pub fn pi_agent_dir(home: &Path) -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".pi/agent"))
}

fn pi_sessions_dir(home: &Path) -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| pi_agent_dir(home).join("sessions"))
}

#[derive(Deserialize)]
struct PiHeader {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    cwd: String,
    timestamp: Option<String>,
}

/// Pi's first line is a `session` header with the session's ID and folder.
/// Pi records no git branch; [`give_folder_branches`] fills it in.
fn read_pi_file(path: &Path) -> Option<Sighting> {
    let mut first = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut first)
        .ok()?;
    let header: PiHeader = serde_json::from_str(&first).ok()?;
    if header.kind != "session" {
        return None;
    }
    let started = header.timestamp.as_deref().and_then(parse_ts);
    Some(Sighting {
        harness: Harness::Pi,
        session_id: header.id,
        app: app_from_path(&header.cwd).unwrap_or("CLI").into(),
        cwd: header.cwd,
        branch: String::new(),
        repo_url: None,
        last_seen: last_logged_time(path).or(started)?,
        log_path: Some(path.to_path_buf()),
        machine: None,
        url: None,
    })
}

// ---------- OpenCode ----------

/// One row of OpenCode's `session_v2` table.
#[derive(Deserialize, Debug, Clone)]
pub struct OpenCodeSession {
    pub id: String,
    pub directory: String,
    /// Milliseconds since the epoch, as are the other times.
    pub time_updated: i64,
    pub time_busy: Option<i64>,
    pub time_idle: Option<i64>,
    pub time_archived: Option<i64>,
}

impl OpenCodeSession {
    /// Mid-turn: it went busy after it last went idle.
    pub fn busy(&self) -> bool {
        self.time_busy
            .is_some_and(|b| self.time_idle.is_none_or(|i| b > i))
    }
}

pub fn opencode_db(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join("opencode/opencode.db")
}

/// Top-level sessions from OpenCode's database, read through the `sqlite3`
/// command in read-only mode. Empty when OpenCode or `sqlite3` is missing.
pub fn opencode_sessions(db: &Path) -> Vec<OpenCodeSession> {
    if !db.is_file() {
        return Vec::new();
    }
    let out = std::process::Command::new("sqlite3")
        .arg("-readonly")
        .arg("-json")
        .arg(db)
        .arg(
            "select id, directory, time_updated, time_busy, time_idle, time_archived \
             from session_v2 where parent_id is null",
        )
        .output();
    match out {
        Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// OpenCode records no git branch; [`give_folder_branches`] fills it in.
pub fn opencode_sightings(sessions: &[OpenCodeSession]) -> Vec<Sighting> {
    sessions
        .iter()
        .filter_map(|s| {
            Some(Sighting {
                harness: Harness::OpenCode,
                session_id: s.id.clone(),
                app: app_from_path(&s.directory).unwrap_or("OpenCode").into(),
                cwd: s.directory.clone(),
                branch: String::new(),
                repo_url: None,
                last_seen: DateTime::from_timestamp_millis(s.time_updated)?,
                log_path: None,
                machine: None,
                url: None,
            })
        })
        .collect()
}

/// Harnesses whose sessions record no git branch.
fn records_no_branch(h: Harness) -> bool {
    matches!(h, Harness::OpenCode | Harness::Pi)
}

/// For sessions of harnesses that record no git branch: the folder's current
/// branch, given only when that session is the most recent activity in the
/// folder by any harness. An older session, or one followed by another
/// harness's session in the same folder, may have worked on another branch.
pub fn give_folder_branches(
    sightings: &mut [Sighting],
    branch_of: impl Fn(&str) -> Option<String>,
) {
    let mut latest: HashMap<&str, usize> = HashMap::new();
    for (i, s) in sightings.iter().enumerate() {
        if s.cwd.is_empty() {
            continue;
        }
        let e = latest.entry(&s.cwd).or_insert(i);
        if s.last_seen > sightings[*e].last_seen {
            *e = i;
        }
    }
    let give: Vec<usize> = latest
        .into_values()
        .filter(|&i| records_no_branch(sightings[i].harness) && sightings[i].branch.is_empty())
        .collect();
    for i in give {
        if let Some(b) = branch_of(&sightings[i].cwd) {
            sightings[i].branch = b;
        }
    }
}

/// The branch checked out in a folder, if it exists and isn't detached.
pub fn current_branch(cwd: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", cwd, "symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .ok()?;
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !b.is_empty()).then_some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oc(id: &str, dir: &str, updated: i64) -> OpenCodeSession {
        OpenCodeSession {
            id: id.into(),
            directory: dir.into(),
            time_updated: updated,
            time_busy: Some(updated - 10),
            time_idle: Some(updated),
            time_archived: None,
        }
    }

    #[test]
    fn folder_branch_goes_to_the_folders_latest_session_only() {
        let sessions = [
            oc("ses_old", "/r", 1_000),
            oc("ses_new", "/r", 2_000),
            oc("ses_x", "/x", 500),
            oc("ses_y", "/y", 500),
        ];
        let mut got = opencode_sightings(&sessions);
        // Another harness worked in /y after the OpenCode session there.
        let mut later = got[3].clone();
        later.harness = Harness::ClaudeCode;
        later.session_id = "claude".into();
        later.branch = "other".into();
        later.last_seen = DateTime::from_timestamp_millis(900).unwrap();
        got.push(later);
        give_folder_branches(&mut got, |_| Some("feat".to_string()));
        let branch = |id: &str| {
            got.iter()
                .find(|s| s.session_id == id)
                .unwrap()
                .branch
                .clone()
        };
        assert_eq!(branch("ses_new"), "feat");
        assert_eq!(branch("ses_old"), "");
        assert_eq!(branch("ses_x"), "feat");
        assert_eq!(
            branch("ses_y"),
            "",
            "a later session by another harness in /y"
        );
        assert_eq!(branch("claude"), "other");
        assert_eq!(got[1].last_seen.timestamp_millis(), 2_000);
        assert!(
            got[..4]
                .iter()
                .all(|s| s.harness == Harness::OpenCode && s.app == "OpenCode")
        );
    }

    #[test]
    fn reads_a_pi_session_file() {
        let dir = std::env::temp_dir().join(format!("prov-pi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-10-31T00-00-00-000Z_abc.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session","version":3,"id":"pi-1","timestamp":"2026-10-31T00:00:00.000Z","cwd":"/r"}"#, "\n",
                r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2026-10-31T00:05:00.000Z","message":{"role":"user","content":"hi"}}"#, "\n",
            ),
        )
        .unwrap();
        let s = read_pi_file(&path).unwrap();
        assert_eq!(
            (
                s.harness,
                s.session_id.as_str(),
                s.cwd.as_str(),
                s.app.as_str()
            ),
            (Harness::Pi, "pi-1", "/r", "CLI")
        );
        assert_eq!(s.last_seen.to_rfc3339(), "2026-10-31T00:05:00+00:00");
        assert!(s.branch.is_empty());
        std::fs::write(&path, r#"{"type":"message"}"#).unwrap();
        assert!(read_pi_file(&path).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opencode_busy_means_busy_after_idle() {
        let mut s = oc("a", "/r", 2_000);
        assert!(!s.busy());
        s.time_busy = Some(3_000);
        assert!(s.busy());
        s.time_idle = None;
        assert!(s.busy());
        s.time_busy = None;
        assert!(!s.busy());
    }

    #[test]
    fn reads_an_opencode_database() {
        let dir = std::env::temp_dir().join(format!("prov-oc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("opencode.db");
        let ok = std::process::Command::new("sqlite3")
            .arg(&db)
            .arg(concat!(
                "create table session_v2 (id text, parent_id text, directory text, time_updated integer,",
                " time_busy integer, time_idle integer, time_archived integer);",
                "insert into session_v2 values ('ses_a', null, '/r', 2000, 1000, 1500, null);",
                "insert into session_v2 values ('ses_child', 'ses_a', '/r', 2500, null, null, null);"
            ))
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            let got = opencode_sessions(&db);
            assert_eq!(got.len(), 1, "{got:?}");
            assert_eq!(got[0].id, "ses_a");
            assert!(!got[0].busy() && got[0].time_archived.is_none());
        }
        assert!(opencode_sessions(&dir.join("missing.db")).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

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

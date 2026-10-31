//! Live session records, written by harness hooks and read when listing.
//!
//! Each Claude Code or Codex hook event updates one small JSON file per
//! session under `~/.local/state/provenance/live/`: the harness process, the
//! last event, and every branch the session has been on.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::sessions::{Harness, Sighting, app_from_path, claude_app, codex_app};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub harness: Harness,
    pub session_id: String,
    pub app: String,
    pub cwd: String,
    /// Harness process that ran the hook, with its start time so a reused PID
    /// is never mistaken for the session.
    pub pid: Option<u32>,
    pub pid_started: Option<String>,
    pub last_event: String,
    pub updated: DateTime<Utc>,
    /// Every folder and branch the session has been seen on, with the last
    /// time. Kept per folder: a branch name only means something in its repository.
    pub branches: Vec<BranchSeen>,
    pub transcript_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BranchSeen {
    pub cwd: String,
    pub branch: String,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Working on a turn, and its process is up.
    Running,
    /// Finished its turn and is waiting for you.
    Waiting,
    /// Was mid-turn when its process disappeared.
    Stopped,
    /// The session was closed.
    Ended,
}

pub fn state(r: &Record, alive: bool) -> State {
    match r.last_event.as_str() {
        "SessionEnd" => State::Ended,
        "Stop" | "SessionStart" => State::Waiting,
        _ if alive => State::Running,
        _ => State::Stopped,
    }
}

pub fn live_dir(home: &Path) -> PathBuf {
    home.join(".local/state/provenance/live")
}

fn record_path(dir: &Path, harness: Harness, id: &str) -> PathBuf {
    let h = match harness {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::OpenCode => "opencode",
        Harness::Pi => "pi",
    };
    // Session IDs are UUIDs; keep only safe characters in the file name.
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    dir.join(format!("{h}-{safe}.json"))
}

#[derive(Deserialize)]
struct HookInput {
    session_id: String,
    cwd: Option<String>,
    hook_event_name: String,
    transcript_path: Option<String>,
}

/// Hook entry point. Never fails the harness: errors are swallowed and
/// nothing is printed, because some hook output is fed back to the agent.
pub fn run_hook(harness: Harness, home: &Path) {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Ok(event) = serde_json::from_str::<HookInput>(&input) else {
        return;
    };
    let dir = live_dir(home);
    let _ = std::fs::create_dir_all(&dir);
    let path = record_path(&dir, harness, &event.session_id);
    let previous: Option<Record> = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let record = update(previous, harness, event.into(), Utc::now(), &SystemProbe);
    if let Ok(json) = serde_json::to_vec_pretty(&record) {
        // Write then rename, so readers never see half a file.
        let tmp = path.with_extension(format!("json.{}", std::process::id()));
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// What a hook needs from the machine, so `update` can be tested.
pub trait Probe {
    fn harness_process(&self) -> Option<(u32, String)>;
    fn branch(&self, cwd: &str) -> Option<String>;
    fn env(&self, key: &str) -> Option<String>;
}

struct SystemProbe;

impl Probe for SystemProbe {
    /// Hooks run through a shell, so it's usually the grandparent.
    fn harness_process(&self) -> Option<(u32, String)> {
        nearest_harness().map(|(_, pid, started)| (pid, started))
    }

    fn branch(&self, cwd: &str) -> Option<String> {
        let out = Command::new("git")
            .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()?;
        let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !b.is_empty() && b != "HEAD").then_some(b)
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// The nearest ancestor process that is a harness (`claude`, `codex`,
/// `opencode` or `pi`): its harness, PID and start time.
pub fn nearest_harness() -> Option<(Harness, u32, String)> {
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..12 {
        let (ppid, started, comm) = ps_info(pid)?;
        let name = Path::new(&comm).file_name()?.to_string_lossy().to_string();
        // Pi runs on Node or Bun and names itself through its process title,
        // which shows in the command line rather than the executable.
        let name = if matches!(name.as_str(), "node" | "bun") {
            process_title(pid).unwrap_or(name)
        } else {
            name
        };
        if let Some(h) = Harness::from_binary(&name) {
            return Some((h, pid, started));
        }
        if ppid <= 1 {
            return None;
        }
        pid = ppid;
    }
    None
}

/// An OpenCode session's status from its database row. OpenCode leaves a
/// session busy when its process dies mid-turn, and one background service can
/// outlive it, so a running `opencode` doesn't prove this session is working:
/// a busy session is stopped when no `opencode` runs, and unknown otherwise.
/// Idle sessions count as waiting only while OpenCode runs, since its sessions
/// never formally end.
pub fn opencode_state(s: &crate::sessions::OpenCodeSession, running: bool) -> Option<State> {
    match (s.time_archived.is_some(), s.busy(), running) {
        (true, _, _) => Some(State::Ended),
        (false, true, true) => None,
        (false, true, false) => Some(State::Stopped),
        (false, false, true) => Some(State::Waiting),
        (false, false, false) => None,
    }
}

/// First word of a process's command line, which a process title replaces.
fn process_title(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&out.stdout);
    let first = line.split_whitespace().next()?;
    Some(Path::new(first).file_name()?.to_string_lossy().to_string())
}

/// Whether any `opencode` process is running.
pub fn opencode_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "opencode"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `(ppid, start time, command path)` for one process.
pub(crate) fn ps_info(pid: u32) -> Option<(u32, String, String)> {
    let out = Command::new("ps")
        .args([
            "-o",
            "ppid=",
            "-o",
            "lstart=",
            "-o",
            "comm=",
            "-p",
            &pid.to_string(),
        ])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // `lstart` is always five fields: "Fri Oct 30 18:46:07 2026".
    let mut parts = line.split_whitespace();
    let ppid = parts.next()?.parse().ok()?;
    let started: Vec<&str> = parts.by_ref().take(5).collect();
    let comm = parts.collect::<Vec<_>>().join(" ");
    (started.len() == 5).then(|| (ppid, started.join(" "), comm))
}

pub fn update(
    previous: Option<Record>,
    harness: Harness,
    event: HookEvent,
    now: DateTime<Utc>,
    probe: &dyn Probe,
) -> Record {
    let cwd = event
        .cwd
        .unwrap_or_else(|| previous.as_ref().map(|p| p.cwd.clone()).unwrap_or_default());
    // Desktop apps start a new harness process per turn, so look again when a
    // session starts or a turn begins; tool and stop events reuse it.
    let new_process = matches!(
        event.hook_event_name.as_str(),
        "SessionStart" | "UserPromptSubmit"
    );
    let (pid, pid_started) = match &previous {
        Some(p) if !new_process && p.pid.is_some() => (p.pid, p.pid_started.clone()),
        _ => match probe.harness_process() {
            Some((pid, started)) => (Some(pid), Some(started)),
            None => (None, None),
        },
    };
    let mut branches = previous
        .as_ref()
        .map(|p| p.branches.clone())
        .unwrap_or_default();
    if let Some(branch) = probe.branch(&cwd) {
        match branches
            .iter_mut()
            .find(|b| b.cwd == cwd && b.branch == branch)
        {
            Some(b) => b.last_seen = now,
            None => branches.push(BranchSeen {
                cwd: cwd.clone(),
                branch,
                last_seen: now,
            }),
        }
    }
    let app = match &previous {
        Some(p) if !p.app.is_empty() => p.app.clone(),
        _ => match harness {
            Harness::ClaudeCode => claude_app(probe.env("CLAUDE_CODE_ENTRYPOINT").as_deref(), &cwd),
            // Codex hooks don't say which app started the session; leave it
            // empty so the session log's `originator` fills it in.
            Harness::Codex => Some(codex_app(None, &cwd))
                .filter(|a| a != "SDK")
                .unwrap_or_default(),
            Harness::OpenCode => app_from_path(&cwd).unwrap_or_default().into(),
            Harness::Pi => app_from_path(&cwd).unwrap_or("CLI").into(),
        },
    };
    Record {
        harness,
        session_id: event.session_id,
        app,
        cwd,
        pid,
        pid_started,
        last_event: event.hook_event_name,
        updated: now,
        branches,
        transcript_path: event
            .transcript_path
            .or_else(|| previous.and_then(|p| p.transcript_path)),
    }
}

#[derive(Debug, Clone)]
pub struct HookEvent {
    pub session_id: String,
    pub cwd: Option<String>,
    pub hook_event_name: String,
    pub transcript_path: Option<String>,
}

impl From<HookInput> for HookEvent {
    fn from(h: HookInput) -> Self {
        HookEvent {
            session_id: h.session_id,
            cwd: h.cwd,
            hook_event_name: h.hook_event_name,
            transcript_path: h.transcript_path,
        }
    }
}

pub fn load_records(home: &Path) -> Vec<Record> {
    let Ok(entries) = std::fs::read_dir(live_dir(home)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect()
}

/// Which recorded processes are still the same process: one `ps` call.
pub fn alive_pids(records: &[Record]) -> HashMap<u32, bool> {
    let pids: Vec<String> = records
        .iter()
        .filter_map(|r| r.pid)
        .map(|p| p.to_string())
        .collect();
    let mut alive: HashMap<u32, bool> = HashMap::new();
    if pids.is_empty() {
        return alive;
    }
    let started: HashMap<u32, String> = Command::new("ps")
        .args(["-o", "pid=", "-o", "lstart=", "-p", &pids.join(",")])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| {
                    let mut p = l.split_whitespace();
                    let pid = p.next()?.parse().ok()?;
                    Some((pid, p.collect::<Vec<_>>().join(" ")))
                })
                .collect()
        })
        .unwrap_or_default();
    for r in records {
        if let (Some(pid), Some(want)) = (r.pid, &r.pid_started) {
            alive.insert(pid, started.get(&pid) == Some(want));
        }
    }
    alive
}

pub fn is_alive(r: &Record, alive: &HashMap<u32, bool>) -> bool {
    r.pid.and_then(|p| alive.get(&p).copied()).unwrap_or(false)
}

/// Branch sightings from live records: these cover branches a session moved
/// to after it started, which Codex logs don't record.
pub fn sightings(records: &[Record]) -> Vec<Sighting> {
    records
        .iter()
        .flat_map(|r| {
            r.branches.iter().map(move |b| Sighting {
                harness: r.harness,
                session_id: r.session_id.clone(),
                // Hooks don't always say which app ran the session; the folder often does.
                app: if r.app.is_empty() {
                    crate::sessions::app_from_path(&b.cwd)
                        .unwrap_or_default()
                        .to_string()
                } else {
                    r.app.clone()
                },
                cwd: b.cwd.clone(),
                branch: b.branch.clone(),
                repo_url: None,
                last_seen: b.last_seen,
                log_path: r.transcript_path.as_ref().map(PathBuf::from),
                machine: None,
                url: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::cell::Cell;

    #[test]
    fn opencode_busy_is_never_claimed_running() {
        use crate::sessions::OpenCodeSession;
        let s = |busy: Option<i64>, idle: Option<i64>, archived: Option<i64>| OpenCodeSession {
            id: "ses_a".into(),
            directory: "/r".into(),
            time_updated: 3,
            time_busy: busy,
            time_idle: idle,
            time_archived: archived,
        };
        // Interrupted mid-turn while another OpenCode process (the service) survives.
        assert_eq!(opencode_state(&s(Some(2), None, None), true), None);
        assert_eq!(
            opencode_state(&s(Some(2), None, None), false),
            Some(State::Stopped)
        );
        assert_eq!(
            opencode_state(&s(Some(1), Some(2), None), true),
            Some(State::Waiting)
        );
        assert_eq!(opencode_state(&s(Some(1), Some(2), None), false), None);
        assert_eq!(
            opencode_state(&s(Some(1), Some(2), Some(3)), true),
            Some(State::Ended)
        );
    }

    struct Fake {
        pid: u32,
        branch: &'static str,
        probes: Cell<u32>,
    }

    impl Probe for Fake {
        fn harness_process(&self) -> Option<(u32, String)> {
            self.probes.set(self.probes.get() + 1);
            Some((self.pid, "Fri Oct 30 18:46:07 2026".into()))
        }
        fn branch(&self, _: &str) -> Option<String> {
            Some(self.branch.into())
        }
        fn env(&self, _: &str) -> Option<String> {
            Some("cli".into())
        }
    }

    fn ev(name: &str) -> HookEvent {
        HookEvent {
            session_id: "s1".into(),
            cwd: Some("/r".into()),
            hook_event_name: name.into(),
            transcript_path: None,
        }
    }

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 30, h, 0, 0).unwrap()
    }

    #[test]
    fn tracks_branches_and_rechecks_process_per_turn() {
        let f = Fake {
            pid: 10,
            branch: "main",
            probes: Cell::new(0),
        };
        let r = update(None, Harness::ClaudeCode, ev("SessionStart"), at(1), &f);
        assert_eq!(r.app, "CLI");
        let f2 = Fake {
            pid: 20,
            branch: "feat",
            probes: Cell::new(0),
        };
        let r = update(Some(r), Harness::ClaudeCode, ev("PostToolUse"), at(2), &f2);
        assert_eq!(r.pid, Some(10), "tool events reuse the known process");
        assert_eq!(f2.probes.get(), 0);
        let r = update(
            Some(r),
            Harness::ClaudeCode,
            ev("UserPromptSubmit"),
            at(3),
            &f2,
        );
        assert_eq!(r.pid, Some(20), "a new turn may run in a new process");
        let seen: Vec<_> = r
            .branches
            .iter()
            .map(|b| (b.branch.as_str(), b.last_seen))
            .collect();
        assert_eq!(seen, [("main", at(1)), ("feat", at(3))]);
    }

    #[test]
    fn state_from_last_event_and_process() {
        let f = Fake {
            pid: 1,
            branch: "b",
            probes: Cell::new(0),
        };
        let mut r = update(None, Harness::Codex, ev("UserPromptSubmit"), at(1), &f);
        assert_eq!(state(&r, true), State::Running);
        assert_eq!(state(&r, false), State::Stopped);
        r.last_event = "Stop".into();
        assert_eq!(state(&r, false), State::Waiting);
        r.last_event = "SessionEnd".into();
        assert_eq!(state(&r, true), State::Ended);
    }

    #[test]
    fn record_file_names_are_safe() {
        let p = record_path(Path::new("/d"), Harness::Codex, "../../etc/x");
        assert_eq!(p, Path::new("/d/codex-etcx.json"));
    }

    #[test]
    fn parses_ps_output_for_this_process() {
        let (ppid, started, comm) = ps_info(std::process::id()).unwrap();
        assert!(ppid > 0);
        assert_eq!(started.split_whitespace().count(), 5);
        assert!(!comm.is_empty());
    }

    #[test]
    fn branches_keep_the_folder_they_were_seen_in() {
        let f = Fake {
            pid: 1,
            branch: "old-feature",
            probes: Cell::new(0),
        };
        let mut e = ev("PostToolUse");
        e.cwd = Some("/a".into());
        let r = update(None, Harness::Codex, e, at(1), &f);
        let f = Fake {
            pid: 1,
            branch: "other-feature",
            probes: Cell::new(0),
        };
        let mut e = ev("PostToolUse");
        e.cwd = Some("/b".into());
        let r = update(Some(r), Harness::Codex, e, at(2), &f);
        let got: Vec<_> = sightings(&[r])
            .into_iter()
            .map(|s| (s.cwd, s.branch))
            .collect();
        assert_eq!(
            got,
            [
                ("/a".to_string(), "old-feature".to_string()),
                ("/b".to_string(), "other-feature".to_string())
            ]
        );
    }
}

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
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub permission_mode_observed_at: Option<DateTime<Utc>>,
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
    permission_mode: Option<String>,
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
    nearest_harness_with(
        std::os::unix::process::parent_id(),
        &ps_info,
        &process_title,
    )
}

fn nearest_harness_with(
    mut pid: u32,
    info: &dyn Fn(u32) -> Option<(u32, String, String)>,
    title: &dyn Fn(u32) -> Option<String>,
) -> Option<(Harness, u32, String)> {
    for _ in 0..12 {
        let (ppid, started, comm) = info(pid)?;
        let name = Path::new(&comm).file_name()?.to_string_lossy().to_string();
        // Pi runs on Node or Bun and names itself through its process title,
        // which shows in the command line rather than the executable.
        let name = if matches!(name.as_str(), "node" | "bun") {
            // An unreadable Node/Bun title may hide an inner harness.
            // Never continue to an outer session without that evidence.
            title(pid)?
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
    if !out.status.success() {
        return None;
    }
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
    let result = ps_process_info(pid);
    #[cfg(target_os = "macos")]
    let result = result.or_else(|| native_process_info(pid));
    result
}

// Codex's macOS sandbox can deny executing ps while allowing libproc queries.
// Read the same kernel identity; never substitute environment or folder hints.
#[cfg(target_os = "macos")]
fn native_process_info(pid: u32) -> Option<(u32, String, String)> {
    let (ppid, started) = native_process_identity(pid)?;
    // Liveness does not need this path; ancestor recognition does. Never
    // substitute a resolved path that could hide a symlink-launched harness.
    let path = native_invoked_path(i32::try_from(pid).ok()?)?;
    Some((ppid, started, path))
}

/// Kernel process identity independent of executable path/title visibility.
#[cfg(target_os = "macos")]
fn native_process_identity(pid: u32) -> Option<(u32, String)> {
    let pid = i32::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: correctly sized writable buffer; read only after a full write.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if n != size as i32 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let started = chrono::DateTime::from_timestamp(i64::try_from(info.pbi_start_tvsec).ok()?, 0)?
        .with_timezone(&chrono::Local)
        .format("%a %b %-d %H:%M:%S %Y")
        .to_string();
    Some((info.pbi_ppid, started))
}

#[cfg(target_os = "macos")]
fn invoked_path(bytes: &[u8]) -> Option<String> {
    // Kernel layout: argc, NUL-terminated executable path, argv, environment.
    // Only consume the executable path; never expose arguments/environment.
    let path = bytes.get(std::mem::size_of::<libc::c_int>()..)?;
    let end = path.iter().position(|b| *b == 0)?;
    let path = std::str::from_utf8(&path[..end]).ok()?;
    (!path.is_empty()).then(|| path.to_string())
}

#[cfg(target_os = "macos")]
fn native_invoked_path(pid: i32) -> Option<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0;
    // SAFETY: query the required buffer size, with a valid MIB and size pointer.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size == 0
        || size > 16 * 1024 * 1024
    {
        return None;
    }
    let mut bytes = vec![0u8; size];
    // SAFETY: bytes is writable for size bytes. Read only the returned extent.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    invoked_path(bytes.get(..size)?)
}

// Pin only time. LC_ALL would also change comm's character encoding.
fn start_time_ps() -> Command {
    let mut command = Command::new("ps");
    command.env_remove("LC_ALL").env("LC_TIME", "C");
    if let Some(ctype) = std::env::var_os("LC_ALL").filter(|s| !s.is_empty()) {
        command.env("LC_CTYPE", ctype);
    }
    command
}

fn ps_process_info(pid: u32) -> Option<(u32, String, String)> {
    let out = start_time_ps()
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
    if !out.status.success() {
        return None;
    }
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
    let same_verified_process = pid.is_some()
        && pid_started.is_some()
        && previous.as_ref().is_some_and(|p| {
            p.session_id == event.session_id && p.pid == pid && p.pid_started == pid_started
        });
    let permission_mode = event.permission_mode.clone().or_else(|| {
        previous.as_ref().and_then(|p| {
            same_verified_process
                .then(|| p.permission_mode.clone())
                .flatten()
        })
    });
    let permission_mode_observed_at = if event.permission_mode.is_some() {
        Some(now)
    } else if same_verified_process {
        previous
            .as_ref()
            .and_then(|p| p.permission_mode_observed_at)
    } else {
        None
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
        permission_mode,
        permission_mode_observed_at,
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
    pub permission_mode: Option<String>,
}

impl From<HookInput> for HookEvent {
    fn from(h: HookInput) -> Self {
        HookEvent {
            session_id: h.session_id,
            cwd: h.cwd,
            hook_event_name: h.hook_event_name,
            transcript_path: h.transcript_path,
            permission_mode: h.permission_mode,
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

/// Start times of the recorded processes that still exist, by PID: one `ps`
/// call. A record's process is alive only if its own start time matches, so a
/// reused PID never vouches for an older record.
pub fn alive_pids(records: &[Record]) -> HashMap<u32, String> {
    let pids: Vec<String> = records
        .iter()
        .filter_map(|r| r.pid)
        .map(|p| p.to_string())
        .collect();
    if pids.is_empty() {
        return HashMap::new();
    }
    let mut alive: HashMap<u32, String> = start_time_ps()
        .args(["-o", "pid=", "-o", "lstart=", "-p", &pids.join(",")])
        .output()
        .ok()
        .filter(|out| out.status.success())
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
    // Start time alone proves liveness. Do not require executable-path or
    // title visibility, which can be more restricted in a sandbox.
    fill_alive_pids(records, &mut alive, &process_start);
    alive
}

fn process_start(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        native_process_identity(pid).map(|(_, started)| started)
    }
    #[cfg(not(target_os = "macos"))]
    {
        ps_info(pid).map(|(_, started, _)| started)
    }
}

fn fill_alive_pids(
    records: &[Record],
    alive: &mut HashMap<u32, String>,
    start: &dyn Fn(u32) -> Option<String>,
) {
    for pid in records.iter().filter_map(|r| r.pid) {
        if let std::collections::hash_map::Entry::Vacant(entry) = alive.entry(pid)
            && let Some(started) = start(pid)
        {
            entry.insert(started);
        }
    }
}

/// A legacy localized or missing start time cannot establish that a PID is
/// gone. Keep it unknown until a start/prompt hook refreshes the record.
pub fn has_c_start_time(record: &Record) -> bool {
    record.pid_started.as_deref().is_some_and(|s| {
        chrono::NaiveDateTime::parse_from_str(s, "%a %b %e %H:%M:%S %Y")
            .is_ok_and(|date| date.format("%a %b %-d %H:%M:%S %Y").to_string() == s)
    })
}

pub fn is_alive(r: &Record, started: &HashMap<u32, String>) -> bool {
    match (r.pid, &r.pid_started) {
        (Some(pid), Some(want)) => started.get(&pid) == Some(want),
        _ => false,
    }
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
        started: String,
        branch: &'static str,
        probes: Cell<u32>,
    }

    impl Probe for Fake {
        fn harness_process(&self) -> Option<(u32, String)> {
            self.probes.set(self.probes.get() + 1);
            Some((self.pid, self.started.clone()))
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
            permission_mode: None,
        }
    }

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 30, h, 0, 0).unwrap()
    }

    #[test]
    fn tracks_branches_and_rechecks_process_per_turn() {
        let f = Fake {
            started: "Fri Oct 30 18:46:07 2026".into(),
            pid: 10,
            branch: "main",
            probes: Cell::new(0),
        };
        let r = update(None, Harness::ClaudeCode, ev("SessionStart"), at(1), &f);
        assert_eq!(r.app, "CLI");
        let f2 = Fake {
            started: "Fri Oct 30 18:46:07 2026".into(),
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
    fn permission_mode_observation_updates_and_missing_evidence_is_unknown() {
        let probe = Fake {
            started: "Fri Oct 30 18:46:07 2026".into(),
            pid: 42,
            branch: "b",
            probes: Cell::new(0),
        };
        let mut event = ev("UserPromptSubmit");
        event.permission_mode = Some("bypassPermissions".into());
        let record = update(None, Harness::ClaudeCode, event, at(1), &probe);
        assert_eq!(record.permission_mode.as_deref(), Some("bypassPermissions"));
        assert_eq!(record.permission_mode_observed_at, Some(at(1)));
        let mut event = ev("Stop");
        event.permission_mode = Some("default".into());
        let record = update(Some(record), Harness::ClaudeCode, event, at(2), &probe);
        assert_eq!(record.permission_mode.as_deref(), Some("default"));
        assert_eq!(record.permission_mode_observed_at, Some(at(2)));
        let record = update(
            Some(record),
            Harness::ClaudeCode,
            ev("SessionStart"),
            at(3),
            &probe,
        );
        assert_eq!(record.permission_mode.as_deref(), Some("default"));
        assert_eq!(record.permission_mode_observed_at, Some(at(2)));
        let mut old = serde_json::to_value(record).unwrap();
        old.as_object_mut().unwrap().remove("permission_mode");
        assert!(
            serde_json::from_value::<Record>(old)
                .unwrap()
                .permission_mode
                .is_none()
        );
    }

    #[test]
    fn permission_mode_does_not_cross_session_or_process_identity() {
        let probe = Fake {
            pid: 42,
            started: "Fri Oct 30 18:46:07 2026".into(),
            branch: "b",
            probes: Cell::new(0),
        };
        let mut event = ev("UserPromptSubmit");
        event.permission_mode = Some("bypassPermissions".into());
        let previous = update(None, Harness::ClaudeCode, event, at(1), &probe);

        let mut other_session = ev("SessionStart");
        other_session.session_id = "s2".into();
        let changed_session = update(
            Some(previous.clone()),
            Harness::ClaudeCode,
            other_session,
            at(2),
            &probe,
        );
        assert!(changed_session.permission_mode.is_none());

        let different_pid = Fake {
            pid: 43,
            ..Fake {
                pid: 42,
                started: "Fri Oct 30 18:46:07 2026".into(),
                branch: "b",
                probes: Cell::new(0),
            }
        };
        let changed_pid = update(
            Some(previous.clone()),
            Harness::ClaudeCode,
            ev("SessionStart"),
            at(2),
            &different_pid,
        );
        assert!(changed_pid.permission_mode.is_none());

        let different_start_time = Fake {
            started: "Sat Oct 31 18:46:07 2026".into(),
            ..probe
        };
        let reused_pid = update(
            Some(previous),
            Harness::ClaudeCode,
            ev("SessionStart"),
            at(2),
            &different_start_time,
        );
        assert!(reused_pid.permission_mode.is_none());
    }

    #[test]
    fn missing_process_identity_does_not_preserve_permission_mode() {
        struct NoProcess;
        impl Probe for NoProcess {
            fn harness_process(&self) -> Option<(u32, String)> {
                None
            }
            fn branch(&self, _: &str) -> Option<String> {
                None
            }
            fn env(&self, _: &str) -> Option<String> {
                None
            }
        }

        let mut event = ev("UserPromptSubmit");
        event.permission_mode = Some("bypassPermissions".into());
        let previous = update(None, Harness::ClaudeCode, event, at(1), &NoProcess);
        assert_eq!(previous.pid, None);
        assert_eq!(previous.pid_started, None);

        let next = update(
            Some(previous),
            Harness::ClaudeCode,
            ev("SessionStart"),
            at(2),
            &NoProcess,
        );
        assert!(next.permission_mode.is_none());
        assert!(next.permission_mode_observed_at.is_none());
    }

    #[test]
    fn state_from_last_event_and_process() {
        let f = Fake {
            started: "Fri Oct 30 18:46:07 2026".into(),
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
    fn nested_harness_uses_nearest_verified_ancestor() {
        let info = |pid| match pid {
            10 => Some((20, "shell".into(), "/bin/zsh".into())),
            20 => Some((30, "inner".into(), "/usr/bin/claude".into())),
            30 => Some((1, "outer".into(), "/usr/bin/codex".into())),
            _ => None,
        };
        assert_eq!(
            nearest_harness_with(10, &info, &|_| None),
            Some((Harness::ClaudeCode, 20, "inner".into()))
        );
        assert!(nearest_harness_with(99, &info, &|_| None).is_none());
    }

    #[test]
    fn parses_ps_output_for_this_process() {
        let (ppid, started, comm) = ps_info(std::process::id()).unwrap();
        assert!(ppid > 0);
        assert_eq!(started.split_whitespace().count(), 5);
        assert!(!comm.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_identity_matches_ps_and_rejects_missing_process() {
        let pid = std::process::id();
        let native = native_process_info(pid).unwrap();
        let ps = ps_process_info(pid).unwrap();
        assert_eq!((&native.0, &native.1), (&ps.0, &ps.1));
        assert_eq!(
            std::fs::canonicalize(native.2).unwrap(),
            std::fs::canonicalize(ps.2).unwrap()
        );
        assert!(native_process_info(u32::MAX).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn non_us_locale_keeps_ps_native_and_hook_start_times_equal() {
        if std::env::var_os("PROVENANCE_LOCALE_FIXTURE").is_none() {
            let root = std::env::temp_dir().join(format!("prov-locale-{}", crate::send::new_id()));
            let directory = root.join("é dir/Fake.app/Contents/MacOS");
            std::fs::create_dir_all(&directory).unwrap();
            let binary = directory.join("codex");
            std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
            for (category, locale) in [
                ("LC_ALL", "C"),
                ("LC_ALL", "en_GB.UTF-8"),
                ("LC_ALL", "de_DE.UTF-8"),
                ("LANG", "en_GB.UTF-8"),
                ("LANG", "de_DE.UTF-8"),
                ("none", ""),
            ] {
                let mut command = Command::new(&binary);
                command
                    .args([
                        "--exact",
                        "live::tests::non_us_locale_keeps_ps_native_and_hook_start_times_equal",
                        "--nocapture",
                    ])
                    .env_remove("LC_ALL")
                    .env_remove("LANG")
                    .env_remove("LC_TIME")
                    .env_remove("LC_CTYPE")
                    .env("PROVENANCE_LOCALE_FIXTURE", format!("{category}:{locale}"));
                if category != "none" {
                    command.env(category, locale);
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "{category}={locale}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
        let pid = std::process::id();
        let native = native_process_identity(pid).unwrap().1;
        let (ppid, ps, comm) = ps_process_info(pid).unwrap();
        assert_eq!(ppid, std::os::unix::process::parent_id());
        assert_eq!(ps, native);
        let raw_comm = Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        assert!(raw_comm.status.success());
        assert_eq!(comm, String::from_utf8(raw_comm.stdout).unwrap().trim());
        let variant = std::env::var("PROVENANCE_LOCALE_FIXTURE").unwrap();
        if variant.contains("UTF-8") {
            assert!(comm.contains("é dir/Fake.app/Contents/MacOS/codex"));
            assert_eq!(
                std::fs::canonicalize(&comm).unwrap(),
                std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
            );
        }
        // Prove the child locale is effective, rather than silently testing C.
        let localized = Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        assert!(localized.status.success());
        let localized = String::from_utf8(localized.stdout)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(localized == native, !variant.contains("UTF-8"), "{variant}");
        struct IdentityProbe {
            pid: u32,
            started: String,
        }
        impl Probe for IdentityProbe {
            fn harness_process(&self) -> Option<(u32, String)> {
                Some((self.pid, self.started.clone()))
            }
            fn branch(&self, _: &str) -> Option<String> {
                None
            }
            fn env(&self, _: &str) -> Option<String> {
                None
            }
        }
        let probe = IdentityProbe { pid, started: ps };
        let mut old = update(None, Harness::Codex, ev("SessionStart"), Utc::now(), &probe);
        old.pid_started = Some(localized);
        for event in ["SessionStart", "UserPromptSubmit"] {
            let refreshed = update(
                Some(old.clone()),
                Harness::Codex,
                ev(event),
                Utc::now(),
                &probe,
            );
            assert_eq!(refreshed.pid_started.as_deref(), Some(native.as_str()));
            assert!(is_alive(
                &refreshed,
                &alive_pids(std::slice::from_ref(&refreshed))
            ));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn invoked_path_parser_is_bounded_to_executable() {
        let mut data = 1i32.to_ne_bytes().to_vec();
        data.extend_from_slice(b"/tmp/claude\0argv\0PRIVATE_ENV=hidden\0");
        assert_eq!(invoked_path(&data).as_deref(), Some("/tmp/claude"));
        assert_eq!(
            invoked_path(b"1234./claude\0ignored").as_deref(),
            Some("./claude")
        );
        for data in [&b""[..], &b"abc"[..], &b"1234\0"[..], &b"1234/no-null"[..]] {
            assert!(invoked_path(data).is_none());
        }
    }

    // Each subprocess is this test executable, under throwaway symlink names.
    // No model, hooks, owner state, or existing harness is involved.
    #[cfg(target_os = "macos")]
    #[test]
    fn native_symlink_harness_fixture() {
        let Ok(stage) = std::env::var("PROVENANCE_NATIVE_FIXTURE_STAGE") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("PROVENANCE_NATIVE_FIXTURE_ROOT").unwrap());
        if stage == "probe" {
            let found = nearest_harness_with(
                std::os::unix::process::parent_id(),
                &native_process_info,
                &|_| None,
            )
            .unwrap();
            assert_eq!(found.0, Harness::ClaudeCode);
            assert_eq!(found.1, std::os::unix::process::parent_id());
            if std::env::var_os("PROVENANCE_NATIVE_FIXTURE_RELATIVE").is_some() {
                let pid = std::os::unix::process::parent_id();
                let (ppid, started, path) = native_process_info(pid).unwrap();
                assert!(ppid > 0);
                assert_eq!(path, "./claude");
                let r = Record {
                    harness: Harness::ClaudeCode,
                    session_id: "relative".into(),
                    app: String::new(),
                    cwd: String::new(),
                    pid: Some(pid),
                    pid_started: Some(started),
                    last_event: "UserPromptSubmit".into(),
                    updated: Utc::now(),
                    branches: vec![],
                    transcript_path: None,
                    permission_mode: None,
                    permission_mode_observed_at: None,
                };
                let mut alive = HashMap::new();
                fill_alive_pids(std::slice::from_ref(&r), &mut alive, &process_start);
                assert!(is_alive(&r, &alive));
            }
        } else {
            let (next, stage) = if stage == "outer" {
                (
                    if std::env::var_os("PROVENANCE_NATIVE_FIXTURE_RELATIVE").is_some() {
                        PathBuf::from("./claude")
                    } else {
                        root.join("claude")
                    },
                    "inner",
                )
            } else {
                (std::env::current_exe().unwrap(), "probe")
            };
            let output = Command::new(next)
                .current_dir(&root)
                .args([
                    "--exact",
                    "live::tests::native_symlink_harness_fixture",
                    "--nocapture",
                ])
                .env("PROVENANCE_NATIVE_FIXTURE_STAGE", stage)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_lookup_recognises_symlinked_inner_harness() {
        let root = std::env::temp_dir().join(format!("prov-native-{}", crate::send::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let binary = root.join("2.1.321");
        std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
        for name in ["claude", "codex"] {
            std::os::unix::fs::symlink(&binary, root.join(name)).unwrap();
        }
        let output = Command::new(root.join("codex"))
            .args([
                "--exact",
                "live::tests::native_symlink_harness_fixture",
                "--nocapture",
            ])
            .env("PROVENANCE_NATIVE_FIXTURE_ROOT", &root)
            .env("PROVENANCE_NATIVE_FIXTURE_STAGE", "outer")
            .output()
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_relative_path_keeps_ancestor_and_liveness() {
        let root = std::env::temp_dir().join(format!("prov-relative-{}", crate::send::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let binary = root.join("2.1.321");
        std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
        for name in ["claude", "codex"] {
            std::os::unix::fs::symlink(&binary, root.join(name)).unwrap();
        }
        let output = Command::new("./codex")
            .current_dir(&root)
            .args([
                "--exact",
                "live::tests::native_symlink_harness_fixture",
                "--nocapture",
            ])
            .env("PROVENANCE_NATIVE_FIXTURE_ROOT", &root)
            .env("PROVENANCE_NATIVE_FIXTURE_STAGE", "outer")
            .env("PROVENANCE_NATIVE_FIXTURE_RELATIVE", "1")
            .output()
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn unreadable_node_or_bun_title_stops_before_outer_harness() {
        for binary in ["node", "bun"] {
            let info = |pid| match pid {
                10 => Some((20, "inner".into(), format!("/bin/{binary}"))),
                20 => Some((1, "outer".into(), "/bin/codex".into())),
                _ => None,
            };
            assert!(nearest_harness_with(10, &info, &|_| None).is_none());
            assert_eq!(
                nearest_harness_with(10, &info, &|_| Some("claude".into())),
                Some((Harness::ClaudeCode, 10, "inner".into()))
            );
        }
    }

    #[test]
    fn legacy_start_time_shapes_are_not_evidence_of_exit() {
        let f = Fake {
            started: "Fri Oct 30 18:46:07 2026".into(),
            pid: 42,
            branch: "b",
            probes: Cell::new(0),
        };
        let mut r = update(None, Harness::Codex, ev("SessionStart"), Utc::now(), &f);
        assert!(has_c_start_time(&r));
        for legacy in [
            "Tue 10 Nov 21:39:39 2026",
            "Di. 10 Nov. 21:39:39 2026",
            "garbage",
            "",
        ] {
            r.pid_started = Some(legacy.into());
            assert!(!has_c_start_time(&r));
        }
        r.pid_started = None;
        assert!(!has_c_start_time(&r));
    }

    #[test]
    fn liveness_fill_needs_only_matching_start_time() {
        let r = Record {
            harness: Harness::Codex,
            session_id: "test".into(),
            app: String::new(),
            cwd: String::new(),
            pid: Some(42),
            pid_started: Some("start".into()),
            last_event: "Stop".into(),
            updated: Utc::now(),
            branches: vec![],
            transcript_path: None,
            permission_mode: None,
            permission_mode_observed_at: None,
        };
        let mut alive = HashMap::new();
        fill_alive_pids(std::slice::from_ref(&r), &mut alive, &|pid| {
            (pid == 42).then(|| "start".into())
        });
        assert!(is_alive(&r, &alive));
        let mut reused = r.clone();
        reused.pid_started = Some("other".into());
        assert!(!is_alive(&reused, &alive));
        fill_alive_pids(&[r], &mut alive, &|_| None);
        assert_eq!(alive.get(&42).map(String::as_str), Some("start"));
    }

    #[test]
    fn branches_keep_the_folder_they_were_seen_in() {
        let f = Fake {
            started: "Fri Oct 30 18:46:07 2026".into(),
            pid: 1,
            branch: "old-feature",
            probes: Cell::new(0),
        };
        let mut e = ev("PostToolUse");
        e.cwd = Some("/a".into());
        let r = update(None, Harness::Codex, e, at(1), &f);
        let f = Fake {
            started: "Fri Oct 30 18:46:07 2026".into(),
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

    #[test]
    fn a_reused_pid_vouches_only_for_its_own_record() {
        let rec = |started: &str| Record {
            harness: Harness::ClaudeCode,
            session_id: started.into(),
            app: "CLI".into(),
            cwd: "/tmp".into(),
            pid: Some(42),
            pid_started: Some(started.into()),
            last_event: "UserPromptSubmit".into(),
            updated: Utc::now(),
            branches: Vec::new(),
            transcript_path: None,
            permission_mode: None,
            permission_mode_observed_at: None,
        };
        let (old, new) = (
            rec("Mon Oct 26 10:00:00 2026"),
            rec("Sat Oct 31 10:00:00 2026"),
        );
        let started = HashMap::from([(42, "Sat Oct 31 10:00:00 2026".to_string())]);
        assert!(is_alive(&new, &started));
        assert!(!is_alive(&old, &started));
    }
}

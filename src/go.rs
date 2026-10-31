//! `provenance go`: open the session behind a PR where it lives.
//!
//! A session whose process is up is never started a second time: `go`
//! switches to the terminal tab or app it runs in. Other sessions are resumed
//! with the harness's own command in the session's folder, or opened in the
//! desktop app that owns them.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::live::State;
use crate::matching::SessionMatch;
use crate::sessions::Harness;

#[derive(Debug, PartialEq)]
pub enum Action {
    /// Open a link: a web session, or a thread in a desktop app.
    Open(String),
    /// Bring an app forward when there is no link to the session itself.
    App {
        bundle: &'static str,
        name: &'static str,
    },
    /// The session's process is up: switch to where it runs.
    Focus { pid: u32, cwd: String },
    /// Start the harness on the session again, in its folder.
    Resume { cwd: String, argv: Vec<String> },
    /// The session can't be opened from here, and why.
    Explain(String),
}

/// What is known about the process holding a session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Process {
    /// The session's recorded process is still that process.
    Live(u32),
    /// Its recorded process has exited, or nothing could hold it.
    Gone,
    /// A process may hold it, but which one isn't known (OpenCode).
    MaybeHeld,
    /// No record of its process: hooks weren't installed when it ran.
    Unknown,
}

/// Links into desktop apps, looked up from each app's own state.
pub trait Links {
    fn t3(&self, session_id: &str) -> Option<String>;
    fn claude_desktop(&self, session_id: &str) -> Option<String>;
}

/// What `go` does for one session; `machine` is this machine's name in stamps.
pub fn plan(
    m: &SessionMatch,
    process: Process,
    machine: Option<&str>,
    links: &dyn Links,
) -> Action {
    let s = &m.sighting;
    if let Some(url) = &s.url {
        return Action::Open(url.clone());
    }
    if let Some(other) = &s.machine
        && Some(other.as_str()) != machine
    {
        return Action::Explain(format!(
            "it ran on machine {other}, not this one ({})",
            machine.unwrap_or("unknown")
        ));
    }
    match s.app.as_str() {
        "T3 Code" => {
            return match links.t3(&s.session_id) {
                Some(url) => Action::Open(url),
                None => Action::App {
                    bundle: "com.t3tools.t3code",
                    name: "T3 Code",
                },
            };
        }
        "Claude desktop" => {
            return match links.claude_desktop(&s.session_id) {
                Some(url) => Action::Open(url),
                None => Action::App {
                    bundle: "com.anthropic.claudefordesktop",
                    name: "Claude",
                },
            };
        }
        "Codex Desktop" => {
            return Action::App {
                bundle: "com.openai.codex",
                name: "Codex",
            };
        }
        "Conductor" => {
            return Action::App {
                bundle: "com.conductor.app",
                name: "Conductor",
            };
        }
        _ => {}
    }
    let argv = resume_argv(s.harness, &s.session_id);
    match process {
        Process::Live(pid) if m.state != Some(State::Ended) => Action::Focus {
            pid,
            cwd: s.cwd.clone(),
        },
        Process::MaybeHeld => Action::Explain(format!(
            "OpenCode is running and may have it open; switch to it there, or run `{}` in {} if it's closed",
            argv.join(" "),
            s.cwd
        )),
        _ if s.cwd.is_empty() => Action::Explain("its folder isn't known".into()),
        _ => Action::Resume {
            cwd: s.cwd.clone(),
            argv,
        },
    }
}

/// Each harness's own command for reopening a session.
pub fn resume_argv(harness: Harness, id: &str) -> Vec<String> {
    let args: &[&str] = match harness {
        Harness::ClaudeCode => &["claude", "--resume"],
        Harness::Codex => &["codex", "resume"],
        Harness::OpenCode => &["opencode", "-s"],
        Harness::Pi => &["pi", "--session"],
    };
    args.iter()
        .map(|a| a.to_string())
        .chain([id.to_string()])
        .collect()
}

/// A PR given as `123`, `owner/name#123` or a GitHub PR URL: its repository
/// (if named) and number.
pub fn parse_pr(arg: &str) -> Result<(Option<String>, u64)> {
    let arg = arg.trim().trim_end_matches('/');
    let bad = || anyhow::anyhow!("expected a PR number, owner/name#number or PR URL, got {arg:?}");
    if let Some(rest) = arg
        .strip_prefix("https://github.com/")
        .or_else(|| arg.strip_prefix("github.com/"))
    {
        let path = rest.split(['?', '#']).next().unwrap_or_default();
        let parts: Vec<&str> = path.split('/').collect();
        return match parts.as_slice() {
            [owner, name, "pull", n, ..] if !owner.is_empty() && !name.is_empty() => {
                Ok((Some(format!("{owner}/{name}")), number(n).ok_or_else(bad)?))
            }
            _ => Err(bad()),
        };
    }
    if let Some(n) = arg.strip_prefix('#') {
        return Ok((None, number(n).ok_or_else(bad)?));
    }
    if let Some((repo, n)) = arg.split_once('#') {
        if repo.split('/').count() != 2 || repo.split('/').any(str::is_empty) {
            return Err(bad());
        }
        return Ok((Some(repo.to_string()), number(n).ok_or_else(bad)?));
    }
    Ok((None, number(arg).ok_or_else(bad)?))
}

fn number(s: &str) -> Option<u64> {
    s.parse().ok().filter(|&n| n > 0)
}

/// The current folder's GitHub repository, as `gh` sees it.
pub fn current_repo() -> Result<String> {
    let out = Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .output()
        .context("running gh (is the GitHub CLI installed?)")?;
    let repo = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || repo.is_empty() {
        bail!("this folder isn't a GitHub repository; name the PR as owner/name#number");
    }
    Ok(repo)
}

/// The action as a shell command or note, for `--print`.
pub fn describe(action: &Action) -> String {
    match action {
        Action::Open(url) => format!("open {}", quote(url)),
        Action::App { bundle, .. } => format!("open -b {bundle}"),
        Action::Focus { pid, .. } => match where_running(*pid) {
            Some(w) => format!("running as PID {pid} on {}{}", w.tty, w.app_note()),
            None => format!("running as PID {pid}, with no terminal"),
        },
        Action::Resume { cwd, argv } => format!(
            "cd {} && {}",
            quote(cwd),
            argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
        ),
        Action::Explain(why) => why.clone(),
    }
}

/// Carry out the action. A resume replaces this process with the harness.
pub fn run(action: Action) -> Result<()> {
    match action {
        Action::Open(url) => open(&[&url]),
        Action::App { bundle, name } => {
            open(&["-b", bundle])?;
            eprintln!("{name} has no link to the session; find it in the app.");
            Ok(())
        }
        Action::Focus { pid, cwd } => focus(pid, &cwd),
        Action::Resume { cwd, argv } => {
            if !Path::new(&cwd).is_dir() {
                bail!(
                    "the session's folder is gone: {cwd}\nto reopen it elsewhere: {}",
                    argv.join(" ")
                );
            }
            eprintln!("Resuming in {}: {}", tilde(&cwd), argv.join(" "));
            let err = Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir(&cwd)
                .exec();
            Err(err).with_context(|| format!("starting {}", argv[0]))
        }
        Action::Explain(why) => bail!("can't open this session: {why}"),
    }
}

/// `path` with the home folder shown as `~`.
fn tilde(path: &str) -> String {
    tilde_in(path, std::env::var_os("HOME").as_deref())
}

fn tilde_in(path: &str, home: Option<&std::ffi::OsStr>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return path.to_string();
    };
    match Path::new(path).strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.to_string(),
    }
}

fn open(args: &[&str]) -> Result<()> {
    let status = Command::new("open")
        .args(args)
        .status()
        .context("running open")?;
    if !status.success() {
        bail!("open {} failed", args.join(" "));
    }
    Ok(())
}

/// The terminal a process runs on, and the app that owns it.
struct Running {
    tty: String,
    /// Path of the outermost `.app` among the process's ancestors.
    app: Option<PathBuf>,
}

impl Running {
    fn app_name(&self) -> Option<String> {
        Some(
            self.app
                .as_ref()?
                .file_stem()?
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn app_note(&self) -> String {
        self.app_name()
            .map(|a| format!(" in {a}"))
            .unwrap_or_default()
    }
}

fn where_running(pid: u32) -> Option<Running> {
    let out = Command::new("ps")
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let tty = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tty.is_empty() || tty.starts_with('?') {
        return None;
    }
    let mut app = None;
    let mut p = pid;
    for _ in 0..16 {
        let Some((ppid, _, comm)) = crate::live::ps_info(p) else {
            break;
        };
        if let Some(a) = outer_app(&comm) {
            app = Some(a);
            break;
        }
        if ppid <= 1 {
            break;
        }
        p = ppid;
    }
    Some(Running {
        tty: format!("/dev/{tty}"),
        app,
    })
}

/// `/Applications/iTerm.app/Contents/MacOS/iTerm2` -> `/Applications/iTerm.app`.
fn outer_app(path: &str) -> Option<PathBuf> {
    let end = path.find(".app/")? + ".app".len();
    Some(PathBuf::from(&path[..end]))
}

fn focus(pid: u32, cwd: &str) -> Result<()> {
    let Some(w) = where_running(pid) else {
        bail!("the session is running as PID {pid} with no terminal (a background or app process)");
    };
    // Terminal and iTerm2 find the tab by its tty. Ghostty can't report a
    // terminal's tty, so it finds the terminal by folder.
    let script = match w.app_name().as_deref() {
        Some("Terminal") => Some((TERMINAL_SCRIPT, w.tty.as_str())),
        Some("iTerm") => Some((ITERM_SCRIPT, w.tty.as_str())),
        Some("Ghostty") => Some((GHOSTTY_SCRIPT, cwd)),
        _ => None,
    };
    if let Some((script, key)) = script {
        let out = Command::new("osascript")
            .args(["-e", script, key])
            .output()
            .context("running osascript")?;
        let reply = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success()
            && let Some(n) = reply.strip_prefix("ok")
        {
            if n.trim().parse::<u32>().is_ok_and(|n| n > 1) {
                eprintln!(
                    "{} Ghostty terminals are in {cwd}; switched to the first.",
                    n.trim()
                );
            }
            return Ok(());
        }
    }
    match &w.app {
        Some(app) => {
            open(&[&app.to_string_lossy()])?;
            eprintln!(
                "The session is running on {}{}; switch to that tab.",
                w.tty,
                w.app_note()
            );
            Ok(())
        }
        None => bail!(
            "the session is running on {} (PID {pid}), in no app found (tmux or ssh?)",
            w.tty
        ),
    }
}

const TERMINAL_SCRIPT: &str = r#"on run argv
    tell application id "com.apple.Terminal"
        repeat with w in windows
            repeat with t in tabs of w
                if tty of t is item 1 of argv then
                    set selected of t to true
                    set index of w to 1
                    activate
                    return "ok"
                end if
            end repeat
        end repeat
    end tell
end run"#;

const ITERM_SCRIPT: &str = r#"on run argv
    tell application id "com.googlecode.iterm2"
        repeat with w in windows
            repeat with t in tabs of w
                repeat with s in sessions of t
                    if tty of s is item 1 of argv then
                        select w
                        select t
                        select s
                        activate
                        return "ok"
                    end if
                end repeat
            end repeat
        end repeat
    end tell
end run"#;

const GHOSTTY_SCRIPT: &str = r#"on run argv
    tell application id "com.mitchellh.ghostty"
        set found to (every terminal whose working directory is item 1 of argv)
        if (count of found) is 0 then return "none"
        focus item 1 of found
        activate
        return "ok " & (count of found)
    end tell
end run"#;

fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Links read from T3 Code's and Claude desktop's local state.
pub struct AppLinks {
    pub home: PathBuf,
}

impl Links for AppLinks {
    /// `t3code://threads/<environment>/<thread>` for the T3 thread whose
    /// provider thread is this session.
    fn t3(&self, session_id: &str) -> Option<String> {
        if !safe_id(session_id) {
            return None;
        }
        let dir = self.home.join(".t3/userdata");
        let env = std::fs::read_to_string(dir.join("environment-id")).ok()?;
        let out = Command::new("sqlite3")
            .arg("-readonly")
            .arg(dir.join("statev2.sqlite"))
            .arg(format!(
                "select p.thread_id from orchestration_v2_projection_provider_threads p \
                 join orchestration_v2_projection_threads t on t.thread_id = p.thread_id \
                 where json_extract(p.payload_json, '$.nativeThreadRef.nativeId') = '{session_id}' \
                 and t.deleted_at is null order by p.updated_at desc limit 1"
            ))
            .output()
            .ok()?;
        let thread = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !thread.is_empty()).then(|| {
            format!(
                "t3code://threads/{}/{}",
                encode(env.trim()),
                encode(&thread)
            )
        })
    }

    /// `claude://code/continue?session=local_<id>` for the Claude desktop
    /// session running this CLI session. The app ignores archived sessions.
    fn claude_desktop(&self, session_id: &str) -> Option<String> {
        let root = self
            .home
            .join("Library/Application Support/Claude/claude-code-sessions");
        for account in std::fs::read_dir(root).ok()?.flatten() {
            for org in std::fs::read_dir(account.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                for f in std::fs::read_dir(org.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let Ok(bytes) = std::fs::read(f.path()) else {
                        continue;
                    };
                    let Ok(d) = serde_json::from_slice::<DesktopSession>(&bytes) else {
                        continue;
                    };
                    if d.cli_session_id.as_deref() == Some(session_id) && !d.is_archived {
                        return safe_id(&d.session_id)
                            .then(|| format!("claude://code/continue?session={}", d.session_id));
                    }
                }
            }
        }
        None
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSession {
    session_id: String,
    cli_session_id: Option<String>,
    #[serde(default)]
    is_archived: bool,
}

/// Session IDs are UUIDs or prefixed IDs; anything else isn't put in a query or link.
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Percent-encode one URL path segment.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::MatchKind;
    use crate::sessions::Sighting;
    use chrono::Utc;

    struct NoLinks;
    impl Links for NoLinks {
        fn t3(&self, _: &str) -> Option<String> {
            None
        }
        fn claude_desktop(&self, _: &str) -> Option<String> {
            None
        }
    }

    fn session(harness: Harness, app: &str, state: Option<State>) -> SessionMatch {
        SessionMatch {
            kind: MatchKind::Stamp,
            state,
            sighting: Sighting {
                harness,
                session_id: "abc-123".into(),
                app: app.into(),
                cwd: "/tmp/work".into(),
                branch: "b".into(),
                repo_url: None,
                last_seen: Utc::now(),
                log_path: None,
                machine: None,
                url: None,
            },
        }
    }

    #[test]
    fn a_live_session_is_focused_never_resumed() {
        let m = session(Harness::ClaudeCode, "CLI", Some(State::Waiting));
        assert_eq!(
            plan(&m, Process::Live(42), None, &NoLinks),
            Action::Focus {
                pid: 42,
                cwd: "/tmp/work".into()
            }
        );
        let m = session(Harness::OpenCode, "OpenCode", None);
        assert!(matches!(
            plan(&m, Process::MaybeHeld, None, &NoLinks),
            Action::Explain(_)
        ));
    }

    #[test]
    fn a_session_whose_process_is_gone_resumes_even_if_it_looked_waiting() {
        let m = session(Harness::ClaudeCode, "CLI", Some(State::Waiting));
        assert!(matches!(
            plan(&m, Process::Gone, None, &NoLinks),
            Action::Resume { .. }
        ));
        let m = session(Harness::OpenCode, "OpenCode", Some(State::Stopped));
        assert!(matches!(
            plan(&m, Process::Gone, None, &NoLinks),
            Action::Resume { .. }
        ));
        // A closed session whose process lingers is reopened, not focused.
        let m = session(Harness::Codex, "CLI", Some(State::Ended));
        assert!(matches!(
            plan(&m, Process::Live(7), None, &NoLinks),
            Action::Resume { .. }
        ));
    }

    #[test]
    fn ended_sessions_resume_with_their_harness_in_their_folder() {
        for (h, cmd) in [
            (Harness::ClaudeCode, "claude --resume abc-123"),
            (Harness::Codex, "codex resume abc-123"),
            (Harness::OpenCode, "opencode -s abc-123"),
            (Harness::Pi, "pi --session abc-123"),
        ] {
            let m = session(h, "CLI", Some(State::Ended));
            assert_eq!(
                plan(&m, Process::Unknown, None, &NoLinks),
                Action::Resume {
                    cwd: "/tmp/work".into(),
                    argv: cmd.split(' ').map(String::from).collect()
                }
            );
        }
    }

    #[test]
    fn app_sessions_open_in_their_app() {
        struct Some3;
        impl Links for Some3 {
            fn t3(&self, id: &str) -> Option<String> {
                Some(format!("t3code://threads/e/{id}"))
            }
            fn claude_desktop(&self, _: &str) -> Option<String> {
                None
            }
        }
        let m = session(Harness::Codex, "T3 Code", Some(State::Running));
        assert_eq!(
            plan(&m, Process::Live(1), None, &Some3),
            Action::Open("t3code://threads/e/abc-123".into())
        );
        let m = session(Harness::ClaudeCode, "Claude desktop", None);
        assert!(matches!(
            plan(&m, Process::Unknown, None, &Some3),
            Action::App { name: "Claude", .. }
        ));
    }

    #[test]
    fn other_machines_and_web_sessions() {
        let mut m = session(Harness::ClaudeCode, "CLI", None);
        m.sighting.machine = Some("m-other".into());
        assert!(matches!(
            plan(&m, Process::Unknown, Some("m-mine"), &NoLinks),
            Action::Explain(_)
        ));
        m.sighting.machine = Some("m-mine".into());
        assert!(matches!(
            plan(&m, Process::Unknown, Some("m-mine"), &NoLinks),
            Action::Resume { .. }
        ));
        m.sighting.url = Some("https://claude.ai/code/session_1".into());
        assert_eq!(
            plan(&m, Process::Unknown, None, &NoLinks),
            Action::Open("https://claude.ai/code/session_1".into())
        );
    }

    #[test]
    fn pr_arguments() {
        assert_eq!(parse_pr("12").unwrap(), (None, 12));
        assert_eq!(parse_pr("#12").unwrap(), (None, 12));
        assert_eq!(parse_pr("a/b#7").unwrap(), (Some("a/b".into()), 7));
        assert_eq!(
            parse_pr("https://github.com/a/b/pull/9/files").unwrap(),
            (Some("a/b".into()), 9)
        );
        for url in [
            "https://github.com/a/b/pull/9",
            "https://github.com/a/b/pull/9/",
            "https://github.com/a/b/pull/9/commits",
            "https://github.com/a/b/pull/9#issuecomment-1",
            "https://github.com/a/b/pull/9?diff=split",
            "github.com/a/b/pull/9",
        ] {
            assert_eq!(parse_pr(url).unwrap(), (Some("a/b".into()), 9), "{url}");
        }
        assert!(parse_pr("https://github.com//b/pull/9").is_err());
        assert!(parse_pr("https://example.com/a/b/pull/9").is_err());
        assert!(parse_pr("0").is_err());
        assert!(parse_pr("a#7").is_err());
        assert!(parse_pr("https://github.com/a/b/issues/9").is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(
            outer_app("/Applications/iTerm.app/Contents/MacOS/iTerm2"),
            Some(PathBuf::from("/Applications/iTerm.app"))
        );
        assert_eq!(outer_app("/bin/zsh"), None);
        assert_eq!(quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(encode("thread:a%3Ab"), "thread%3Aa%253Ab");
        assert!(!safe_id("x' or 1=1 --"));
    }

    #[test]
    fn home_is_shown_as_tilde() {
        let t = |path: &str, home: &str| tilde_in(path, Some(std::ffi::OsStr::new(home)));
        assert_eq!(t("/Users/t/code", "/Users/t"), "~/code");
        assert_eq!(t("/Users/t", "/Users/t/"), "~");
        assert_eq!(t("/Users/t2/code", "/Users/t"), "/Users/t2/code");
        assert_eq!(t("/Users//t/code", "/Users/t"), "~/code");
        assert_eq!(t("/Users/./t/code", "/Users/t"), "~/code");
        assert_eq!(t("/Users/t", "/Users//t"), "~");
        assert_eq!(t("/Users//é/code", "/Users/é"), "~/code");
        assert_eq!(t("/tmp/x", ""), "/tmp/x");
        assert_eq!(tilde_in("/tmp/x", None), "/tmp/x");
    }
}

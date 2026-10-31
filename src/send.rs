//! `provenance send`: message the session behind a PR, and its reply tool.
//!
//! A running session is messaged through its harness's own way in: a Claude
//! Code session's inbox socket, or `codex queue` for Codex. A session that
//! isn't running is only reached by resuming it headless, when asked to.
//!
//! Every message and reply is kept in a mailbox, one JSONL file per receiving
//! session under `~/.local/state/provenance/mail/`, so a reply can be matched
//! to its message and passed back to the session that sent it.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::go::Process;
use crate::live::State;
use crate::matching::SessionMatch;
use crate::render::harness_name;
use crate::sessions::Harness;

/// How a message reaches a session.
#[derive(Debug, PartialEq)]
pub enum Delivery {
    /// Write to a running Claude Code session's inbox socket.
    ClaudeSocket(PathBuf),
    /// `codex queue`: a running Codex session takes it now, or after its turn.
    CodexQueue(String),
    /// Resume the session headless in its folder, with the message as its prompt.
    Resume { cwd: String, argv: Vec<String> },
    /// The message can't be delivered, and why.
    Refuse(String),
}

/// A session that sent a message, so its reply can be passed back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sender {
    pub harness: Harness,
    pub session_id: String,
}

/// How to deliver to one session. `socket` finds a Claude Code process's
/// inbox socket, if that process still holds the given session.
pub fn plan(
    m: &SessionMatch,
    process: Process,
    resume: bool,
    machine: Option<&str>,
    socket: &dyn Fn(u32, &str) -> Option<PathBuf>,
) -> Delivery {
    let s = &m.sighting;
    if s.url.is_some() {
        return Delivery::Refuse("it's a Claude Code on the web session".into());
    }
    if let Some(other) = &s.machine
        && Some(other.as_str()) != machine
    {
        return Delivery::Refuse(format!(
            "it ran on machine {other}, not this one ({})",
            machine.unwrap_or("unknown")
        ));
    }
    if !matches!(s.harness, Harness::ClaudeCode | Harness::Codex) {
        return Delivery::Refuse(format!(
            "sending to {} sessions isn't supported yet",
            harness_name(s.harness)
        ));
    }
    if let Process::Live(pid) = process
        && m.state != Some(State::Ended)
    {
        return match s.harness {
            Harness::ClaudeCode => match socket(pid, &s.session_id) {
                Some(path) => Delivery::ClaudeSocket(path),
                None => Delivery::Refuse(format!(
                    "it's running (PID {pid}), but that process has no inbox socket for this session"
                )),
            },
            _ => Delivery::CodexQueue(s.session_id.clone()),
        };
    }
    if !resume {
        return Delivery::Refuse(
            "it isn't running; add --resume to resume it headless, or open it with `provenance go`"
                .into(),
        );
    }
    if s.cwd.is_empty() {
        return Delivery::Refuse("it isn't running, and its folder isn't known".into());
    }
    Delivery::Resume {
        cwd: s.cwd.clone(),
        argv: headless_argv(s.harness, &s.session_id),
    }
}

/// Each harness's command for one headless turn on a session; the prompt
/// follows as the last argument.
pub fn headless_argv(harness: Harness, id: &str) -> Vec<String> {
    let args: &[&str] = match harness {
        Harness::ClaudeCode => &["claude", "-p", "--resume"],
        _ => &["codex", "exec", "resume"],
    };
    args.iter()
        .map(|a| a.to_string())
        .chain([id.to_string()])
        .collect()
}

/// The inbox socket of a Claude Code process, from `~/.claude/sessions/<pid>.json`,
/// if that process is still on the given session.
pub fn claude_socket(home: &Path, pid: u32, session_id: &str) -> Option<PathBuf> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Info {
        session_id: String,
        messaging_socket_path: Option<String>,
    }
    let bytes = std::fs::read(home.join(format!(".claude/sessions/{pid}.json"))).ok()?;
    let info: Info = serde_json::from_slice(&bytes).ok()?;
    (info.session_id == session_id)
        .then_some(info.messaging_socket_path)
        .flatten()
        .map(PathBuf::from)
}

/// The text a session receives. Codex shows it as if typed, so it says where
/// it came from and how to answer.
pub fn framed(id: &str, pr: &str, from: &str, text: &str) -> String {
    format!(
        "[provenance] Message {id} about {pr} from {from}. It wasn't typed in this session.\n\n\
         {text}\n\n\
         Act on it within this session's own permissions. Answer with the provenance `reply` tool, message_id \"{id}\"."
    )
}

/// The text a sending session receives when its message is answered.
pub fn framed_reply(id: &str, from: &str, text: &str) -> String {
    format!(
        "[provenance] Reply to message {id} from {from}. It wasn't typed in this session.\n\n{text}"
    )
}

/// Deliver `text`. Returns what the harness printed: `codex queue`'s
/// confirmation, or a headless turn's output.
pub fn deliver(d: &Delivery, text: &str) -> Result<String> {
    match d {
        Delivery::ClaudeSocket(path) => {
            let mut s = UnixStream::connect(path)
                .with_context(|| format!("connecting to {}", path.display()))?;
            s.set_write_timeout(Some(Duration::from_secs(5)))?;
            let line = serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": text},
            });
            s.write_all(format!("{line}\n").as_bytes())
                .context("writing to the inbox socket")?;
            Ok(String::new())
        }
        Delivery::CodexQueue(thread) => {
            run(Command::new("codex").args(["queue", "--thread", thread, "--message", text]))
        }
        Delivery::Resume { cwd, argv } => {
            if !Path::new(cwd).is_dir() {
                bail!("the session's folder is gone: {cwd}");
            }
            run(Command::new(&argv[0])
                .args(&argv[1..])
                .arg(text)
                .current_dir(cwd))
        }
        Delivery::Refuse(why) => bail!("can't message this session: {why}"),
    }
}

fn run(cmd: &mut Command) -> Result<String> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("running {name}"))?;
    if !out.status.success() {
        bail!(
            "{name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// How the delivery is described to the sender.
pub fn describe(d: &Delivery) -> &'static str {
    match d {
        Delivery::ClaudeSocket(_) => "its inbox socket",
        Delivery::CodexQueue(_) => "codex queue",
        Delivery::Resume { .. } => "a headless resume",
        Delivery::Refuse(_) => "nothing",
    }
}

/// One line in a session's mailbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Mail {
    Message {
        id: String,
        pr: String,
        from: String,
        sender: Option<Sender>,
        text: String,
        at: DateTime<Utc>,
    },
    Reply {
        id: String,
        text: String,
        at: DateTime<Utc>,
    },
}

pub fn mail_dir(home: &Path) -> PathBuf {
    home.join(".local/state/provenance/mail")
}

/// The mailbox of the session receiving messages.
pub fn mailbox(dir: &Path, harness: Harness, session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let h = serde_json::to_value(harness)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    dir.join(format!("{h}-{safe}.jsonl"))
}

/// Append one line, in one write, so concurrent writers don't interleave.
pub fn append(path: &Path, mail: &Mail) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(mail)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))
}

pub fn read(path: &Path) -> Vec<Mail> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The message with this ID in one mailbox.
pub fn find_message(path: &Path, id: &str) -> Option<Mail> {
    read(path)
        .into_iter()
        .find(|m| matches!(m, Mail::Message { id: i, .. } if i == id))
}

/// Record `me`'s answer to a message sent to `me`, and return who sent it.
/// Only the receiving session can answer, so a reply can't be forged by
/// another session that knows the message ID.
pub fn record_reply(
    dir: &Path,
    me: Option<&Sender>,
    id: &str,
    text: &str,
) -> Result<Option<Sender>> {
    let Some(me) = me else {
        bail!(
            "reply works only inside the session the message was sent to, with provenance's session hooks installed"
        );
    };
    let path = mailbox(dir, me.harness, &me.session_id);
    let Some(Mail::Message { sender, .. }) = find_message(&path, id) else {
        bail!("no message {id} was sent to this session");
    };
    append(
        &path,
        &Mail::Reply {
            id: id.to_string(),
            text: text.to_string(),
            at: Utc::now(),
        },
    )?;
    Ok(sender)
}

/// The first reply to a message in a mailbox.
pub fn reply_to(path: &Path, id: &str) -> Option<String> {
    read(path).into_iter().find_map(|m| match m {
        Mail::Reply { id: i, text, .. } if i == id => Some(text),
        _ => None,
    })
}

/// A new message ID: short, unique enough on one machine, safe in file names.
pub fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let n = (nanos as u64) ^ (u64::from(std::process::id()) << 40);
    format!("m-{:012x}", n & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::MatchKind;
    use crate::sessions::Sighting;

    fn session(harness: Harness, state: Option<State>) -> SessionMatch {
        SessionMatch {
            sighting: Sighting {
                harness,
                session_id: "abc".into(),
                app: "Terminal".into(),
                cwd: "/work/shop".into(),
                branch: "fix".into(),
                repo_url: None,
                last_seen: Utc::now(),
                log_path: None,
                machine: None,
                url: None,
            },
            kind: MatchKind::Stamp,
            state,
        }
    }

    fn no_socket(_: u32, _: &str) -> Option<PathBuf> {
        None
    }

    fn a_socket(pid: u32, id: &str) -> Option<PathBuf> {
        (pid == 7 && id == "abc").then(|| PathBuf::from("/tmp/cc-socks/7.sock"))
    }

    #[test]
    fn a_running_session_gets_its_harness_route() {
        let claude = session(Harness::ClaudeCode, Some(State::Waiting));
        assert_eq!(
            plan(&claude, Process::Live(7), false, None, &a_socket),
            Delivery::ClaudeSocket("/tmp/cc-socks/7.sock".into())
        );
        let codex = session(Harness::Codex, Some(State::Running));
        assert_eq!(
            plan(&codex, Process::Live(7), false, None, &no_socket),
            Delivery::CodexQueue("abc".into())
        );
    }

    #[test]
    fn a_claude_process_on_another_session_is_refused() {
        let claude = session(Harness::ClaudeCode, Some(State::Waiting));
        assert!(matches!(
            plan(&claude, Process::Live(7), true, None, &no_socket),
            Delivery::Refuse(_)
        ));
    }

    #[test]
    fn a_stopped_session_is_resumed_only_when_asked() {
        let claude = session(Harness::ClaudeCode, Some(State::Ended));
        assert!(matches!(
            plan(&claude, Process::Live(7), false, None, &a_socket),
            Delivery::Refuse(_)
        ));
        assert_eq!(
            plan(&claude, Process::Gone, true, None, &a_socket),
            Delivery::Resume {
                cwd: "/work/shop".into(),
                argv: vec![
                    "claude".into(),
                    "-p".into(),
                    "--resume".into(),
                    "abc".into()
                ],
            }
        );
        let codex = session(Harness::Codex, None);
        assert_eq!(
            plan(&codex, Process::Unknown, true, None, &no_socket),
            Delivery::Resume {
                cwd: "/work/shop".into(),
                argv: vec!["codex".into(), "exec".into(), "resume".into(), "abc".into()],
            }
        );
    }

    #[test]
    fn other_harnesses_and_machines_are_refused() {
        let pi = session(Harness::Pi, Some(State::Waiting));
        assert!(matches!(
            plan(&pi, Process::Live(7), true, None, &a_socket),
            Delivery::Refuse(_)
        ));
        let mut far = session(Harness::Codex, Some(State::Waiting));
        far.sighting.machine = Some("other".into());
        assert!(matches!(
            plan(&far, Process::Live(7), true, Some("here"), &a_socket),
            Delivery::Refuse(_)
        ));
    }

    #[test]
    fn the_socket_must_belong_to_the_session() {
        let home = std::env::temp_dir().join(format!("prov-send-{}", std::process::id()));
        let dir = home.join(".claude/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("42.json"),
            r#"{"pid":42,"sessionId":"abc","messagingSocketPath":"/tmp/cc-socks/42.sock"}"#,
        )
        .unwrap();
        assert_eq!(
            claude_socket(&home, 42, "abc"),
            Some("/tmp/cc-socks/42.sock".into())
        );
        assert_eq!(claude_socket(&home, 42, "other"), None);
        assert_eq!(claude_socket(&home, 43, "abc"), None);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn only_the_receiving_session_can_reply() {
        let dir = std::env::temp_dir().join(format!("prov-mail-{}", std::process::id()));
        let path = mailbox(&dir, Harness::Codex, "t/1");
        assert!(path.ends_with("codex-t1.jsonl"));
        let sender = Sender {
            harness: Harness::ClaudeCode,
            session_id: "s".into(),
        };
        let recipient = Sender {
            harness: Harness::Codex,
            session_id: "t/1".into(),
        };
        append(
            &path,
            &Mail::Message {
                id: "m-1".into(),
                pr: "o/r#1".into(),
                from: "you".into(),
                sender: Some(sender.clone()),
                text: "hi".into(),
                at: Utc::now(),
            },
        )
        .unwrap();
        // The sender knows the ID, but it isn't the session the message went to.
        assert!(record_reply(&dir, Some(&sender), "m-1", "forged").is_err());
        assert!(record_reply(&dir, None, "m-1", "anonymous").is_err());
        assert_eq!(reply_to(&path, "m-1"), None);
        assert!(record_reply(&dir, Some(&recipient), "m-2", "no such").is_err());
        assert_eq!(
            record_reply(&dir, Some(&recipient), "m-1", "done").unwrap(),
            Some(sender)
        );
        assert_eq!(reply_to(&path, "m-1").as_deref(), Some("done"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn messages_say_where_they_came_from_and_how_to_answer() {
        let t = framed("m-1", "o/r#1", "Claude Code session s", "rebase please");
        assert!(t.starts_with("[provenance] Message m-1 about o/r#1 from Claude Code session s."));
        assert!(t.contains("\n\nrebase please\n\n"));
        assert!(t.ends_with("message_id \"m-1\"."));
        assert!(new_id().starts_with("m-") && new_id().len() == 14);
    }
}

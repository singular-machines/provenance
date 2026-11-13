//! Cross-machine sends. A message for a session on another machine goes over
//! SSH to `provenance relay` on that machine, which delivers it with its own
//! local rules: only the machine running a session can tell whether it's
//! live, held in bypass mode or hosted by T3 Code. Machines are paired
//! explicitly with `provenance peer add`; nothing is relayed further.
//! Replies go back the same way, or are collected by the sending machine
//! when the receiving one can't reach it (`provenance peer pull`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::send::Sender;
use crate::sessions::Harness;

/// Bumped whenever the envelope or outcome changes incompatibly.
/// 2: `relay reply` passes replies back to the sending machine.
/// 3: `relay collect` hands replies to a sending machine that asks.
pub const PROTOCOL: u32 = 3;

/// Most message IDs one `relay collect` asks about.
pub const MAX_COLLECT: usize = 500;

/// Largest envelope `relay` reads from stdin.
pub const MAX_ENVELOPE: u64 = 4 * 1024 * 1024;

/// A paired machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    /// Its machine name in stamps, as it reported it.
    pub machine: String,
    /// The SSH destination that reaches it.
    pub ssh: String,
    /// Its provenance binary, found when pairing.
    pub bin: String,
    pub version: String,
    pub protocol: u32,
    pub added_at: DateTime<Utc>,
}

/// What `relay hello` reports about the machine it runs on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub version: String,
    pub machine: String,
    pub bin: String,
}

/// Who a relayed message is for, resolved on the receiving machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// A name in the receiving machine's own registry.
    Name { name: String },
    /// An exact session, from a PR's commit stamp.
    Session {
        harness: Harness,
        session_id: String,
    },
}

/// One relayed message, sent as JSON on `relay deliver`'s stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol: u32,
    pub id: String,
    pub target: Target,
    pub pr: Option<String>,
    pub from_machine: String,
    pub sender: Option<Sender>,
    pub text: String,
    /// Resume headless under the receiving machine's rules; never forced.
    pub resume: bool,
    /// Only say whether it could be delivered; record and send nothing.
    pub dry_run: bool,
}

/// A reply passed back to the machine a relayed message came from, as JSON
/// on `relay reply`'s stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyEnvelope {
    pub protocol: u32,
    /// The relayed message's ID.
    pub id: String,
    /// The machine the reply comes from: where the message was delivered.
    pub from_machine: String,
    /// The session that answered, which the message was delivered to.
    pub replier: Sender,
    pub text: String,
}

/// Asks a machine for the replies to messages this machine relayed there,
/// as JSON on `relay collect`'s stdin: the way back when that machine can't
/// reach this one over SSH.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collect {
    pub protocol: u32,
    /// The asking machine: replies are given only to messages from it.
    pub from_machine: String,
    /// The messages it is still waiting on.
    pub ids: Vec<String>,
}

/// `relay collect`'s answer: each reply as `relay reply` would have sent it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Collected {
    pub replies: Vec<ReplyEnvelope>,
    /// Why nothing could be collected.
    pub why: Option<String>,
}

/// The receiving machine's answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub accepted: bool,
    /// Why it was refused.
    pub why: Option<String>,
    /// It was (or would be) delivered by resuming the session headless.
    pub resumes: bool,
    pub to: String,
    pub how: String,
    pub socket_write: bool,
    pub note: Option<String>,
    pub output: String,
    pub recipient: Option<Sender>,
}

impl Outcome {
    pub fn refused(why: impl Into<String>) -> Self {
        Outcome {
            why: Some(why.into()),
            ..Outcome::default()
        }
    }
}

/// Runs `remote_command` on `destination` with `input` on its stdin and
/// returns its stdout. Production uses [`ssh`]; tests pass a fake.
pub type Transport<'a> = dyn Fn(&str, &str, &[u8]) -> Result<Vec<u8>> + 'a;

pub fn peers_path(home: &Path) -> PathBuf {
    crate::state::root(home).join("peers.json")
}

pub fn load(home: &Path) -> Result<Vec<Peer>> {
    match std::fs::read(peers_path(home)) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("reading peers.json"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).context("reading peers.json"),
    }
}

pub fn save(home: &Path, peers: &[Peer]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = peers_path(home);
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("peers.json.{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(peers)?)?;
    file.sync_all()?;
    std::fs::rename(&tmp, &path).context("writing peers.json")
}

/// The peer a machine name or SSH destination names.
pub fn find<'a>(peers: &'a [Peer], name: &str) -> Option<&'a Peer> {
    peers
        .iter()
        .find(|p| p.machine == name)
        .or_else(|| peers.iter().find(|p| p.ssh == name))
}

/// An SSH destination provenance will pass to `ssh`: a host alias,
/// `user@host` or an address. Never an option.
pub fn valid_destination(destination: &str) -> bool {
    !destination.is_empty()
        && destination.len() <= 255
        && !destination.starts_with('-')
        && destination
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._@:-".contains(c))
}

/// Single-quote for the remote shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Finds provenance on a machine whose non-interactive shell may not have
/// `~/.cargo/bin` on its PATH, then says hello.
const PROBE: &str = r#"p=$(command -v provenance 2>/dev/null); [ -x "$p" ] || p="$HOME/.cargo/bin/provenance"; [ -x "$p" ] || { echo "provenance is not installed there" >&2; exit 127; }; exec "$p" relay hello"#;

pub fn remote_command(bin: &str, subcommand: &str) -> String {
    format!("{} relay {subcommand}", quote(bin))
}

pub fn ssh_argv(destination: &str, remote_command: &str) -> Vec<String> {
    [
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "--",
        destination,
        remote_command,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The production transport: `ssh` with no prompts and a short connect
/// timeout, the input on stdin.
pub fn ssh(destination: &str, remote_command: &str, input: &[u8]) -> Result<Vec<u8>> {
    if !valid_destination(destination) {
        bail!("{destination:?} isn't an SSH destination provenance will use");
    }
    run_with_input(&ssh_argv(destination, remote_command), destination, input)
}

/// Runs `argv` with `input` on its stdin. Stdin is written on its own thread
/// so a large envelope can't deadlock against the child's output, and the
/// child is always waited for, so its exit status and stderr explain a
/// failure rather than the broken pipe they caused.
fn run_with_input(argv: &[String], destination: &str, input: &[u8]) -> Result<Vec<u8>> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {}", argv[0]))?;
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output()?;
    let written = writer
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("the stdin writer panicked")));
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if out.status.code() == Some(255) {
            bail!("couldn't reach {destination} over SSH: {stderr}");
        }
        bail!(
            "provenance on {destination} failed ({}): {stderr}",
            out.status
        );
    }
    written.with_context(|| format!("sending the message to {destination}"))?;
    Ok(out.stdout)
}

/// Asks the machine at `destination` who it is.
pub fn hello(destination: &str, bin: Option<&str>, transport: &Transport<'_>) -> Result<Hello> {
    let command = match bin {
        Some(bin) => remote_command(bin, "hello"),
        None => PROBE.to_string(),
    };
    let out = transport(destination, &command, b"")?;
    serde_json::from_slice(&out).with_context(|| {
        format!("unexpected answer from {destination}; is provenance 0.4 or later installed there?")
    })
}

/// Sends one envelope to a peer and returns its outcome.
pub fn call(peer: &Peer, envelope: &Envelope, transport: &Transport<'_>) -> Result<Outcome> {
    exchange(peer, "deliver", &serde_json::to_vec(envelope)?, transport)
}

/// Passes a reply back to the peer its message came from.
pub fn call_reply(
    peer: &Peer,
    reply: &ReplyEnvelope,
    transport: &Transport<'_>,
) -> Result<Outcome> {
    exchange(peer, "reply", &serde_json::to_vec(reply)?, transport)
}

/// Collects the replies a peer holds for messages relayed there from here.
pub fn call_collect(
    peer: &Peer,
    collect: &Collect,
    transport: &Transport<'_>,
) -> Result<Collected> {
    let collected: Collected = exchange(peer, "collect", &serde_json::to_vec(collect)?, transport)?;
    if let Some(why) = &collected.why {
        bail!("machine {} refused: {why}", peer.machine);
    }
    Ok(collected)
}

fn exchange<T: serde::de::DeserializeOwned>(
    peer: &Peer,
    subcommand: &str,
    input: &[u8],
    transport: &Transport<'_>,
) -> Result<T> {
    if peer.protocol != PROTOCOL {
        bail!(
            "machine {} was paired with relay protocol {}, this provenance {} speaks {PROTOCOL}; install the same version on both machines and pair again",
            peer.machine,
            peer.protocol,
            env!("CARGO_PKG_VERSION")
        );
    }
    let out = transport(&peer.ssh, &remote_command(&peer.bin, subcommand), input)?;
    serde_json::from_slice(&out).with_context(|| {
        format!(
            "unexpected answer from provenance on machine {}",
            peer.machine
        )
    })
}

/// Reads the envelope `relay deliver` was sent.
pub fn read_envelope(input: impl Read) -> Result<Envelope> {
    let envelope: Envelope = read_limited(input)?;
    check_protocol(envelope.protocol)?;
    Ok(envelope)
}

/// Reads the request `relay collect` was sent.
pub fn read_collect(input: impl Read) -> Result<Collect> {
    let collect: Collect = read_limited(input)?;
    check_protocol(collect.protocol)?;
    if collect.ids.len() > MAX_COLLECT {
        bail!("asked about more than {MAX_COLLECT} messages at once");
    }
    Ok(collect)
}

/// Reads the reply `relay reply` was sent.
pub fn read_reply(input: impl Read) -> Result<ReplyEnvelope> {
    let reply: ReplyEnvelope = read_limited(input)?;
    check_protocol(reply.protocol)?;
    Ok(reply)
}

fn read_limited<T: serde::de::DeserializeOwned>(input: impl Read) -> Result<T> {
    let mut bytes = Vec::new();
    input.take(MAX_ENVELOPE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ENVELOPE {
        bail!("relayed message is larger than {MAX_ENVELOPE} bytes");
    }
    serde_json::from_slice(&bytes).context("reading the relayed message")
}

fn check_protocol(protocol: u32) -> Result<()> {
    if protocol != PROTOCOL {
        bail!(
            "relay protocol {protocol} isn't supported by provenance {} here (protocol {PROTOCOL}); install the same version on both machines",
            env!("CARGO_PKG_VERSION")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations_never_become_options() {
        for ok in [
            "mbp",
            "nazreen@100.81.234.78",
            "box.tail-net.ts.net",
            "host:22",
        ] {
            assert!(valid_destination(ok), "{ok}");
        }
        for bad in ["", "-oProxyCommand=x", "a b", "a;b", "$(x)", "a'b"] {
            assert!(!valid_destination(bad), "{bad}");
        }
        let argv = ssh_argv("mbp", "'/bin/provenance' relay deliver");
        assert_eq!(argv[argv.len() - 3], "--");
        assert_eq!(argv[argv.len() - 2], "mbp");
        assert!(argv.contains(&"BatchMode=yes".to_string()));
    }

    #[test]
    fn an_early_exit_reports_its_stderr_not_a_broken_pipe() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("prov-ssh-{}", crate::send::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("ssh");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'fixture host is offline' >&2\nexit 255\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let argv = vec![fake.display().to_string()];
        let error = run_with_input(&argv, "mbp", &vec![b'x'; 1 << 20])
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "couldn't reach mbp over SSH: fixture host is offline"
        );
        let ok = dir.join("ok");
        std::fs::write(&ok, "#!/bin/sh\nwc -c\n").unwrap();
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o700)).unwrap();
        let out = run_with_input(&[ok.display().to_string()], "mbp", &vec![b'x'; 1 << 20]).unwrap();
        assert_eq!(String::from_utf8_lossy(&out).trim(), "1048576");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remote_binary_is_quoted_for_the_remote_shell() {
        assert_eq!(
            remote_command("/Users/o'neil/.cargo/bin/provenance", "deliver"),
            r"'/Users/o'\''neil/.cargo/bin/provenance' relay deliver"
        );
    }

    #[test]
    fn peers_are_found_by_machine_then_destination() {
        let peer = |machine: &str, ssh: &str| Peer {
            machine: machine.into(),
            ssh: ssh.into(),
            bin: "/bin/provenance".into(),
            version: "0.4.0".into(),
            protocol: PROTOCOL,
            added_at: Utc::now(),
        };
        let peers = [peer("m-1", "mbp"), peer("mbp", "mini")];
        assert_eq!(find(&peers, "mbp").unwrap().ssh, "mini");
        assert_eq!(find(&peers, "m-1").unwrap().ssh, "mbp");
        assert!(find(&peers, "nope").is_none());
    }

    #[test]
    fn envelopes_of_another_protocol_or_too_large_are_refused() {
        let envelope = Envelope {
            protocol: PROTOCOL + 1,
            id: "m-1".into(),
            target: Target::Name { name: "r".into() },
            pr: None,
            from_machine: "m-a".into(),
            sender: None,
            text: "hi".into(),
            resume: false,
            dry_run: false,
        };
        let bytes = serde_json::to_vec(&envelope).unwrap();
        assert!(
            read_envelope(&bytes[..])
                .unwrap_err()
                .to_string()
                .contains("protocol")
        );
        let big = vec![b' '; MAX_ENVELOPE as usize + 2];
        assert!(
            read_envelope(&big[..])
                .unwrap_err()
                .to_string()
                .contains("larger")
        );
        let ok = Envelope {
            protocol: PROTOCOL,
            ..envelope
        };
        assert_eq!(
            read_envelope(&serde_json::to_vec(&ok).unwrap()[..]).unwrap(),
            ok
        );
    }
}

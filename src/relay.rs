//! Cross-machine sends. A message for a session on another machine goes over
//! SSH to `provenance relay` on that machine, which delivers it with its own
//! local rules: only the machine running a session can tell whether it's
//! live, held in bypass mode or hosted by T3 Code. Machines are paired
//! explicitly with `provenance peer add`; nothing is relayed further.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::send::Sender;
use crate::sessions::Harness;

/// Bumped whenever the envelope or outcome changes incompatibly.
pub const PROTOCOL: u32 = 1;

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
    home.join(".local/state/provenance/peers.json")
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
    let argv = ssh_argv(destination, remote_command);
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running ssh")?;
    child.stdin.take().unwrap().write_all(input)?;
    let out = child.wait_with_output()?;
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
    let out = transport(
        &peer.ssh,
        &remote_command(&peer.bin, "deliver"),
        &serde_json::to_vec(envelope)?,
    )?;
    serde_json::from_slice(&out).with_context(|| {
        format!(
            "unexpected answer from provenance on machine {}",
            peer.machine
        )
    })
}

/// Reads the envelope `relay deliver` was sent.
pub fn read_envelope(input: impl Read) -> Result<Envelope> {
    let mut bytes = Vec::new();
    input.take(MAX_ENVELOPE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ENVELOPE {
        bail!("relayed message is larger than {MAX_ENVELOPE} bytes");
    }
    let envelope: Envelope =
        serde_json::from_slice(&bytes).context("reading the relayed message")?;
    if envelope.protocol != PROTOCOL {
        bail!(
            "relay protocol {} isn't supported by provenance {} here (protocol {PROTOCOL}); install the same version on both machines",
            envelope.protocol,
            env!("CARGO_PKG_VERSION")
        );
    }
    Ok(envelope)
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

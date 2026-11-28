//! Installs the user-level entry point for the repository's decision guidance.
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const START: &str = "<!-- BEGIN provenance decisions (managed by provenance) -->";
const END: &str = "<!-- END provenance decisions -->";
const BOOTSTRAP: &str = "\
# Provenance decision journal

Before implementation work in a Git checkout or worktree, run `provenance decisions guidance` from that directory. Recheck after changing repositories. This read-only command prints instructions only for the current repository when its owner has opted in. If the output is empty, continue without recording. If the check is blocked or fails, report it and leave the journal alone. Never enable recording yourself.
";

fn config_dir(home: &Path, variable: &str, default: &str) -> Result<PathBuf> {
    let path = std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(default));
    if !path.is_absolute() {
        bail!("{variable} must be an absolute path to install decision guidance");
    }
    Ok(path)
}

#[derive(Default, PartialEq, Eq)]
struct Snapshot {
    text: String,
    mode: Option<u32>,
}

impl Snapshot {
    fn read(path: &Path) -> Result<Self> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).with_context(|| format!("opening {}", path.display())),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            bail!("{} must be a regular file", path.display());
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(Self {
            text,
            mode: Some(metadata.permissions().mode() & 0o777),
        })
    }
}

struct Update {
    path: PathBuf,
    original: Snapshot,
    text: String,
}

impl Update {
    fn prepare(path: PathBuf, original: Snapshot, dedicated: bool) -> Result<Self> {
        let block = format!("{START}\n{BOOTSTRAP}{END}");
        let mut starts = original.text.match_indices(START);
        let mut ends = original.text.match_indices(END);
        let text = match (starts.next(), ends.next()) {
            (None, None) if original.text.is_empty() => format!("{block}\n"),
            (None, None) if !dedicated => format!("{block}\n\n{}", original.text),
            (Some((start, _)), Some((end, _)))
                if start < end && starts.next().is_none() && ends.next().is_none() =>
            {
                format!(
                    "{}{block}{}",
                    &original.text[..start],
                    &original.text[end + END.len()..]
                )
            }
            _ => bail!(
                "cannot identify the managed Provenance decision guidance in {}",
                path.display()
            ),
        };
        Ok(Self {
            path,
            original,
            text,
        })
    }

    fn install(&self) -> Result<()> {
        if self.original.text == self.text {
            return Ok(());
        }
        let dir = self.path.parent().context("guidance path has no parent")?;
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            ".provenance-guidance-{}.tmp",
            crate::send::new_id()
        ));
        let mode = self.original.mode.unwrap_or(0o600);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
            .with_context(|| format!("staging {}", self.path.display()))?;
        let result = (|| -> Result<()> {
            file.set_permissions(fs::Permissions::from_mode(mode))?;
            file.write_all(self.text.as_bytes())?;
            file.sync_all()?;
            if Snapshot::read(&self.path)? != self.original {
                bail!("guidance file changed while installing");
            }
            fs::rename(&tmp, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result.with_context(|| format!("installing {}", self.path.display()))
    }
}

pub fn install(home: &Path) -> Result<[PathBuf; 2]> {
    let codex = config_dir(home, "CODEX_HOME", ".codex")?;
    let claude =
        config_dir(home, "CLAUDE_CONFIG_DIR", ".claude")?.join("rules/provenance-decisions.md");
    let override_path = codex.join("AGENTS.override.md");
    let override_doc = Snapshot::read(&override_path)?;
    let (codex_path, codex_doc) = if override_doc.text.trim().is_empty() {
        let path = codex.join("AGENTS.md");
        let doc = Snapshot::read(&path)?;
        (path, doc)
    } else {
        (override_path, override_doc)
    };
    let codex = Update::prepare(codex_path, codex_doc, false)?;
    let claude = Update::prepare(claude.clone(), Snapshot::read(&claude)?, true)?;

    codex.install().context("setting up Codex guidance")?;
    claude
        .install()
        .context("Codex guidance is installed, but Claude Code guidance setup failed")?;
    Ok([codex.path, claude.path])
}

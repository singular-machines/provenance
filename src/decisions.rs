//! Explicit reports of implementation choices. No transcript inference or capture.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Subcommand)]
pub enum Command {
    /// Opt all existing and future PRs in this repository into explicit recording.
    Enable {
        /// owner/name or GitHub repository URL; defaults to the current repository.
        repo: Option<String>,
    },
    /// Stop new records; keep existing history readable.
    Disable {
        /// owner/name or GitHub repository URL; defaults to the current repository.
        repo: Option<String>,
    },
    /// Show whether this repository accepts new records (off by default).
    Status {
        /// owner/name or GitHub repository URL; defaults to the current repository.
        repo: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Append a reported choice; requires prior enablement for its repository.
    Record {
        pr: String,
        /// Supplied attribution, e.g. codex/<session-id>; not authenticated.
        #[arg(long)]
        actor: String,
        #[arg(long)]
        what: String,
        #[arg(long)]
        why: String,
        /// Concrete supporting references or observations; repeat as needed.
        #[arg(long, required = true)]
        evidence: Vec<String>,
        /// Whether the choice was explicitly requested by the user.
        #[arg(long, value_enum)]
        source: Source,
        /// Whether this rationale was recorded at the time or reconstructed later.
        #[arg(long, value_enum)]
        timing: Timing,
    },
    /// Read this PR's history, including while disabled.
    List {
        pr: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    UserDirected,
    Independent,
}

#[derive(Clone, Debug, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Timing {
    Contemporaneous,
    Retrospective,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    id: usize,
    recorded_at: DateTime<Utc>,
    actor: String,
    what: String,
    why: String,
    evidence: Vec<String>,
    source: Source,
    timing: Timing,
}

#[derive(Default, Serialize, Deserialize)]
struct History {
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Registry {
    version: u32,
    #[serde(default)]
    repos: BTreeMap<String, bool>,
    prs: BTreeMap<String, History>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: 2,
            repos: BTreeMap::new(),
            prs: BTreeMap::new(),
        }
    }
}

struct Store {
    dir: PathBuf,
}

impl Store {
    fn new(home: &Path) -> Self {
        Self {
            dir: crate::state::root(home).join("decisions"),
        }
    }

    fn lock(&self) -> Result<File> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        if fs::symlink_metadata(&self.dir)?.file_type().is_symlink() {
            bail!("decisions directory must not be a symlink");
        }
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.dir.join("history.lock"))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.lock()?;
        Ok(file)
    }

    fn read(&self) -> Result<Registry> {
        if let Ok(metadata) = fs::symlink_metadata(&self.dir)
            && metadata.file_type().is_symlink()
        {
            bail!("decisions directory must not be a symlink");
        }
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.dir.join("history.json"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Registry::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let mut registry: Registry = serde_json::from_str(&text)
            .context("reading decision history; refusing to overwrite invalid data")?;
        match registry.version {
            1 => {
                // Legacy per-PR opt-ins cannot authorize broader repository recording.
                // Preserve all entries, but require new explicit repository consent.
                registry.repos.clear();
                registry.version = 2;
            }
            2 => {}
            version => bail!("unsupported decision history version {version}"),
        }
        Ok(registry)
    }

    fn write(&self, registry: &Registry) -> Result<()> {
        let tmp = self
            .dir
            .join(format!("history.{}.tmp", crate::send::new_id()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&serde_json::to_vec_pretty(registry)?)?;
            file.sync_all()?;
            fs::rename(&tmp, self.dir.join("history.json"))?;
            File::open(&self.dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

fn canonical_repo(repo: &str) -> Result<String> {
    let parts: Vec<_> = repo.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || matches!(*part, "." | "..")
                || !part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    {
        bail!("expected repository owner/name with ASCII letters, digits, '.', '_' or '-'");
    }
    Ok(repo.to_ascii_lowercase())
}

fn repository(repo: Option<&str>) -> Result<String> {
    let Some(repo) = repo else {
        return canonical_repo(&crate::go::current_repo()?);
    };
    let repo = repo.trim().trim_end_matches('/');
    let repo = match repo
        .strip_prefix("https://github.com/")
        .or_else(|| repo.strip_prefix("github.com/"))
    {
        Some(path) => path.strip_suffix(".git").unwrap_or(path),
        None => repo,
    };
    canonical_repo(repo)
}

fn pr_key(pr: &str) -> Result<(String, String)> {
    let (repo, number) = crate::go::parse_pr(pr)?;
    let repo = match repo {
        Some(repo) => canonical_repo(&repo)?,
        None => repository(None)?,
    };
    Ok((repo.clone(), format!("{repo}#{number}")))
}

fn nonempty(label: &str, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        bail!("{label} must not be blank");
    }
    Ok(())
}

pub fn run(home: &Path, action: Command) -> Result<()> {
    let (repo, pr) = match &action {
        Command::Enable { repo } | Command::Disable { repo } | Command::Status { repo, .. } => {
            (repository(repo.as_deref())?, None)
        }
        Command::Record { pr, .. } | Command::List { pr, .. } => {
            let (repo, pr) = pr_key(pr)?;
            (repo, Some(pr))
        }
    };
    let store = Store::new(home);
    match action {
        Command::Status { json, .. } => {
            let registry = store.read()?;
            let enabled = registry.repos.get(&repo).copied().unwrap_or(false);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "repo": repo, "enabled": enabled
                    }))?
                );
            } else {
                println!(
                    "{repo}: recording {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }
        }
        Command::List { json, .. } => {
            let pr = pr.expect("list resolves a PR");
            let registry = store.read()?;
            let enabled = registry.repos.get(&repo).copied().unwrap_or(false);
            let entries = registry
                .prs
                .get(&pr)
                .map(|h| h.entries.as_slice())
                .unwrap_or_default();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "repo": repo, "pr": pr, "enabled": enabled, "entries": entries
                    }))?
                );
            } else {
                println!(
                    "{pr}: repository recording {} ({} entries)",
                    if enabled { "enabled" } else { "disabled" },
                    entries.len()
                );
                for entry in entries {
                    println!(
                        "{}",
                        crate::terminal_text(&serde_json::to_string_pretty(entry)?)
                    );
                }
            }
        }
        action => {
            let _lock = store.lock()?;
            let mut registry = store.read()?;
            match action {
                Command::Enable { .. } | Command::Disable { .. } => {
                    let enabled = matches!(action, Command::Enable { .. });
                    registry.repos.insert(repo.clone(), enabled);
                    store.write(&registry)?;
                    println!(
                        "{repo}: recording {} for all existing and future PRs",
                        if enabled { "enabled" } else { "disabled" }
                    );
                }
                Command::Record {
                    actor,
                    what,
                    why,
                    evidence,
                    source,
                    timing,
                    ..
                } => {
                    nonempty("actor", &actor)?;
                    nonempty("what", &what)?;
                    nonempty("why", &why)?;
                    for item in &evidence {
                        nonempty("evidence", item)?;
                    }
                    if !registry.repos.get(&repo).copied().unwrap_or(false) {
                        bail!(
                            "decision recording is disabled for repository {repo}; the owner must opt in with `provenance decisions enable {repo}` first"
                        );
                    }
                    let pr = pr.expect("record resolves a PR");
                    let history = registry.prs.entry(pr.clone()).or_default();
                    let id = history.entries.len() + 1;
                    history.entries.push(Entry {
                        id,
                        recorded_at: Utc::now(),
                        actor,
                        what,
                        why,
                        evidence,
                        source,
                        timing,
                    });
                    store.write(&registry)?;
                    println!("{pr}: recorded decision {id}");
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

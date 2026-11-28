//! Explicit reports of implementation choices. No transcript inference or capture.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::render::{self, Style};

#[derive(Subcommand)]
pub enum Command {
    /// Enable explicit recording for this repository and its PRs.
    Enable {
        /// owner/name or GitHub repository URL; defaults to the current repository.
        repo: Option<String>,
    },
    /// Stop new records and links; keep existing history readable.
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
    /// Print agent guidance for the current repository when recording is enabled.
    Guidance,
    /// Append a reported choice; requires prior enablement for its repository.
    Record {
        /// owner/name, GitHub repository URL, PR number, owner/name#number or PR URL.
        target: String,
        /// Supplied attribution, e.g. codex/<session-id>; not authenticated.
        #[arg(long)]
        actor: String,
        /// Required single-line summary, up to 76 display columns.
        #[arg(long, value_parser = parse_title)]
        title: String,
        #[arg(long)]
        what: String,
        #[arg(long)]
        why: String,
        /// An option considered and why it was passed over. Repeat for more.
        #[arg(
            long = "alternative",
            num_args = 2,
            value_names = ["OPTION", "REASON_NOT_SELECTED"],
            required = true,
            value_parser = parse_alternative_value
        )]
        alternatives: Vec<String>,
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
    /// Read a repository's full history or filter by PR, including while disabled.
    List {
        /// Repository or PR reference; defaults to the current repository.
        target: Option<String>,
        #[arg(long)]
        json: bool,
        /// Show only choices made by the agent's own judgment.
        #[arg(long)]
        independent: bool,
    },
    /// Show a report by short reference or by its original recording target and ID.
    Show {
        /// Short reference (repo:ID or #PR:ID), repository, or original PR.
        target: String,
        /// Short reference when a repository is named, or the original numeric ID.
        #[arg(value_name = "REF_OR_ID")]
        reference: Option<String>,
    },
    /// Attach a repository-recorded decision to a PR, a commit, or both.
    Link {
        /// Repository where the decision was recorded (owner/name or GitHub URL).
        repo: String,
        /// Decision ID returned by record or list for that repository.
        id: usize,
        /// PR number in the named repository, owner/name#number or PR URL.
        #[arg(long, required_unless_present = "commit")]
        pr: Option<String>,
        /// Full Git commit hash in the named repository.
        #[arg(long)]
        commit: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    UserDirected,
    Independent,
}

impl Source {
    fn label(&self) -> &'static str {
        match self {
            Self::UserDirected => "User-directed",
            Self::Independent => "Independent",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Timing {
    Contemporaneous,
    Retrospective,
}

impl Timing {
    fn label(&self) -> &'static str {
        match self {
            Self::Contemporaneous => "Contemporaneous",
            Self::Retrospective => "Retrospective rationale",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Alternative {
    option: String,
    reason_not_selected: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    id: usize,
    recorded_at: DateTime<Utc>,
    actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    what: String,
    why: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    alternatives: Vec<Alternative>,
    evidence: Vec<String>,
    source: Source,
    timing: Timing,
}

#[derive(Default, Serialize, Deserialize)]
struct History {
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct RepositoryEntry {
    #[serde(flatten)]
    entry: Entry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
}

#[derive(Serialize)]
struct ListedEntry<'a> {
    #[serde(flatten)]
    entry: &'a Entry,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<&'a str>,
}

impl<'a> ListedEntry<'a> {
    fn from_pr(entry: &'a Entry, target: Option<&'a str>) -> Self {
        Self {
            entry,
            target,
            pr: target,
            commit: None,
        }
    }

    fn from_repo(record: &'a RepositoryEntry, repo: &'a str) -> Self {
        Self {
            entry: &record.entry,
            target: Some(repo),
            pr: record.pr.as_deref(),
            commit: record.commit.as_deref(),
        }
    }
}

#[derive(Serialize)]
struct ListedHistory<'a> {
    repo: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<&'a str>,
    enabled: bool,
    entries: Vec<ListedEntry<'a>>,
}

const CURRENT_VERSION: u32 = 4;

#[derive(Serialize, Deserialize)]
struct Registry {
    version: u32,
    #[serde(default)]
    repos: BTreeMap<String, bool>,
    prs: BTreeMap<String, History>,
    #[serde(default)]
    repository_entries: BTreeMap<String, Vec<RepositoryEntry>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            repos: BTreeMap::new(),
            prs: BTreeMap::new(),
            repository_entries: BTreeMap::new(),
        }
    }
}

impl Registry {
    fn enabled(&self, repo: &str) -> bool {
        self.repos.get(repo).copied().unwrap_or(false)
    }

    fn require_enabled(&self, repo: &str) -> Result<()> {
        if !self.enabled(repo) {
            bail!(
                "decision recording is disabled for repository {repo}; the owner must opt in with `provenance decisions enable {repo}` first"
            );
        }
        Ok(())
    }

    fn listed_history<'a>(&'a self, repo: &'a str, pr: Option<&'a str>) -> ListedHistory<'a> {
        let repository_entries = self
            .repository_entries
            .get(repo)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let entries = if let Some(pr) = pr {
            self.prs
                .get(pr)
                .into_iter()
                .flat_map(|history| &history.entries)
                .map(|entry| ListedEntry::from_pr(entry, None))
                .chain(
                    repository_entries
                        .iter()
                        .filter(|record| record.pr.as_deref() == Some(pr))
                        .map(|record| ListedEntry::from_repo(record, repo)),
                )
                .collect()
        } else {
            let prefix = format!("{repo}#");
            let mut entries: Vec<_> = self
                .prs
                .iter()
                .filter(|(key, _)| key.starts_with(&prefix))
                .flat_map(|(key, history)| {
                    history
                        .entries
                        .iter()
                        .map(|entry| ListedEntry::from_pr(entry, Some(key)))
                })
                .chain(
                    repository_entries
                        .iter()
                        .map(|record| ListedEntry::from_repo(record, repo)),
                )
                .collect();
            entries.sort_by_key(|record| record.entry.recorded_at);
            entries
        };
        ListedHistory {
            repo,
            pr,
            enabled: self.enabled(repo),
            entries,
        }
    }

    fn decision<'a>(
        &'a self,
        repo: &'a str,
        pr: Option<&'a str>,
        id: usize,
    ) -> Option<ListedEntry<'a>> {
        if let Some(pr) = pr {
            let entry = self
                .prs
                .get(pr)?
                .entries
                .iter()
                .find(|entry| entry.id == id)?;
            return Some(ListedEntry::from_pr(entry, Some(pr)));
        }
        let record = self
            .repository_entries
            .get(repo)?
            .iter()
            .find(|record| record.entry.id == id)?;
        Some(ListedEntry::from_repo(record, repo))
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
            }
            2 | 3 | CURRENT_VERSION => {}
            version => bail!("unsupported decision history version {version}"),
        }
        // Older clients reject this version, so their writes cannot discard titles.
        registry.version = CURRENT_VERSION;
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

fn pr_key(pr: &str, default_repo: Option<&str>) -> Result<(String, String)> {
    let (repo, number) = crate::go::parse_pr(pr)?;
    let repo = match repo {
        Some(repo) => canonical_repo(&repo)?,
        None => repository(default_repo)?,
    };
    Ok((repo.clone(), format!("{repo}#{number}")))
}

fn decision_target(target: &str) -> Result<(String, Option<String>)> {
    if let Ok(repo) = repository(Some(target)) {
        return Ok((repo, None));
    }
    let (repo, pr) = pr_key(target, None)
        .with_context(|| format!("invalid decision target {target:?}; use a repository or PR"))?;
    Ok((repo, Some(pr)))
}

fn show_ref(reference: &str) -> Result<(Option<u64>, usize)> {
    let malformed = || anyhow::anyhow!("expected a decision reference such as repo:5 or '#988:3'");
    let (scope, id) = reference.split_once(':').ok_or_else(malformed)?;
    let pr = if scope == "repo" {
        None
    } else {
        let number = scope
            .strip_prefix('#')
            .filter(|number| number.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|number| number.parse::<u64>().ok())
            .filter(|number| *number > 0)
            .ok_or_else(malformed)?;
        Some(number)
    };
    let id = id
        .parse::<usize>()
        .ok()
        .filter(|number| *number > 0 && id.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(malformed)?;
    Ok((pr, id))
}

fn show_target(target: &str, reference: Option<&str>) -> Result<(String, Option<String>, usize)> {
    let Some(reference) = reference else {
        let (pr, id) = show_ref(target)?;
        let repo = git_repository()?.context(
            "could not infer a GitHub repository here; use `provenance decisions show owner/repo '<ref>'`",
        )?;
        let pr = pr.map(|number| format!("{repo}#{number}"));
        return Ok((repo, pr, id));
    };
    if reference.contains(':') {
        let (pr, id) = show_ref(reference)?;
        let repo = repository(Some(target))
            .context("a short decision reference takes a repository as its explicit scope")?;
        let pr = pr.map(|number| format!("{repo}#{number}"));
        Ok((repo, pr, id))
    } else {
        let (repo, pr) = decision_target(target)?;
        let id = reference
            .parse()
            .context("expected a numeric decision ID")?;
        Ok((repo, pr, id))
    }
}

fn github_remote(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?
        .trim_end_matches('/');
    canonical_repo(path.strip_suffix(".git").unwrap_or(path)).ok()
}

fn repo_from_remotes(remotes: &str) -> Option<String> {
    let mut repos = Vec::new();
    for line in remotes.lines() {
        let mut fields = line.split_whitespace();
        let (Some(remote), Some(url), Some("(fetch)")) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let Some(repo) = github_remote(url) {
            if remote == "origin" {
                return Some(repo);
            }
            repos.push(repo);
        }
    }
    repos.sort();
    repos.dedup();
    if let [repo] = repos.as_slice() {
        return Some(repo.clone());
    }
    None
}

fn git_repository() -> Result<Option<String>> {
    let output = ProcessCommand::new("git")
        .args(["remote", "-v"])
        .env("LC_ALL", "C")
        .output()
        .context("reading local Git remotes for decision scope")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") {
            return Ok(None);
        }
        bail!(
            "could not read local Git remotes for decision scope: {}",
            safe_text(stderr.trim())
        );
    }
    Ok(repo_from_remotes(&String::from_utf8_lossy(&output.stdout)))
}

fn local_repository() -> Result<String> {
    git_repository()?.context(
        "could not infer a GitHub repository here; use `provenance decisions list owner/repo`",
    )
}

fn guidance(store: &Store) -> Result<()> {
    let Some(repo) = git_repository()? else {
        return Ok(());
    };
    if !store.read()?.enabled(&repo) {
        return Ok(());
    }
    let guide = format!(
        "Provenance decision recording is enabled for {repo}.\n\n\
         Before recording, run `provenance decisions status {repo} --json` and continue only when enabled is true. The owner controls opt-in. If the check or a write is blocked or unavailable, report it and leave the journal alone.\n\n\
         Record only material trade-offs you chose independently or the user selected. A choice is worth revisiting if a plausible alternative could change product behavior, security or privacy, reliability or data integrity, performance or cost, or the scope of a change relative to a safeguard. Skip routine implementation mechanics, status or verification recaps, and restatements of requirements.\n\n\
         Use `provenance decisions record <TARGET> --actor <HARNESS/SESSION-ID> --title <TITLE> --what <CHOICE> --why <REASON> --alternative <OPTION> <REASON_NOT_SELECTED> --evidence <REFERENCE> --source <SOURCE> --timing <TIMING>`. The title is a concise single line of at most {TITLE_COLUMNS} display columns. Use {repo}#<PR> for a known PR in this repository, or {repo} when there is no PR. Supply the real current session identity.\n\n\
         Use --what for the choice and affected scope. Use --why for the benefit sought and any material cost, remaining risk, or assumption that would change the decision. Flag material uncertainty. Use --evidence for verifiable references available at the time, and repeat it for additional references.\n\n\
         Give at least one plausible option actually considered and why it was passed over. Repeat --alternative for each such option. Include keeping current behavior or deferring only if genuinely considered. Skip the report if no alternative was considered. For a retrospective report, leave out options merely suggested by later evidence. Leave out invented alternatives, risks, and evidence.\n\n\
         Use user-directed for an explicit user choice and independent for your own judgment. Use contemporaneous when recording now and retrospective for a later reconstruction. Keep secrets and private transcript excerpts out of the journal.\n\n\
         To attach an existing repository entry later, use `provenance decisions link {repo} <ID> --pr <PR>` or `--commit <FULL-SHA>`. Reuse the entry instead of recording the same choice again.\n"
    );
    stdout_result(std::io::stdout().lock().write_all(guide.as_bytes()))
}

const TITLE_COLUMNS: usize = 76;

fn parse_title(value: &str) -> Result<String, String> {
    let title = value.trim();
    let width = title.width();
    if width == 0
        || width > TITLE_COLUMNS
        || value
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}'))
    {
        return Err(format!(
            "use a nonblank single line of at most {TITLE_COLUMNS} display columns"
        ));
    }
    Ok(title.into())
}

fn parse_alternative_value(value: &str) -> Result<String, String> {
    nonempty("each alternative option and reason", value).map_err(|error| error.to_string())?;
    Ok(value.into())
}

const RECENT_LIMIT: usize = 10;

fn safe_text(text: &str) -> String {
    crate::terminal_text(text).replace('\n', "\n    ")
}

fn harness_label(actor: &str) -> &str {
    let Some((prefix, _)) = actor
        .split_once('/')
        .filter(|(prefix, _)| !prefix.is_empty())
    else {
        return "Harness unspecified";
    };
    let binary = match prefix {
        "claude-code" | "claude_code" => "claude",
        _ => prefix,
    };
    crate::sessions::Harness::from_binary(binary)
        .map(crate::render::harness_name)
        .unwrap_or(prefix)
}

fn print_detail(
    out: &mut impl Write,
    record: &ListedEntry<'_>,
    target: &str,
) -> std::io::Result<()> {
    let entry = record.entry;
    writeln!(
        out,
        "{}",
        safe_text(entry.title.as_deref().unwrap_or(&entry.what))
    )?;
    if entry.title.is_some() {
        writeln!(out, "  What: {}", safe_text(&entry.what))?;
    }
    writeln!(out, "  Why: {}", safe_text(&entry.why))?;
    if !entry.alternatives.is_empty() {
        writeln!(out, "  Alternatives considered:")?;
        for alternative in &entry.alternatives {
            writeln!(out, "    - {}", safe_text(&alternative.option))?;
            writeln!(
                out,
                "      Passed over: {}",
                safe_text(&alternative.reason_not_selected)
            )?;
        }
    }
    write!(
        out,
        "  Recorded {} | {} | {} | ",
        entry.recorded_at.format("%Y-%m-%d %H:%M UTC"),
        entry.source.label(),
        safe_text(harness_label(&entry.actor)),
    )?;
    match record.pr {
        Some(pr) => write!(out, "PR {}", safe_text(pr))?,
        None => write!(out, "No PR")?,
    }
    if let Some(commit) = record.commit {
        write!(out, " | Commit {}", safe_text(commit))?;
    }
    writeln!(out, " | {}", entry.timing.label())?;
    writeln!(out, "  Decision {} in {target}", entry.id)?;
    writeln!(out, "  Recorded at: {}", entry.recorded_at.to_rfc3339())?;
    writeln!(out, "  Actor: {}", safe_text(&entry.actor))?;
    writeln!(out, "  Evidence:")?;
    for item in &entry.evidence {
        writeln!(out, "    - {}", safe_text(item))?;
    }
    Ok(())
}

const TIME_HEADER: &str = "Time (UTC)";
const MAX_TIMELINE_COLUMNS: usize = 126;
const DEFAULT_TIMELINE_COLUMNS: usize = 106;
const MIN_TEXT_COLUMNS: usize = 24;
const COLUMN_GAP: usize = 2;
const RETROSPECTIVE: &str = "Retrospective";

fn ref_label(record: &ListedEntry<'_>, target: &str) -> String {
    let id = record.entry.id;
    match record.target.unwrap_or(target).split_once('#') {
        Some((_, number))
            if number
                .parse::<u64>()
                .is_ok_and(|pr| pr.to_string() == number) =>
        {
            format!("#{number}:{id}")
        }
        Some(_) => "Invalid ref".into(),
        None => format!("repo:{id}"),
    }
}

struct TimelineLayout {
    width: usize,
    ref_width: usize,
    text_widths: Option<(usize, usize)>,
}

impl TimelineLayout {
    fn new(style: &Style, refs: &[String]) -> Self {
        let width = style
            .width
            .unwrap_or(DEFAULT_TIMELINE_COLUMNS)
            .clamp(8, MAX_TIMELINE_COLUMNS);
        let ref_width = refs
            .iter()
            .map(|reference| reference.width())
            .max()
            .unwrap_or(3)
            .max(3);
        let fixed_width = TIME_HEADER.len() + ref_width + COLUMN_GAP * 3;
        let available = width.saturating_sub(fixed_width);
        let text_widths = (available >= MIN_TEXT_COLUMNS * 2).then(|| {
            let decision = (available * 44 / 100).clamp(MIN_TEXT_COLUMNS, 48);
            (decision, available - decision)
        });
        Self {
            width,
            ref_width,
            text_widths,
        }
    }

    fn print_header(&self, out: &mut impl Write, style: &Style) -> std::io::Result<()> {
        let heading = if let Some((decision, _)) = self.text_widths {
            format!(
                "{TIME_HEADER}  {}  {}  Why",
                render::pad("Ref", self.ref_width),
                render::pad("Decision", decision),
            )
        } else {
            format!("{TIME_HEADER}  Ref")
        };
        writeln!(out, "{}", style.paint("2", &heading))
    }

    fn print_row(
        &self,
        out: &mut impl Write,
        style: &Style,
        record: &ListedEntry<'_>,
        reference: &str,
        pr: Option<&str>,
    ) -> std::io::Result<()> {
        let entry = record.entry;
        let time = entry.recorded_at.format("%H:%M").to_string();
        let choice = entry.title.as_deref().unwrap_or(&entry.what);
        let indent = if let Some((choice_width, why_width)) = self.text_widths {
            let choice = wrap_timeline_text(choice, choice_width);
            let why = wrap_timeline_text(&entry.why, why_width);
            for idx in 0..choice.len().max(why.len()) {
                let clock = if idx == 0 { time.as_str() } else { "" };
                let row_ref = if idx == 0 { reference } else { "" };
                let decision = choice.get(idx).map(String::as_str).unwrap_or_default();
                let reason = why.get(idx).map(String::as_str).unwrap_or_default();
                let line = if reason.is_empty() {
                    decision.to_string()
                } else {
                    render::pad(decision, choice_width)
                };
                writeln!(
                    out,
                    "{}  {}  {}{separator}{reason}",
                    style.paint("2", &render::pad(clock, TIME_HEADER.len())),
                    style.paint("36", &render::pad(row_ref, self.ref_width)),
                    style.paint("1", &line),
                    separator = if reason.is_empty() { "" } else { "  " },
                )?;
            }
            TIME_HEADER.len() + self.ref_width + COLUMN_GAP * 2
        } else {
            writeln!(
                out,
                "{}  {}",
                style.paint("2", &time),
                style.paint("36", reference)
            )?;
            for line in wrap_timeline_text(choice, self.width - 2) {
                writeln!(out, "  {}", style.paint("1", &line))?;
            }
            for (index, line) in wrap_timeline_text(&entry.why, self.width - 7)
                .iter()
                .enumerate()
            {
                let prefix = if index == 0 { "  Why: " } else { "       " };
                writeln!(out, "{prefix}{line}")?;
            }
            2
        };
        self.print_metadata(out, style, record, pr, indent)
    }

    fn print_metadata(
        &self,
        out: &mut impl Write,
        style: &Style,
        record: &ListedEntry<'_>,
        pr: Option<&str>,
        indent: usize,
    ) -> std::io::Result<()> {
        let entry = record.entry;
        let mut metadata = format!("{} · {}", entry.source.label(), harness_label(&entry.actor));
        if let Some((_, number)) = record.pr.or(pr).and_then(|pr| pr.split_once('#')) {
            metadata.push_str(&format!(" · PR #{number}"));
        } else {
            metadata.push_str(" · No PR");
        }
        if let Some(commit) = record.commit {
            let short: String = commit.chars().take(12).collect();
            metadata.push_str(&format!(" · Commit {short}"));
        }
        let retrospective = matches!(entry.timing, Timing::Retrospective);
        if retrospective {
            metadata.push_str(" · ");
            metadata.push_str(RETROSPECTIVE);
        }
        let lines = wrap_timeline_text(&metadata, self.width - indent);
        for (index, line) in lines.iter().enumerate() {
            if retrospective
                && index + 1 == lines.len()
                && let Some(prefix) = line.strip_suffix(RETROSPECTIVE)
            {
                writeln!(
                    out,
                    "{:indent$}{}{}",
                    "",
                    style.paint("2", prefix),
                    style.paint("1;33", RETROSPECTIVE)
                )?;
            } else {
                writeln!(out, "{:indent$}{}", "", style.paint("2", line))?;
            }
        }
        Ok(())
    }

    fn print_hint(&self, out: &mut impl Write, label: &str, command: &str) -> std::io::Result<()> {
        if label.width() + 2 + command.width() <= self.width {
            writeln!(out, "{label}: {command}")
        } else {
            writeln!(out, "{label}:\n{command}")
        }
    }
}

fn wrap_timeline_text(text: &str, width: usize) -> Vec<String> {
    let text = crate::terminal_text(text).replace('\t', "    ");
    let mut lines = Vec::new();
    for mut rest in text.split(['\n', '\u{2028}', '\u{2029}']) {
        if rest.is_empty() {
            lines.push(String::new());
        }
        while !rest.is_empty() {
            let mut end = rest.len();
            let mut space = None;
            for (offset, ch) in rest.char_indices() {
                if ch.is_whitespace() && offset > 0 {
                    space = Some(offset);
                }
                let after = offset + ch.len_utf8();
                if rest[..after].width() > width {
                    end = space
                        .filter(|offset| !rest[..*offset].trim().is_empty())
                        .unwrap_or(if offset == 0 { after } else { offset });
                    break;
                }
            }
            lines.push(rest[..end].trim_end().to_string());
            rest = rest[end..].trim_start();
        }
    }
    lines
}

fn print_timeline(
    out: &mut impl Write,
    mut history: ListedHistory<'_>,
    recent: bool,
    independent: bool,
    style: &Style,
) -> std::io::Result<()> {
    let target = history.pr.unwrap_or(history.repo);
    let status = if history.enabled {
        "enabled"
    } else {
        "disabled"
    };
    writeln!(out, "{target} | Recording {status}")?;
    if history.entries.is_empty() {
        return if history.pr.is_some() {
            writeln!(out, "No decisions match this PR.")
        } else {
            writeln!(out, "No decisions recorded for this repository.")
        };
    }
    if independent {
        history
            .entries
            .retain(|record| matches!(record.entry.source, Source::Independent));
        if history.entries.is_empty() {
            return writeln!(out, "No independent decisions match this view.");
        }
    }
    history
        .entries
        .sort_by_key(|record| record.entry.recorded_at);
    history.entries.reverse();
    let count = history.entries.len();
    let shown = if recent {
        count.min(RECENT_LIMIT)
    } else {
        count
    };
    let label = if independent {
        "independent decisions"
    } else {
        "decisions"
    };
    if shown < count {
        writeln!(out, "Showing the newest {shown} of {count} {label}.")?;
    }
    let entries = &history.entries[..shown];
    let refs: Vec<_> = entries
        .iter()
        .map(|record| ref_label(record, target))
        .collect();
    let layout = TimelineLayout::new(style, &refs);
    let mut day = None;
    for (record, reference) in entries.iter().zip(&refs) {
        writeln!(out)?;
        let recorded_day = record.entry.recorded_at.date_naive();
        if day != Some(recorded_day) {
            day = Some(recorded_day);
            writeln!(out, "{}", style.paint("1", &recorded_day.to_string()))?;
            layout.print_header(out, style)?;
        }
        layout.print_row(out, style, record, reference, history.pr)?;
    }
    if let Some(example) = refs.iter().find(|reference| show_ref(reference).is_ok()) {
        writeln!(out)?;
        layout.print_hint(
            out,
            "Details",
            &format!("provenance decisions show '{example}'"),
        )?;
        layout.print_hint(
            out,
            "From anywhere",
            &format!("provenance decisions show {} '{example}'", history.repo),
        )?;
    }
    if shown < count {
        let filter = if independent { " --independent" } else { "" };
        layout.print_hint(
            out,
            "Full history",
            &format!("provenance decisions list {target}{filter}"),
        )?;
    }
    Ok(())
}

fn stdout_result(result: std::io::Result<()>) -> Result<()> {
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

fn linked_pr(repo: &str, pr: &str) -> Result<String> {
    let (pr_repo, key) = pr_key(pr, Some(repo))?;
    if pr_repo != repo {
        bail!("the PR must belong to repository {repo}");
    }
    Ok(key)
}

fn commit_hash(commit: &str) -> Result<String> {
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("expected a full 40- or 64-character hexadecimal Git commit hash");
    }
    Ok(commit.to_ascii_lowercase())
}

fn check_link(label: &str, current: &Option<String>, requested: &Option<String>) -> Result<()> {
    if let (Some(current), Some(requested)) = (current, requested)
        && current != requested
    {
        bail!("this decision already links to {label} {current}");
    }
    Ok(())
}

fn nonempty(label: &str, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        bail!("{label} must not be blank");
    }
    Ok(())
}

pub fn run(home: &Path, action: Option<Command>, independent: bool) -> Result<()> {
    let store = Store::new(home);
    let Some(action) = action else {
        let repo = local_repository()?;
        let registry = store.read()?;
        return stdout_result(print_timeline(
            &mut std::io::stdout().lock(),
            registry.listed_history(&repo, None),
            true,
            independent,
            &Style::detect(false),
        ));
    };
    if matches!(action, Command::Guidance) {
        return guidance(&store);
    }
    if let Command::Show { target, reference } = &action {
        let (repo, pr, id) = show_target(target, reference.as_deref())?;
        let registry = store.read()?;
        let target = pr.as_deref().unwrap_or(&repo);
        let decision = registry.decision(&repo, pr.as_deref(), id)
            .with_context(|| format!("decision {id} was not recorded under {target}; use its original target from the list"))?;
        return stdout_result(print_detail(
            &mut std::io::stdout().lock(),
            &decision,
            target,
        ));
    }
    let (repo, pr) = match &action {
        Command::Enable { repo } | Command::Disable { repo } | Command::Status { repo, .. } => {
            (repository(repo.as_deref())?, None)
        }
        Command::List { target: None, .. } => (local_repository()?, None),
        Command::List {
            target: Some(target),
            ..
        }
        | Command::Record { target, .. } => decision_target(target)?,
        Command::Link { repo, .. } => (repository(Some(repo))?, None),
        Command::Guidance | Command::Show { .. } => unreachable!(),
    };
    match action {
        Command::Status { json, .. } => {
            let registry = store.read()?;
            let enabled = registry.enabled(&repo);
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
        Command::List {
            json, independent, ..
        } => {
            let registry = store.read()?;
            let mut history = registry.listed_history(&repo, pr.as_deref());
            let mut out = std::io::stdout().lock();
            if json {
                if independent {
                    history
                        .entries
                        .retain(|record| matches!(record.entry.source, Source::Independent));
                }
                stdout_result(writeln!(out, "{}", serde_json::to_string_pretty(&history)?))?;
            } else {
                stdout_result(print_timeline(
                    &mut out,
                    history,
                    false,
                    independent,
                    &Style::detect(false),
                ))?;
            }
        }
        action => {
            let _lock = store.lock()?;
            let mut registry = store.read()?;
            match action {
                Command::Enable { .. } | Command::Disable { .. } => {
                    let enabled = matches!(action, Command::Enable { .. });
                    if enabled {
                        let [codex, claude] = crate::decision_guidance::install(home).context(
                            "decision guidance setup failed; repository recording status is unchanged",
                        )?;
                        println!("Codex decision guidance: {}", codex.display());
                        println!("Claude Code decision guidance: {}", claude.display());
                    }
                    if registry.repos.get(&repo) != Some(&enabled) {
                        registry.repos.insert(repo.clone(), enabled);
                        store.write(&registry)?;
                    }
                    println!(
                        "{repo}: recording {} for this repository and its PRs",
                        if enabled { "enabled" } else { "disabled" }
                    );
                }
                Command::Record {
                    actor,
                    title,
                    what,
                    why,
                    alternatives,
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
                    registry.require_enabled(&repo)?;
                    let mut values = alternatives.into_iter();
                    let mut alternatives = Vec::new();
                    while let Some(option) = values.next() {
                        let reason_not_selected = values
                            .next()
                            .context("expected a reason for each alternative")?;
                        alternatives.push(Alternative {
                            option,
                            reason_not_selected,
                        });
                    }
                    let entry = |id| Entry {
                        id,
                        recorded_at: Utc::now(),
                        actor,
                        title: Some(title),
                        what,
                        why,
                        alternatives,
                        evidence,
                        source,
                        timing,
                    };
                    let (target, id) = if let Some(pr) = pr {
                        let entries = &mut registry.prs.entry(pr.clone()).or_default().entries;
                        let id = entries.len() + 1;
                        entries.push(entry(id));
                        (pr, id)
                    } else {
                        let entries = registry.repository_entries.entry(repo.clone()).or_default();
                        let id = entries.len() + 1;
                        entries.push(RepositoryEntry {
                            entry: entry(id),
                            pr: None,
                            commit: None,
                        });
                        (repo, id)
                    };
                    store.write(&registry)?;
                    println!("{target}: recorded decision {id}");
                }
                Command::Link { id, pr, commit, .. } => {
                    registry.require_enabled(&repo)?;
                    let pr = pr
                        .as_deref()
                        .map(|value| linked_pr(&repo, value))
                        .transpose()?;
                    let commit = commit.as_deref().map(commit_hash).transpose()?;
                    let record = registry
                        .repository_entries
                        .get_mut(&repo)
                        .and_then(|entries| entries.iter_mut().find(|record| record.entry.id == id))
                        .with_context(|| format!("repository decision {id} not found in {repo}"))?;
                    check_link("PR", &record.pr, &pr)?;
                    check_link("commit", &record.commit, &commit)?;
                    if (pr.is_some() && record.pr.is_none())
                        || (commit.is_some() && record.commit.is_none())
                    {
                        record.pr = pr.or(record.pr.take());
                        record.commit = commit.or(record.commit.take());
                        store.write(&registry)?;
                    }
                    println!("{repo}: links saved for decision {id}");
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

mod github;
mod install;
mod live;
mod matching;
mod sessions;
mod stamps;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};

use matching::{MatchKind, PrRow, SessionMatch};
use sessions::Harness;

/// Find the coding-agent session behind each pull request.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List open PRs with the last known session that worked on each.
    Prs {
        /// Only this repository (owner/name).
        #[arg(long)]
        repo: Option<String>,
        /// GitHub author to search for.
        #[arg(long, default_value = "@me")]
        author: String,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Install the commit-stamp git hook and the Claude Code and Codex session hooks.
    Install {
        /// Show what would change without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show this machine's name in stamps, or set an alias for it.
    ///
    /// The default is a random ID, never the host name.
    Machine {
        /// New name: letters, digits, '.', '-', '_'.
        alias: Option<String>,
    },
    /// Remove everything `install` added.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
    /// (Run by the git hook.) Stamp a commit message file with the agent session.
    #[command(hide = true)]
    Stamp {
        msg_file: String,
        /// Extra arguments git passes to prepare-commit-msg; ignored.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _rest: Vec<String>,
    },
    /// (Run by harness hooks.) Record a session event read from stdin.
    #[command(hide = true)]
    Hook { harness: HarnessArg },
}

#[derive(Clone, Copy, ValueEnum)]
enum HarnessArg {
    ClaudeCode,
    Codex,
}

fn main() -> Result<()> {
    let home = || -> Result<PathBuf> {
        Ok(PathBuf::from(
            std::env::var_os("HOME").context("HOME is not set")?,
        ))
    };
    match Cli::parse().command {
        Cmd::Prs { repo, author, json } => prs(&home()?, repo, &author, json),
        Cmd::Install { dry_run } => {
            let bin = std::env::current_exe().context("finding the provenance binary")?;
            report(install::install(&home()?, &bin, dry_run)?, dry_run);
            if !dry_run && let Some(m) = stamps::machine_name() {
                println!(
                    "machine name in stamps: {m} (random, not your host name; set an alias with `provenance machine <alias>`)"
                );
            }
            if !dry_run {
                println!(
                    "\nCodex asks you to review new hooks before running them: approve the provenance hooks when it asks."
                );
            }
            Ok(())
        }
        Cmd::Machine { alias } => {
            if let Some(a) = alias {
                stamps::set_machine(&a)?;
            }
            match stamps::machine_name() {
                Some(m) => println!("{m}"),
                None => anyhow::bail!("couldn't read or create a machine name"),
            }
            Ok(())
        }
        Cmd::Uninstall { dry_run } => {
            report(install::uninstall(&home()?, dry_run)?, dry_run);
            Ok(())
        }
        Cmd::Stamp { msg_file, .. } => stamps::stamp_message_file(&msg_file),
        Cmd::Hook { harness } => {
            let h = match harness {
                HarnessArg::ClaudeCode => Harness::ClaudeCode,
                HarnessArg::Codex => Harness::Codex,
            };
            if let Ok(home) = home() {
                live::run_hook(h, &home);
            }
            Ok(())
        }
    }
}

fn report(lines: Vec<String>, dry_run: bool) {
    for l in lines {
        println!("{}{l}", if dry_run { "would change · " } else { "" });
    }
}

fn prs(home: &std::path::Path, repo: Option<String>, author: &str, json: bool) -> Result<()> {
    let mut search = format!("author:{author}");
    if let Some(r) = &repo {
        search.push_str(&format!(" repo:{r}"));
    }
    let (prs, (mut sightings, records)) = rayon::join(
        || github::open_prs(&search),
        || (sessions::load_all(home), live::load_records(home)),
    );
    sightings.extend(live::sightings(&records));
    let mut rows = matching::match_prs(prs?, &sightings);

    let alive = live::alive_pids(&records);
    let by_id: HashMap<(Harness, &str), &live::Record> = records
        .iter()
        .map(|r| ((r.harness, r.session_id.as_str()), r))
        .collect();
    for m in rows.iter_mut().flat_map(|r| r.sessions.iter_mut()) {
        if let Some(rec) = by_id.get(&(m.sighting.harness, m.sighting.session_id.as_str())) {
            m.state = Some(live::state(rec, live::is_alive(rec, &alive)));
        }
    }

    rows.sort_by(|a, b| {
        let last = |r: &PrRow| r.sessions.first().map(|m| m.sighting.last_seen);
        last(b).cmp(&last(a))
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print_table(&rows, Utc::now());
    }
    Ok(())
}

fn session_label(m: &SessionMatch) -> String {
    let s = &m.sighting;
    let id: String = s
        .session_id
        .trim_start_matches("session_")
        .chars()
        .take(8)
        .collect();
    let place = match (&s.app[..], &s.machine) {
        ("", Some(machine)) => format!("on {machine}"),
        ("", None) => "?".into(),
        (app, _) => app.into(),
    };
    format!("{} · {place} · {id}", s.harness.label())
}

fn print_table(rows: &[PrRow], now: DateTime<Utc>) {
    let table: Vec<[String; 7]> = rows
        .iter()
        .map(|r| {
            let [session, kind, state, active] = match r.sessions.first() {
                Some(m) => [
                    session_label(m),
                    match m.kind {
                        MatchKind::Stamp => "stamp",
                        MatchKind::Repo => "repo",
                        MatchKind::Path => "path",
                    }
                    .to_string(),
                    m.state.map(|s| s.label().to_string()).unwrap_or_default(),
                    ago(m.sighting.last_seen, now),
                ],
                None => ["— no session found".into(), "".into(), "".into(), "".into()],
            };
            [
                truncate(&r.pr.repo, 28),
                format!("#{}", r.pr.number),
                truncate(&r.pr.title, 40),
                session,
                kind,
                state,
                active,
            ]
        })
        .collect();

    let header = [
        "REPO",
        "PR",
        "TITLE",
        "LAST SESSION",
        "MATCH",
        "STATE",
        "ACTIVE",
    ]
    .map(String::from);
    let mut widths = header.clone().map(|h| h.chars().count());
    for row in &table {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&table) {
        let line: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }

    let matched: Vec<_> = rows.iter().filter_map(|r| r.sessions.first()).collect();
    let count = |k: MatchKind| matched.iter().filter(|m| m.kind == k).count();
    println!(
        "\n{} open PRs · {} with a session ({} by stamp, {} by repo, {} by folder path) · {} no session found",
        rows.len(),
        matched.len(),
        count(MatchKind::Stamp),
        count(MatchKind::Repo),
        count(MatchKind::Path),
        rows.len() - matched.len()
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

fn ago(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - t).num_seconds().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn formats_relative_time_and_truncates_by_char() {
        let now = Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap();
        assert_eq!(ago(now - chrono::Duration::minutes(3), now), "3m");
        assert_eq!(ago(now - chrono::Duration::days(2), now), "2d");
        assert_eq!(truncate("héllo world", 6), "héllo…");
        assert_eq!(truncate("short", 6), "short");
    }
}

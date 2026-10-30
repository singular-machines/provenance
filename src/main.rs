mod github;
mod matching;
mod sessions;

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};

use matching::{MatchKind, PrRow};

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
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Cmd::Prs { repo, author, json } => prs(repo, &author, json),
    }
}

fn prs(repo: Option<String>, author: &str, json: bool) -> Result<()> {
    let mut search = format!("author:{author}");
    if let Some(r) = &repo {
        search.push_str(&format!(" repo:{r}"));
    }
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let (prs, sightings) = rayon::join(|| github::open_prs(&search), || sessions::load_all(&home));
    let mut rows = matching::match_prs(prs?, &sightings);
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

fn print_table(rows: &[PrRow], now: DateTime<Utc>) {
    let table: Vec<[String; 6]> = rows
        .iter()
        .map(|r| {
            let (session, kind, active) = match r.sessions.first() {
                Some(m) => {
                    let s = &m.sighting;
                    let id: String = s.session_id.chars().take(8).collect();
                    (
                        format!("{} · {} · {id}", s.harness.label(), s.app),
                        match m.kind {
                            MatchKind::Repo => "repo",
                            MatchKind::Path => "path",
                        }
                        .to_string(),
                        ago(s.last_seen, now),
                    )
                }
                None => ("— no session found".into(), "".into(), "".into()),
            };
            [
                truncate(&r.pr.repo, 28),
                format!("#{}", r.pr.number),
                truncate(&r.pr.title, 40),
                session,
                kind,
                active,
            ]
        })
        .collect();

    let header = ["REPO", "PR", "TITLE", "LAST SESSION", "MATCH", "ACTIVE"].map(String::from);
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
    let by_repo = matched.iter().filter(|m| m.kind == MatchKind::Repo).count();
    println!(
        "\n{} open PRs · {} with a session ({} by repo, {} by folder path) · {} no session found",
        rows.len(),
        matched.len(),
        by_repo,
        matched.len() - by_repo,
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

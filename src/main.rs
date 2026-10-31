mod cache;
mod github;
mod install;
mod live;
mod matching;
mod render;
mod sessions;
mod stamps;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};

use matching::PrRow;
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
        /// Search GitHub now instead of using a PR list cached in the last 2 minutes.
        #[arg(long)]
        refresh: bool,
        /// Also show how many sessions were matched by stamp, repo and folder path.
        #[arg(long, short)]
        verbose: bool,
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
    Pi,
}

fn main() -> Result<()> {
    let home = || -> Result<PathBuf> {
        Ok(PathBuf::from(
            std::env::var_os("HOME").context("HOME is not set")?,
        ))
    };
    match Cli::parse().command {
        Cmd::Prs {
            repo,
            author,
            json,
            refresh,
            verbose,
        } => prs(&home()?, repo, &author, json, refresh, verbose),
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
                HarnessArg::Pi => Harness::Pi,
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

fn prs(
    home: &std::path::Path,
    repo: Option<String>,
    author: &str,
    json: bool,
    refresh: bool,
    verbose: bool,
) -> Result<()> {
    let mut search = format!("author:{author}");
    if let Some(r) = &repo {
        search.push_str(&format!(" repo:{r}"));
    }
    let now = Utc::now();
    let cache_dir = cache::dir(home);
    let (fetched, ((mut sightings, records), opencode)) = rayon::join(
        || github::open_prs(&search, &cache_dir, refresh, now),
        || {
            rayon::join(
                || {
                    (
                        sessions::load_all(home, Some(&cache_dir)),
                        live::load_records(home),
                    )
                },
                || sessions::opencode_sessions(&sessions::opencode_db(home)),
            )
        },
    );
    let fetched = fetched?;
    sightings.extend(live::sightings(&records));
    sightings.extend(sessions::opencode_sightings(&opencode));
    sessions::give_folder_branches(&mut sightings, sessions::current_branch);
    let folders = matching::resolve_folders(&fetched.prs, &sightings);
    let mut rows = matching::match_prs_with(fetched.prs, &sightings, &folders);

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
    // OpenCode keeps its own busy and idle times, so it needs no hooks.
    let matched_opencode = rows
        .iter()
        .flat_map(|r| &r.sessions)
        .any(|m| m.sighting.harness == Harness::OpenCode);
    if matched_opencode {
        let running = live::opencode_running();
        let by_id: HashMap<&str, &sessions::OpenCodeSession> =
            opencode.iter().map(|s| (s.id.as_str(), s)).collect();
        for m in rows.iter_mut().flat_map(|r| r.sessions.iter_mut()) {
            if m.sighting.harness == Harness::OpenCode
                && let Some(s) = by_id.get(m.sighting.session_id.as_str())
            {
                m.state = live::opencode_state(s, running);
            }
        }
    }

    rows.sort_by(|a, b| {
        let last = |r: &PrRow| r.sessions.first().map(|m| m.sighting.last_seen);
        last(b).cmp(&last(a))
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        let cached = fetched
            .from_cache
            .then(|| render::ago(fetched.fetched_at, now));
        print!(
            "{}",
            render::render(
                &rows,
                now,
                &render::Style::detect(verbose),
                cached.as_deref()
            )
        );
    }
    Ok(())
}

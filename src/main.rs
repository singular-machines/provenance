mod cache;
mod github;
mod go;
mod install;
mod live;
mod matching;
mod mcp;
mod render;
mod send;
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
    /// Open the session behind a PR: switch to it if it's running, else resume it.
    Go {
        /// PR number (in this folder's repository), owner/name#number or PR URL.
        pr: String,
        /// GitHub author of the PR.
        #[arg(long, default_value = "@me")]
        author: String,
        /// Print what would be run instead of running it.
        #[arg(long)]
        print: bool,
    },
    /// Message the session behind a PR (Claude Code or Codex).
    ///
    /// A running session gets it now, or after its current turn. It answers
    /// with the `reply` tool of `provenance mcp`.
    Send {
        /// PR number (in this folder's repository), owner/name#number or PR URL.
        pr: String,
        /// The message.
        message: String,
        /// GitHub author of the PR.
        #[arg(long, default_value = "@me")]
        author: String,
        /// If the session isn't running, resume it headless to answer.
        #[arg(long)]
        resume: bool,
        /// Wait for its reply, up to this many seconds (default 300).
        #[arg(long, value_name = "SECS", num_args = 0..=1, default_missing_value = "300")]
        wait: Option<u64>,
    },
    /// Run the MCP server that gives agent sessions `send` and `reply` tools.
    Mcp,
    /// Install the commit-stamp git hook, the Claude Code and Codex session hooks, and the MCP server.
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
        Cmd::Go { pr, author, print } => go(&home()?, &pr, &author, print),
        Cmd::Send {
            pr,
            message,
            author,
            resume,
            wait,
        } => send_cmd(&home()?, &pr, &message, &author, resume, wait),
        Cmd::Mcp => mcp::serve(&McpTools { home: home()? }),
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

/// Open PRs matched to sessions, with session states filled in.
struct Loaded {
    fetched: github::Fetched,
    rows: Vec<PrRow>,
    records: Vec<live::Record>,
    alive: HashMap<u32, String>,
    opencode: Vec<sessions::OpenCodeSession>,
}

fn load(
    home: &std::path::Path,
    search: &str,
    refresh: bool,
    now: chrono::DateTime<Utc>,
) -> Result<Loaded> {
    let cache_dir = cache::dir(home);
    let (fetched, ((mut sightings, records), opencode)) = rayon::join(
        || github::open_prs(search, &cache_dir, refresh, now),
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
    let mut fetched = fetched?;
    sightings.extend(live::sightings(&records));
    sightings.extend(sessions::opencode_sightings(&opencode));
    sessions::give_folder_branches(&mut sightings, sessions::current_branch);
    let folders = matching::resolve_folders(&fetched.prs, &sightings);
    let mut rows = matching::match_prs_with(std::mem::take(&mut fetched.prs), &sightings, &folders);

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
    Ok(Loaded {
        fetched,
        rows,
        records,
        alive,
        opencode,
    })
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
    let Loaded {
        fetched, mut rows, ..
    } = load(home, &search, refresh, now)?;

    rows.sort_by(|a, b| {
        let last = |r: &PrRow| r.sessions.first().map(|m| m.sighting.last_seen);
        last(b).cmp(&last(a))
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        // GitHub search finds nothing for a repo that doesn't exist, so check.
        if let Some(r) = &repo
            && github::repo_exists(r) == Some(false)
        {
            anyhow::bail!("no repository {r} on GitHub (or `gh` can't see it)");
        }
        let place = repo.map(|r| format!(" in {r}")).unwrap_or_default();
        println!("No open PRs by {author}{place}.");
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

/// The open PR named by `pr`, with sessions loaded: its repo, number and row.
fn resolve(home: &std::path::Path, pr: &str, author: &str) -> Result<(String, u64, Loaded, usize)> {
    let (repo, number) = go::parse_pr(pr)?;
    let repo = match repo {
        Some(r) => r,
        None => go::current_repo()?,
    };
    let search = format!("author:{author} repo:{repo}");
    let mut loaded = load(home, &search, false, Utc::now())?;
    // A PR opened in the last two minutes may be missing from a cached list.
    if !loaded.rows.iter().any(|r| r.pr.number == number) && loaded.fetched.from_cache {
        loaded = load(home, &search, true, Utc::now())?;
    }
    let Some(row) = loaded.rows.iter().position(|r| r.pr.number == number) else {
        anyhow::bail!("{repo}#{number} isn't an open PR by {author} (try --author)");
    };
    Ok((repo, number, loaded, row))
}

/// What is known about the process holding a matched session.
fn process_of(loaded: &Loaded, m: &matching::SessionMatch) -> go::Process {
    let s = &m.sighting;
    if s.harness == Harness::OpenCode {
        // OpenCode doesn't say which process holds a session, so any running
        // `opencode` might hold one that isn't archived.
        let archived = loaded
            .opencode
            .iter()
            .any(|o| o.id == s.session_id && o.time_archived.is_some());
        return if !archived && live::opencode_running() {
            go::Process::MaybeHeld
        } else {
            go::Process::Gone
        };
    }
    match loaded
        .records
        .iter()
        .find(|r| r.harness == s.harness && r.session_id == s.session_id)
    {
        Some(r) if live::is_alive(r, &loaded.alive) => {
            r.pid.map_or(go::Process::Unknown, go::Process::Live)
        }
        Some(r) if r.pid.is_some() => go::Process::Gone,
        _ => go::Process::Unknown,
    }
}

fn go(home: &std::path::Path, pr: &str, author: &str, print: bool) -> Result<()> {
    let (repo, number, loaded, row) = resolve(home, pr, author)?;
    let Some(m) = render::pick(&loaded.rows[row]) else {
        anyhow::bail!("no session found for {repo}#{number}");
    };
    let s = &m.sighting;
    let process = process_of(&loaded, m);
    let action = go::plan(
        m,
        process,
        stamps::machine_name().as_deref(),
        &go::AppLinks {
            home: home.to_path_buf(),
        },
    );
    if print {
        println!("{}", go::describe(&action));
        return Ok(());
    }
    eprintln!(
        "{repo}#{number} → {} session {} ({}{})",
        render::harness_name(s.harness),
        s.session_id,
        if s.app.is_empty() {
            "unknown app"
        } else {
            &s.app
        },
        m.state
            .map(|st| format!(", {}", render::state_word(st)))
            .unwrap_or_default()
    );
    go::run(action)
}

/// The agent session this process runs under, if its hooks recorded it.
fn current_session(records: &[live::Record]) -> Option<send::Sender> {
    let (harness, pid, started) = live::nearest_harness()?;
    records
        .iter()
        .filter(|r| {
            r.harness == harness && r.pid == Some(pid) && r.pid_started.as_deref() == Some(&started)
        })
        .max_by_key(|r| r.updated)
        .map(|r| send::Sender {
            harness: r.harness,
            session_id: r.session_id.clone(),
        })
}

fn label(sender: Option<&send::Sender>) -> String {
    match sender {
        Some(s) => format!(
            "{} session {}",
            render::harness_name(s.harness),
            s.session_id
        ),
        None => "a terminal (provenance send)".into(),
    }
}

/// A delivered message.
struct Sent {
    id: String,
    to: String,
    how: &'static str,
    mailbox: PathBuf,
    output: String,
    from_session: bool,
}

fn send_message(
    home: &std::path::Path,
    pr: &str,
    text: &str,
    resume: bool,
    author: &str,
) -> Result<Sent> {
    let (repo, number, loaded, row) = resolve(home, pr, author)?;
    let Some(m) = render::pick(&loaded.rows[row]) else {
        anyhow::bail!("no session found for {repo}#{number}");
    };
    let s = &m.sighting;
    let target = send::Sender {
        harness: s.harness,
        session_id: s.session_id.clone(),
    };
    let sender = current_session(&loaded.records);
    if sender.as_ref() == Some(&target) {
        anyhow::bail!("{repo}#{number} belongs to this session");
    }
    let delivery = send::plan(
        m,
        process_of(&loaded, m),
        resume,
        stamps::machine_name().as_deref(),
        &|pid, id| send::claude_socket(home, pid, id),
    );
    if let send::Delivery::Refuse(why) = &delivery {
        anyhow::bail!("can't message the session behind {repo}#{number}: {why}");
    }
    let id = send::new_id();
    let pr = format!("{repo}#{number}");
    let from = label(sender.as_ref());
    // Recorded before delivery, so a quick reply finds its message.
    let mailbox = send::mailbox(&send::mail_dir(home), s.harness, &s.session_id);
    send::append(
        &mailbox,
        &send::Mail::Message {
            id: id.clone(),
            pr: pr.clone(),
            from: from.clone(),
            sender: sender.clone(),
            text: text.to_string(),
            at: Utc::now(),
        },
    )?;
    let output = send::deliver(&delivery, &send::framed(&id, &pr, &from, text))?;
    Ok(Sent {
        id,
        to: format!("{pr}'s {}", label(Some(&target))),
        how: send::describe(&delivery),
        mailbox,
        output,
        from_session: sender.is_some(),
    })
}

fn send_cmd(
    home: &std::path::Path,
    pr: &str,
    text: &str,
    author: &str,
    resume: bool,
    wait: Option<u64>,
) -> Result<()> {
    let sent = send_message(home, pr, text, resume, author)?;
    println!("Sent {} to {} via {}.", sent.id, sent.to, sent.how);
    if !sent.output.is_empty() {
        println!("{}", sent.output);
    }
    let Some(secs) = wait else {
        return Ok(());
    };
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Some(reply) = send::reply_to(&sent.mailbox, &sent.id) {
            println!("Reply:\n{reply}");
            return Ok(());
        }
        if std::time::Instant::now() >= end {
            anyhow::bail!("no reply to {} after {secs}s", sent.id);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Record an answer to a message, and pass it to the session that sent it.
fn reply_message(home: &std::path::Path, id: &str, text: &str) -> Result<String> {
    let dir = send::mail_dir(home);
    let Some((mailbox, send::Mail::Message { sender, .. })) = send::find_message(&dir, id) else {
        anyhow::bail!("no message {id}");
    };
    send::append(
        &mailbox,
        &send::Mail::Reply {
            id: id.to_string(),
            text: text.to_string(),
            at: Utc::now(),
        },
    )?;
    let Some(to) = sender else {
        return Ok(format!(
            "Reply to {id} saved for the terminal that sent it."
        ));
    };
    let records = live::load_records(home);
    let alive = live::alive_pids(&records);
    let route = records
        .iter()
        .find(|r| r.harness == to.harness && r.session_id == to.session_id)
        .filter(|r| live::is_alive(r, &alive) && live::state(r, true) != live::State::Ended)
        .and_then(|r| match to.harness {
            Harness::ClaudeCode => {
                send::claude_socket(home, r.pid?, &to.session_id).map(send::Delivery::ClaudeSocket)
            }
            Harness::Codex => Some(send::Delivery::CodexQueue(to.session_id.clone())),
            _ => None,
        });
    let Some(route) = route else {
        return Ok(format!(
            "Reply to {id} saved, but {} isn't running to receive it.",
            label(Some(&to))
        ));
    };
    let from = label(current_session(&records).as_ref());
    send::deliver(&route, &send::framed_reply(id, &from, text))?;
    Ok(format!("Reply to {id} sent to {}.", label(Some(&to))))
}

/// The MCP tools, run against this machine.
struct McpTools {
    home: PathBuf,
}

impl mcp::Tools for McpTools {
    fn send(&self, pr: &str, text: &str, resume: bool, author: &str) -> Result<String> {
        let sent = send_message(&self.home, pr, text, resume, author)?;
        let mut out = format!("Sent {} to {} via {}.", sent.id, sent.to, sent.how);
        out.push_str(if sent.from_session {
            " Its answer will arrive in this session as a \"[provenance] Reply\" message."
        } else {
            " This session isn't recorded by provenance's hooks, so its answer can't be passed back here."
        });
        if !sent.output.is_empty() {
            out.push_str(&format!(
                "\n\nOutput of the headless turn:\n{}",
                sent.output
            ));
        }
        Ok(out)
    }

    fn reply(&self, message_id: &str, text: &str) -> Result<String> {
        reply_message(&self.home, message_id, text)
    }
}

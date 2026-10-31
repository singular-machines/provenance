//! The `provenance prs` table: one group per repository, live sessions first.
//! Widths are measured on plain text and styling is added afterwards, so
//! colour and links never break the alignment.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use unicode_width::UnicodeWidthStr;

use crate::live::State;
use crate::matching::{MatchKind, PrRow, SessionMatch};
use crate::sessions::Harness;

pub struct Style {
    pub color: bool,
    /// OSC 8 hyperlinks: PR numbers open the PR, web sessions open the session.
    pub links: bool,
    /// Terminal width; `None` when not printing to a terminal.
    pub width: Option<usize>,
    pub verbose: bool,
}

impl Style {
    /// Colour and links only on a terminal; `NO_COLOR` and `TERM=dumb` turn colour off.
    pub fn detect(verbose: bool) -> Style {
        use std::io::IsTerminal;
        let tty = std::io::stdout().is_terminal();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        Style {
            color: tty && !dumb && !no_color,
            links: tty && !dumb,
            width: tty
                .then(|| terminal_size::terminal_size().map(|(w, _)| usize::from(w.0)))
                .flatten(),
            verbose,
        }
    }

    fn paint(&self, codes: &str, text: &str) -> String {
        if self.color && !codes.is_empty() && !text.is_empty() {
            format!("\x1b[{codes}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn link(&self, url: &str, text: &str) -> String {
        if self.links && !url.is_empty() {
            format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
        } else {
            text.to_string()
        }
    }
}

const BOLD: &str = "1";
const DIM: &str = "2";
const GREEN: &str = "32";
const YELLOW_BOLD: &str = "1;33";
const RED: &str = "31";

/// Widest a title gets when the terminal width is unknown.
const TITLE_MAX: usize = 60;
const TITLE_MIN: usize = 20;

fn glyph(s: Option<State>) -> &'static str {
    match s {
        Some(State::Running) => "●",
        Some(State::Waiting) => "◐",
        Some(State::Stopped) => "✕",
        Some(State::Ended) => "○",
        None => " ",
    }
}

fn glyph_color(s: Option<State>) -> &'static str {
    match s {
        Some(State::Running) => GREEN,
        Some(State::Waiting) => YELLOW_BOLD,
        Some(State::Stopped) => RED,
        _ => DIM,
    }
}

fn is_live(s: Option<State>) -> bool {
    matches!(s, Some(State::Running | State::Waiting))
}

/// Running, then waiting, then everything else.
fn rank(s: Option<State>) -> u8 {
    match s {
        Some(State::Running) => 0,
        Some(State::Waiting) => 1,
        _ => 2,
    }
}

/// The session a row shows: a live one if any (running, then waiting), else
/// the most recent. `sessions` is newest first, which `--json` keeps.
pub fn pick(r: &PrRow) -> Option<&SessionMatch> {
    r.sessions
        .iter()
        .min_by_key(|m| (rank(m.state), std::cmp::Reverse(m.sighting.last_seen)))
}

fn state(r: &PrRow) -> Option<State> {
    pick(r).and_then(|m| m.state)
}

fn last_seen(r: &PrRow) -> Option<DateTime<Utc>> {
    pick(r).map(|m| m.sighting.last_seen)
}

/// Plain-text session cells: where to find it, which harness and the short ID.
struct Session<'a> {
    place: String,
    harness: &'static str,
    id: String,
    guessed: bool,
    url: Option<&'a str>,
}

fn session_cells(m: &SessionMatch) -> Session<'_> {
    let s = &m.sighting;
    Session {
        place: match (&s.app[..], &s.machine) {
            ("", Some(machine)) => format!("on {machine}"),
            ("", None) => "?".into(),
            (app, _) => app.into(),
        },
        harness: harness_name(s.harness),
        id: s
            .session_id
            .trim_start_matches("session_")
            .trim_start_matches("ses_")
            .chars()
            .take(8)
            .collect(),
        guessed: m.kind != MatchKind::Stamp,
        url: s.url.as_deref(),
    }
}

pub fn render(rows: &[PrRow], now: DateTime<Utc>, style: &Style, cached: Option<&str>) -> String {
    let mut groups: Vec<(&str, Vec<&PrRow>)> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for r in rows {
        let i = *index.entry(&r.pr.repo).or_insert_with(|| {
            groups.push((&r.pr.repo, Vec::new()));
            groups.len() - 1
        });
        groups[i].1.push(r);
    }
    for (_, g) in &mut groups {
        g.sort_by(|a, b| {
            (rank(state(a)), std::cmp::Reverse(last_seen(a)))
                .cmp(&(rank(state(b)), std::cmp::Reverse(last_seen(b))))
        });
    }
    // Repositories with live work first, then by most recent activity.
    groups.sort_by_key(|(_, g)| {
        (
            g.iter().map(|r| rank(state(r))).min(),
            std::cmp::Reverse(g.iter().filter_map(|r| last_seen(r)).max()),
        )
    });

    let sessions: Vec<Option<Session>> = rows.iter().map(|r| pick(r).map(session_cells)).collect();

    let max = |f: &dyn Fn(&Session) -> usize| sessions.iter().flatten().map(f).max().unwrap_or(0);
    let place_w = max(&|s| width(&s.place));
    let harness_w = max(&|s| s.harness.len());
    let id_w = max(&|s| width(&s.id));
    let num_w = rows
        .iter()
        .map(|r| r.pr.number.to_string().len() + 1)
        .max()
        .unwrap_or(0);
    let ago_w = rows
        .iter()
        .filter_map(|r| last_seen(r).map(|t| ago(t, now).len()))
        .max()
        .unwrap_or(0);
    let longest_title = rows.iter().map(|r| width(&r.pr.title)).max().unwrap_or(0);
    // "  ● #927  " + title + "  " + place + "  " + harness + " " + id + " ~" + " " + ago
    let session_w = if sessions.iter().any(Option::is_some) {
        place_w + 2 + harness_w + 1 + id_w + 2
    } else {
        width("— no session found")
    };
    let fixed = 2 + 1 + 1 + num_w + 2 + 2 + session_w + 1 + ago_w;
    let title_w = match style.width {
        Some(w) => w.saturating_sub(fixed).max(TITLE_MIN),
        None => TITLE_MAX,
    }
    .min(longest_title);
    let line_w = style.width.unwrap_or(fixed + title_w).min(fixed + title_w);

    let mut out = String::new();
    for (gi, (repo, group)) in groups.iter().enumerate() {
        if gi > 0 {
            out.push('\n');
        }
        let running = group
            .iter()
            .filter(|r| state(r) == Some(State::Running))
            .count();
        let waiting = group
            .iter()
            .filter(|r| state(r) == Some(State::Waiting))
            .count();
        let mut counts = vec![format!("{} open", group.len())];
        if running > 0 {
            counts.push(format!("{running} running"));
        }
        if waiting > 0 {
            counts.push(format!("{waiting} waiting"));
        }
        let counts = counts.join(" · ");
        let gap = line_w.saturating_sub(width(repo) + width(&counts)).max(2);
        out.push_str(&format!(
            "{}{}{}\n",
            style.paint(BOLD, repo),
            " ".repeat(gap),
            style.paint(DIM, &counts)
        ));

        for r in group {
            let st = state(r);
            let live = is_live(st);
            let num = format!("#{}", r.pr.number);
            let num = format!(
                "{}{}",
                style.link(&r.pr.url, &style.paint(if live { BOLD } else { "" }, &num)),
                " ".repeat(num_w - num.len())
            );
            let title = pad(&truncate(&r.pr.title, title_w), title_w);
            let title = match st {
                Some(State::Waiting) => style.paint(BOLD, &title),
                _ if live => title,
                _ => style.paint(DIM, &title),
            };
            let (session, when) = match pick(r).map(session_cells) {
                Some(s) => {
                    let id = style.paint(DIM, &pad(&s.id, id_w));
                    let id = match s.url {
                        Some(u) => style.link(u, &id),
                        None => id,
                    };
                    let cells = format!(
                        "{}  {} {} {}",
                        style.paint(if live { "" } else { DIM }, &pad(&s.place, place_w)),
                        style.paint(if live { "" } else { DIM }, &pad(s.harness, harness_w)),
                        id,
                        style.paint(DIM, if s.guessed { "~" } else { " " }),
                    );
                    let when = format!(
                        "{:>ago_w$}",
                        last_seen(r).map(|t| ago(t, now)).unwrap_or_default()
                    );
                    let when = match st {
                        Some(State::Running) => style.paint(GREEN, &when),
                        Some(State::Waiting) => style.paint(YELLOW_BOLD, &when),
                        _ => style.paint(DIM, &when),
                    };
                    (cells, when)
                }
                None => (
                    style.paint(DIM, &pad("— no session found", session_w)),
                    String::new(),
                ),
            };
            let line = format!(
                "  {} {}  {}  {} {}",
                style.paint(glyph_color(st), glyph(st)),
                num,
                title,
                session,
                when
            );
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }

    out.push('\n');
    out.push_str(&footer(rows, groups.len(), style, cached));
    out
}

pub fn harness_name(h: Harness) -> &'static str {
    match h {
        Harness::ClaudeCode => "Claude",
        Harness::Codex => "Codex",
        Harness::OpenCode => "OpenCode",
        Harness::Pi => "Pi",
    }
}

pub fn state_word(s: State) -> &'static str {
    match s {
        State::Running => "running",
        State::Waiting => "waiting for you",
        State::Stopped => "stopped mid-turn",
        State::Ended => "ended",
    }
}

fn footer(rows: &[PrRow], repos: usize, style: &Style, cached: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (s, text) in [
        (State::Running, "running"),
        (State::Waiting, "waiting for you"),
        (State::Stopped, "stopped mid-turn"),
        (State::Ended, "ended"),
    ] {
        if rows.iter().any(|r| state(r) == Some(s)) {
            parts.push(format!(
                "{} {text}",
                style.paint(glyph_color(Some(s)), glyph(Some(s)))
            ));
        }
    }
    let matched: Vec<&SessionMatch> = rows.iter().filter_map(pick).collect();
    if matched.iter().any(|m| m.kind != MatchKind::Stamp) {
        parts.push("~ matched by branch, not stamped".into());
    }
    let mut legend = parts.join("  ");
    if repos > 1 {
        let none = rows.len() - matched.len();
        let total = format!(
            "{} open PRs in {repos} repos{}",
            rows.len(),
            if none > 0 {
                format!(" · {none} with no session found")
            } else {
                String::new()
            }
        );
        legend = if legend.is_empty() {
            total
        } else {
            format!("{total}  ·  {legend}")
        };
    }
    if let Some(age) = cached {
        let note = format!("cached {age} ago (--refresh)");
        legend = if legend.is_empty() {
            note
        } else {
            format!("{legend}  ·  {note}")
        };
    }
    let mut out = String::new();
    if !legend.is_empty() {
        out.push_str(&style.paint(DIM, &legend));
        out.push('\n');
    }
    if style.verbose {
        let count = |k: MatchKind| matched.iter().filter(|m| m.kind == k).count();
        out.push_str(&style.paint(
            DIM,
            &format!(
                "{} with a session: {} by stamp, {} by repo, {} by folder path · {} no session found\n",
                matched.len(),
                count(MatchKind::Stamp),
                count(MatchKind::Repo),
                count(MatchKind::Path),
                rows.len() - matched.len()
            ),
        ));
    }
    out
}

/// Terminal cells, not chars: CJK and emoji take two, combining marks none.
fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(width(s))))
}

/// Cut to at most `max` cells, ending in `…` when cut. Each prefix is
/// measured whole, because a sequence such as `©` + U+FE0F is wider than its
/// characters added up.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let budget = max.saturating_sub(1);
    let end = s
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .rev()
        .find(|&end| width(&s[..end]) <= budget)
        .unwrap_or(0);
    format!("{}…", &s[..end])
}

pub fn ago(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
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
    use crate::github::PullRequest;
    use crate::sessions::Sighting;
    use chrono::{Duration, TimeZone};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap()
    }

    /// Harness, app, session ID, match kind, live state, minutes since last seen.
    type Seen<'a> = (Harness, &'a str, &'a str, MatchKind, Option<State>, i64);

    fn row(repo: &str, number: u64, title: &str, m: Option<Seen>) -> PrRow {
        PrRow {
            pr: PullRequest {
                id: format!("PR_{number}"),
                head_sha: "h".into(),
                base_sha: "b".into(),
                repo: repo.into(),
                head_repo: None,
                number,
                title: title.into(),
                url: format!("https://github.com/{repo}/pull/{number}"),
                head_branch: "feat".into(),
                base_branch: "main".into(),
                default_branches: vec![],
                stamps: vec![],
            },
            sessions: m
                .into_iter()
                .map(|(harness, app, id, kind, state, mins)| SessionMatch {
                    kind,
                    state,
                    sighting: Sighting {
                        harness,
                        session_id: id.into(),
                        app: app.into(),
                        cwd: "/w".into(),
                        branch: "feat".into(),
                        repo_url: None,
                        last_seen: now() - Duration::minutes(mins),
                        log_path: None,
                        machine: None,
                        url: None,
                    },
                })
                .collect(),
        }
    }

    fn plain(width: Option<usize>) -> Style {
        Style {
            color: false,
            links: false,
            width,
            verbose: false,
        }
    }

    fn sample() -> Vec<PrRow> {
        use Harness::*;
        use MatchKind::*;
        vec![
            row(
                "acme/api",
                68,
                "Ask before rebuilding the local cache",
                Some((ClaudeCode, "Conductor", "7be310aa-1", Path, None, 3 * 1440)),
            ),
            row(
                "acme/storefront",
                896,
                "release: ledger hotfix",
                Some((ClaudeCode, "T3 Code", "d361dde1-2", Repo, None, 600)),
            ),
            row(
                "acme/storefront",
                927,
                "docs: Release 3.1 (RC pinned before go)",
                Some((
                    ClaudeCode,
                    "T3 Code",
                    "7157cbf5-3",
                    Stamp,
                    Some(State::Running),
                    0,
                )),
            ),
            row(
                "acme/storefront",
                946,
                "docs(releases): add release process explainer",
                Some((
                    Codex,
                    "T3 Code",
                    "01a17e4f-4",
                    Stamp,
                    Some(State::Waiting),
                    6,
                )),
            ),
            row("example/web", 41, "Bump deps", None),
        ]
    }

    #[test]
    fn groups_by_repo_with_live_sessions_first() {
        let got = render(&sample(), now(), &plain(Some(100)), Some("9s"));
        let want = "\
acme/storefront                                            3 open · 1 running · 1 waiting
  ● #927  docs: Release 3.1 (RC pinned before go)        T3 Code    Claude 7157cbf5    0s
  ◐ #946  docs(releases): add release process explainer  T3 Code    Codex  01a17e4f    6m
    #896  release: ledger hotfix                         T3 Code    Claude d361dde1 ~ 10h

acme/api                                                                           1 open
    #68   Ask before rebuilding the local cache          Conductor  Claude 7be310aa ~  3d

example/web                                                                        1 open
    #41   Bump deps                                      — no session found

5 open PRs in 3 repos · 1 with no session found  ·  ● running  ◐ waiting for you  ~ matched by branch, not stamped  ·  cached 9s ago (--refresh)
";
        assert_eq!(got, want, "\n{got}");
    }

    #[test]
    fn titles_shrink_to_fit_the_terminal() {
        let got = render(&sample(), now(), &plain(Some(80)), None);
        assert!(
            got.lines()
                .filter(|l| l.starts_with("  "))
                .all(|l| l.chars().count() <= 80),
            "{got}"
        );
        assert!(
            got.contains("docs(releases): add release process…"),
            "{got}"
        );
    }

    #[test]
    fn colour_and_links_only_when_asked() {
        let rows = sample();
        let plain_out = render(&rows, now(), &plain(None), None);
        assert!(!plain_out.contains('\x1b'));
        let styled = render(
            &rows,
            now(),
            &Style {
                color: true,
                links: true,
                width: None,
                verbose: true,
            },
            None,
        );
        assert!(styled.contains("\x1b]8;;https://github.com/acme/storefront/pull/927\x1b\\"));
        assert!(styled.contains("\x1b[32m●\x1b[0m"));
        assert!(styled.contains("by stamp"));
        // Styling never changes the visible text.
        let strip = |s: &str| {
            let mut out = String::new();
            let mut it = s.chars().peekable();
            while let Some(c) = it.next() {
                if c != '\x1b' {
                    out.push(c);
                } else if it.next() == Some(']') {
                    while let Some(c) = it.next() {
                        if c == '\x1b' {
                            it.next();
                            break;
                        }
                    }
                } else {
                    for c in it.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                }
            }
            out
        };
        let visible = strip(&styled);
        assert_eq!(visible.lines().next(), plain_out.lines().next());
        assert!(
            visible.starts_with(&plain_out[..plain_out.find("\n\n").unwrap()]),
            "{visible}"
        );
    }

    #[test]
    fn formats_relative_time_and_truncates_by_char() {
        assert_eq!(ago(now() - Duration::minutes(3), now()), "3m");
        assert_eq!(ago(now() - Duration::days(2), now()), "2d");
        assert_eq!(truncate("héllo world", 6), "héllo…");
        assert_eq!(truncate("short", 6), "short");
        assert_eq!(truncate("修正修正", 5), "修正…");
        let emoji = "\u{a9}\u{fe0f}".repeat(4);
        assert!(width(&truncate(&emoji, 5)) <= 5);
        assert_eq!(width(&pad("修正", 6)), 6);
    }

    #[test]
    fn wide_titles_fit_the_terminal() {
        for title in ["修正".repeat(40), "\u{a9}\u{fe0f}".repeat(80)] {
            let mut rows = sample();
            rows[2].pr.title = title;
            let got = render(&rows, now(), &plain(Some(80)), None);
            for l in got.lines().filter(|l| l.starts_with("  ")) {
                assert!(width(l) <= 80, "{} cells: {l}", width(l));
            }
        }
    }

    #[test]
    fn a_live_session_wins_over_a_newer_idle_one() {
        let mut rows = sample();
        // #896: an older session waiting for the owner, plus a newer idle one.
        let mut waiting = rows[1].sessions[0].clone();
        waiting.state = Some(State::Waiting);
        waiting.sighting.session_id = "aaaabbbb-old".into();
        waiting.sighting.last_seen = now() - Duration::minutes(700);
        rows[1].sessions[0].sighting.last_seen = now() - Duration::minutes(5);
        rows[1].sessions.push(waiting);
        let got = render(&rows, now(), &plain(Some(100)), None);
        assert!(got.contains("3 open · 1 running · 2 waiting"), "{got}");
        assert!(got.contains("◐ #896"), "{got}");
        assert!(got.contains("aaaabbbb"), "{got}");
    }
}

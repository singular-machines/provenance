//! `provenance log`: what happened to your PRs in a time window, and what
//! needs you now. The terminal shows an attention list; `--html` writes a
//! self-contained timeline page.

use std::collections::HashMap;

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};

use crate::github::PrState;
use crate::live::State;
use crate::matching::{MatchKind, PrRow};
use crate::render::{self, Style};
use crate::sessions::{self, Harness, Sighting};

/// The period `--since` names, from local midnight on its first day to now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub start: DateTime<Local>,
    /// Calendar days covered, today included.
    pub days: u32,
}

impl Window {
    pub fn parse(since: &str, now: DateTime<Local>) -> Result<Window> {
        let today = now.date_naive();
        let days = match since {
            "today" => 1,
            "yesterday" => 2,
            s if s.ends_with('d') => match s[..s.len() - 1].parse::<u32>() {
                Ok(n @ 1..=366) => n,
                _ => bail!("--since {s}: use 1d to 366d"),
            },
            s => {
                let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") else {
                    bail!(
                        "--since {s}: use today, yesterday, a number of days such as 7d, or a date such as 2026-11-05"
                    );
                };
                if date > today {
                    bail!("--since {s} is in the future");
                }
                u32::try_from((today - date).num_days() + 1).unwrap_or(u32::MAX)
            }
        };
        let first = today - Duration::days(i64::from(days) - 1);
        Ok(Window {
            start: local_midnight(first),
            days,
        })
    }

    /// "Today (Wed 11 Nov)" or "Thu 5 – Wed 11 Nov".
    pub fn label(&self, now: DateTime<Local>) -> String {
        if self.days == 1 {
            return format!("Today ({})", now.format("%a %-d %b"));
        }
        let start = if self.start.month() == now.month() && self.start.year() == now.year() {
            self.start.format("%a %-d").to_string()
        } else if self.start.year() == now.year() {
            self.start.format("%a %-d %b").to_string()
        } else {
            self.start.format("%a %-d %b %Y").to_string()
        };
        format!("{start} – {}", now.format("%a %-d %b"))
    }
}

/// Midnight at the start of `day`, or the first hour that exists when a
/// clock change skips midnight.
fn local_midnight(day: NaiveDate) -> DateTime<Local> {
    (0..24)
        .find_map(|h| {
            Local
                .from_local_datetime(&day.and_hms_opt(h, 0, 0)?)
                .earliest()
        })
        .unwrap_or_else(Local::now)
}

/// Where a PR stands for the person reading the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Group {
    /// Open, and its session waits for you or stopped mid-turn.
    NeedsYou,
    /// Open, with recent activity.
    InFlight,
    /// Open, but nothing happened for `--stale-days`: possibly forgotten.
    Quiet,
    /// Merged or closed in the window.
    Done,
}

/// The session behind a PR, as the log shows it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Agent {
    pub harness: Harness,
    /// Harness with model, such as "Claude Code (Opus 5.5)".
    pub name: String,
    /// The model ID from the session's log, when it records one.
    pub model: Option<String>,
    pub app: String,
    pub session_id: String,
    pub state: Option<State>,
    pub last_seen: DateTime<Utc>,
    pub matched_by: MatchKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Entry {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub created_at: DateTime<Utc>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    /// Newest of the PR's last change on GitHub and its session's last activity.
    pub last_activity: DateTime<Utc>,
    pub group: Group,
    /// "waiting for you", "running", "no activity 9d", "merged" and so on.
    pub status: String,
    pub agent: Option<Agent>,
}

impl Entry {
    /// When it was merged or closed, for PRs that are.
    pub fn ended_at(&self) -> Option<DateTime<Utc>> {
        match self.state {
            PrState::Open => None,
            PrState::Merged => self.merged_at.or(self.closed_at),
            PrState::Closed => self.closed_at,
        }
    }

    /// Its newest event: merged or closed, else opened.
    fn event_at(&self) -> DateTime<Utc> {
        self.ended_at().unwrap_or(self.created_at)
    }
}

pub fn harness_long(h: Harness) -> &'static str {
    match h {
        Harness::ClaudeCode => "Claude Code",
        Harness::Codex => "Codex",
        Harness::OpenCode => "OpenCode",
        Harness::Pi => "Pi",
    }
}

/// The log's entries: every open PR, and PRs merged or closed since `start`.
/// `model_of` reads a session's model from its log.
pub fn entries(
    rows: Vec<PrRow>,
    start: DateTime<Utc>,
    now: DateTime<Utc>,
    stale_days: u32,
    model_of: impl Fn(&Sighting) -> Option<String>,
) -> Vec<Entry> {
    let mut out: Vec<Entry> = rows
        .into_iter()
        .filter_map(|r| {
            let agent = render::pick(&r).map(|m| {
                let model = model_of(&m.sighting);
                let harness = harness_long(m.sighting.harness);
                Agent {
                    harness: m.sighting.harness,
                    name: match &model {
                        Some(id) => format!("{harness} ({})", sessions::model_name(id)),
                        None => harness.to_string(),
                    },
                    model,
                    app: m.sighting.app.clone(),
                    session_id: m.sighting.session_id.clone(),
                    state: m.state,
                    last_seen: m.sighting.last_seen,
                    matched_by: m.kind,
                    machine: m.sighting.machine.clone(),
                }
            });
            let pr = r.pr;
            let mut e = Entry {
                last_activity: pr
                    .updated_at
                    .unwrap_or(pr.created_at)
                    .max(agent.as_ref().map_or(pr.created_at, |a| a.last_seen)),
                repo: pr.repo,
                number: pr.number,
                title: pr.title,
                url: pr.url,
                state: pr.state,
                created_at: pr.created_at,
                merged_at: pr.merged_at,
                closed_at: pr.closed_at,
                group: Group::Done,
                status: String::new(),
                agent,
            };
            if e.state != PrState::Open && e.ended_at().is_none_or(|t| t < start) {
                return None;
            }
            (e.group, e.status) = group(&e, now, stale_days);
            Some(e)
        })
        .collect();
    out.sort_by(|a, b| a.group.cmp(&b.group).then(b.event_at().cmp(&a.event_at())));
    out
}

fn group(e: &Entry, now: DateTime<Utc>, stale_days: u32) -> (Group, String) {
    match e.state {
        PrState::Merged => return (Group::Done, "merged".into()),
        PrState::Closed => return (Group::Done, "closed".into()),
        PrState::Open => {}
    }
    let state = e.agent.as_ref().and_then(|a| a.state);
    match state {
        Some(State::Waiting) => {
            return (Group::NeedsYou, render::state_word(State::Waiting).into());
        }
        Some(State::Stopped) => {
            return (Group::NeedsYou, render::state_word(State::Stopped).into());
        }
        Some(State::Running) => {
            return (Group::InFlight, render::state_word(State::Running).into());
        }
        _ => {}
    }
    let idle = now - e.last_activity;
    if idle >= Duration::days(i64::from(stale_days)) {
        return (
            Group::Quiet,
            format!("no activity {}", render::ago(e.last_activity, now)),
        );
    }
    let word = match (state, &e.agent) {
        (Some(s), _) => render::state_word(s),
        (None, Some(_)) => "no live status",
        (None, None) => "open",
    };
    (Group::InFlight, word.into())
}

/// `owner/name#1`, or `name#1` when no other repository in the log has that name.
fn short_refs(entries: &[Entry]) -> HashMap<String, String> {
    let mut by_name: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
    for e in entries {
        let name = e.repo.rsplit('/').next().unwrap_or(&e.repo);
        by_name.entry(name).or_default().insert(&e.repo);
    }
    entries
        .iter()
        .map(|e| {
            let name = e.repo.rsplit('/').next().unwrap_or(&e.repo);
            let shown = if by_name[name].len() == 1 {
                name
            } else {
                &e.repo
            };
            (e.repo.clone(), shown.to_string())
        })
        .collect()
}

/// A time as short as it can be: "14:02" today, "Tue 16:00" this week, else "2 Nov".
pub fn when(t: DateTime<Utc>, now: DateTime<Local>) -> String {
    let t = t.with_timezone(&Local);
    let days = (now.date_naive() - t.date_naive()).num_days();
    if days == 0 {
        t.format("%H:%M").to_string()
    } else if (1..7).contains(&days) {
        t.format("%a %H:%M").to_string()
    } else if t.year() == now.year() {
        t.format("%-d %b").to_string()
    } else {
        t.format("%-d %b %Y").to_string()
    }
}

/// "35m", "4h28m", "2d 3h".
pub fn duration(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let m = (to - from).num_minutes().max(0);
    let (d, h, m) = (m / 1440, m % 1440 / 60, m % 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) if m == 0 => format!("{h}h"),
        (0, _) => format!("{h}h{m:02}m"),
        (_, 0) => format!("{d}d"),
        _ => format!("{d}d {h}h"),
    }
}

const BOLD: &str = "1";
const DIM: &str = "2";
const GREEN: &str = "32";
const RED: &str = "31";
const YELLOW: &str = "33";
const YELLOW_BOLD: &str = "1;33";
const CYAN: &str = "36";

/// Widest a title gets when the terminal width is unknown.
const TITLE_MAX: usize = 60;
const TITLE_MIN: usize = 20;
/// Quiet PRs listed before the rest are summed up, unless `--all`.
const QUIET_SHOWN: usize = 5;

/// The attention list: needs you, in flight, gone quiet, done.
pub fn render_terminal(
    entries: &[Entry],
    window: &Window,
    now: DateTime<Utc>,
    style: &Style,
    cached: Option<&str>,
    all: bool,
) -> String {
    let local = now.with_timezone(&Local);
    let mut out = String::new();
    let count = |f: &dyn Fn(&Entry) -> bool| entries.iter().filter(|e| f(e)).count();
    let merged = count(&|e| e.state == PrState::Merged);
    let closed = count(&|e| e.state == PrState::Closed);
    let open = count(&|e| e.state == PrState::Open);
    let needs = count(&|e| e.group == Group::NeedsYou);
    let quiet = count(&|e| e.group == Group::Quiet);
    let mut summary = vec![format!(
        "{} PR{}",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" }
    )];
    let parts: Vec<String> = [(merged, "merged"), (closed, "closed"), (open, "open")]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, w)| format!("{n} {w}"))
        .collect();
    if !parts.is_empty() {
        summary[0].push_str(&format!(": {}", parts.join(", ")));
    }
    if needs > 0 {
        summary.push(style.paint(
            YELLOW_BOLD,
            &format!("{needs} need{} you", if needs == 1 { "s" } else { "" }),
        ));
    }
    if quiet > 0 {
        summary.push(format!("{quiet} gone quiet"));
    }
    out.push_str(&format!(
        "{} · {}\n",
        style.paint(BOLD, &window.label(local)),
        summary.join(" · ")
    ));
    if entries.is_empty() {
        out.push_str("\nNo open PRs, and none merged or closed in this window.\n");
        return out;
    }

    let refs = short_refs(entries);
    struct Line<'a> {
        e: &'a Entry,
        time: String,
        glyph: (&'static str, &'static str),
        life: String,
        pr: String,
    }
    let hidden = if all {
        0
    } else {
        quiet.saturating_sub(QUIET_SHOWN)
    };
    let mut quiet_seen = 0;
    let lines: Vec<Line> = entries
        .iter()
        .filter(|e| {
            if e.group != Group::Quiet {
                return true;
            }
            quiet_seen += 1;
            all || quiet_seen <= QUIET_SHOWN
        })
        .map(|e| {
            let (glyph, life) = match e.ended_at() {
                Some(end) => (
                    if e.state == PrState::Merged {
                        ("✓", GREEN)
                    } else {
                        ("✗", RED)
                    },
                    format!(
                        "{} → {} ({})",
                        when(e.created_at, local),
                        e.status,
                        duration(e.created_at, end)
                    ),
                ),
                None => (
                    match e.group {
                        Group::NeedsYou => ("●", YELLOW),
                        Group::Quiet => ("○", DIM),
                        _ => ("●", CYAN),
                    },
                    "opened".to_string(),
                ),
            };
            Line {
                e,
                time: when(e.event_at(), local),
                glyph,
                life,
                pr: format!("{}#{}", refs[&e.repo], e.number),
            }
        })
        .collect();
    let time_w = lines
        .iter()
        .map(|l| render::width(&l.time))
        .max()
        .unwrap_or(0);
    let life_w = lines
        .iter()
        .map(|l| render::width(&l.life))
        .max()
        .unwrap_or(0);
    let pr_w = lines
        .iter()
        .map(|l| render::width(&l.pr))
        .max()
        .unwrap_or(0);
    // Indent, time, glyph, lifecycle, PR: each followed by two spaces.
    let used = 2 + time_w + 2 + 1 + 1 + life_w + 2 + pr_w + 2;
    let title_w = style
        .width
        .map_or(TITLE_MAX, |w| w.saturating_sub(used))
        .max(TITLE_MIN);

    let indent = " ".repeat(2 + time_w + 2);
    let mut current = None;
    for l in &lines {
        if current != Some(l.e.group) {
            if current == Some(Group::Quiet) && hidden > 0 {
                out.push_str(&more(&indent, hidden, style));
            }
            current = Some(l.e.group);
            let (name, color) = match l.e.group {
                Group::NeedsYou => ("NEEDS YOU", YELLOW_BOLD),
                Group::InFlight => ("IN FLIGHT", "1;36"),
                Group::Quiet => ("GONE QUIET", BOLD),
                Group::Done => ("DONE", "1;32"),
            };
            out.push('\n');
            out.push_str(&style.paint(color, name));
            out.push('\n');
        }
        out.push_str(&format!(
            "  {}  {} {}  {}  {}\n",
            render::pad(&l.time, time_w),
            style.paint(l.glyph.1, l.glyph.0),
            render::pad(&l.life, life_w),
            style.link(&l.e.url, &render::pad(&l.pr, pr_w)),
            render::truncate(&l.e.title, title_w),
        ));
        let who = match &l.e.agent {
            Some(a) => {
                let place = match (&a.app[..], &a.machine) {
                    ("", Some(m)) => format!("on {m}"),
                    ("", None) => String::new(),
                    (app, _) => app.to_string(),
                };
                [a.name.as_str(), place.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
            None => "no session found (likely you)".into(),
        };
        let status = if l.e.ended_at().is_some() {
            String::new()
        } else {
            let color = match l.e.group {
                Group::NeedsYou => YELLOW,
                _ if l.e.status == "running" => GREEN,
                _ => DIM,
            };
            format!(" · {}", style.paint(color, &l.e.status))
        };
        out.push_str(&format!("{indent}{}{status}\n", style.paint(DIM, &who)));
    }
    if current == Some(Group::Quiet) && hidden > 0 {
        out.push_str(&more(&indent, hidden, style));
    }
    let mut foot = Vec::new();
    if let Some(age) = cached {
        foot.push(format!("cached {age} ago (--refresh)"));
    }
    foot.push("--html for the timeline".into());
    out.push('\n');
    out.push_str(&style.paint(DIM, &foot.join("  ·  ")));
    out.push('\n');
    out
}

fn more(indent: &str, hidden: usize, style: &Style) -> String {
    let line = format!("… and {hidden} more with no recent activity (--all)");
    format!("{indent}{}\n", style.paint(DIM, &line))
}

#[derive(serde::Serialize)]
struct Page<'a> {
    generated_at: DateTime<Utc>,
    start: DateTime<Utc>,
    today: DateTime<Utc>,
    days: u32,
    label: String,
    stale_days: u32,
    entries: &'a [Entry],
}

const TEMPLATE: &str = include_str!("log.html");

/// The timeline page: one self-contained file with the entries inlined.
pub fn render_html(
    entries: &[Entry],
    window: &Window,
    now: DateTime<Utc>,
    stale_days: u32,
) -> Result<String> {
    let local = now.with_timezone(&Local);
    let page = Page {
        generated_at: now,
        start: window.start.with_timezone(&Utc),
        today: local_midnight(local.date_naive()).with_timezone(&Utc),
        days: window.days,
        label: window.label(local),
        stale_days,
        entries,
    };
    // Inside <script>, "</script>" in a title would end the script; escaping
    // every "<" keeps the JSON valid and inert.
    let json = serde_json::to_string(&page)?
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(TEMPLATE.replace("/*DATA*/null", &json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::PullRequest;
    use crate::matching::SessionMatch;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn row(
        repo: &str,
        number: u64,
        state: PrState,
        created: &str,
        ended: Option<&str>,
        session: Option<Option<State>>,
    ) -> PrRow {
        let created_at = at(created);
        let ended = ended.map(at);
        PrRow {
            pr: PullRequest {
                id: format!("PR_{number}"),
                head_sha: "h".into(),
                base_sha: "b".into(),
                repo: repo.into(),
                head_repo: None,
                number,
                state,
                created_at,
                merged_at: ended.filter(|_| state == PrState::Merged),
                closed_at: ended,
                updated_at: Some(ended.unwrap_or(created_at)),
                title: format!("PR {number}"),
                url: format!("https://github.com/{repo}/pull/{number}"),
                head_branch: "f".into(),
                base_branch: "main".into(),
                default_branches: vec!["main".into()],
                stamps: vec![],
            },
            sessions: session
                .map(|state| SessionMatch {
                    kind: MatchKind::Stamp,
                    state,
                    sighting: Sighting {
                        harness: Harness::ClaudeCode,
                        session_id: format!("s{number}"),
                        app: "T3 Code".into(),
                        cwd: "/w".into(),
                        branch: "f".into(),
                        repo_url: None,
                        last_seen: created_at,
                        log_path: None,
                        machine: None,
                        url: None,
                    },
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn groups_open_prs_by_what_they_need_and_drops_old_finished_ones() {
        let now = at("2026-11-11T14:20:00Z");
        let start = at("2026-11-11T00:00:00Z");
        let rows = vec![
            row(
                "o/web",
                1,
                PrState::Open,
                "2026-11-11T14:02:00Z",
                None,
                Some(Some(State::Waiting)),
            ),
            row(
                "o/web",
                2,
                PrState::Open,
                "2026-11-11T12:20:00Z",
                None,
                Some(Some(State::Running)),
            ),
            row(
                "o/web",
                3,
                PrState::Open,
                "2026-11-02T10:00:00Z",
                None,
                Some(Some(State::Ended)),
            ),
            row(
                "o/api",
                4,
                PrState::Merged,
                "2026-11-10T16:00:00Z",
                Some("2026-11-11T13:40:00Z"),
                Some(None),
            ),
            row(
                "o/api",
                5,
                PrState::Closed,
                "2026-11-11T08:45:00Z",
                Some("2026-11-11T10:10:00Z"),
                None,
            ),
            row(
                "o/api",
                6,
                PrState::Merged,
                "2026-11-09T09:00:00Z",
                Some("2026-11-10T12:00:00Z"),
                None,
            ),
            row(
                "o/api",
                7,
                PrState::Open,
                "2026-11-10T09:00:00Z",
                None,
                None,
            ),
        ];
        let got = entries(rows, start, now, 3, |s| {
            (s.session_id == "s1").then(|| "claude-opus-5-5".to_string())
        });
        let summary: Vec<(u64, Group, &str)> = got
            .iter()
            .map(|e| (e.number, e.group, e.status.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (1, Group::NeedsYou, "waiting for you"),
                (2, Group::InFlight, "running"),
                (7, Group::InFlight, "open"),
                (3, Group::Quiet, "no activity 9d"),
                (4, Group::Done, "merged"),
                (5, Group::Done, "closed"),
            ]
        );
        assert_eq!(
            got[0].agent.as_ref().unwrap().name,
            "Claude Code (Opus 5.5)"
        );
        assert_eq!(got[1].agent.as_ref().unwrap().name, "Claude Code");
    }

    #[test]
    fn since_starts_at_local_midnight() {
        let now = Local.with_ymd_and_hms(2026, 11, 11, 14, 20, 0).unwrap();
        let w = Window::parse("today", now).unwrap();
        assert_eq!(
            w.start,
            Local.with_ymd_and_hms(2026, 11, 11, 0, 0, 0).unwrap()
        );
        assert_eq!(w.days, 1);
        assert_eq!(w.label(now), "Today (Wed 11 Nov)");
        let w = Window::parse("7d", now).unwrap();
        assert_eq!(
            w.start,
            Local.with_ymd_and_hms(2026, 11, 5, 0, 0, 0).unwrap()
        );
        assert_eq!(w.label(now), "Thu 5 – Wed 11 Nov");
        assert_eq!(Window::parse("yesterday", now).unwrap().days, 2);
        let w = Window::parse("2026-10-30", now).unwrap();
        assert_eq!(w.days, 13);
        assert_eq!(w.label(now), "Fri 30 Oct – Wed 11 Nov");
        assert!(Window::parse("2026-11-12", now).is_err());
        assert!(Window::parse("0d", now).is_err());
        assert!(Window::parse("week", now).is_err());
    }

    #[test]
    fn durations_and_times_stay_short() {
        let a = at("2026-11-11T09:12:00Z");
        assert_eq!(duration(a, at("2026-11-11T09:47:00Z")), "35m");
        assert_eq!(duration(a, at("2026-11-11T13:40:00Z")), "4h28m");
        assert_eq!(duration(a, at("2026-11-11T11:12:00Z")), "2h");
        assert_eq!(duration(a, at("2026-11-13T12:12:00Z")), "2d 3h");
        let now = Local.with_ymd_and_hms(2026, 11, 11, 14, 20, 0).unwrap();
        let t = |y, mo, d, h| {
            Local
                .with_ymd_and_hms(y, mo, d, h, 5, 0)
                .unwrap()
                .with_timezone(&Utc)
        };
        assert_eq!(when(t(2026, 11, 11, 9), now), "09:05");
        assert_eq!(when(t(2026, 11, 10, 16), now), "Tue 16:05");
        assert_eq!(when(t(2026, 11, 2, 10), now), "2 Nov");
        assert_eq!(when(t(2025, 11, 2, 10), now), "2 Nov 2025");
    }

    #[test]
    fn terminal_list_groups_and_shortens_refs() {
        let now = at("2026-11-11T14:20:00Z");
        let rows = vec![
            row(
                "o/web",
                1,
                PrState::Open,
                "2026-11-11T14:02:00Z",
                None,
                Some(Some(State::Waiting)),
            ),
            row(
                "o/api",
                4,
                PrState::Merged,
                "2026-11-11T09:00:00Z",
                Some("2026-11-11T13:40:00Z"),
                None,
            ),
            row(
                "x/api",
                9,
                PrState::Open,
                "2026-11-11T12:00:00Z",
                None,
                Some(Some(State::Running)),
            ),
        ];
        let got = entries(rows, at("2026-11-11T00:00:00Z"), now, 3, |_| None);
        let style = Style {
            color: false,
            links: false,
            width: Some(120),
            verbose: false,
        };
        let window = Window {
            start: local_midnight(now.with_timezone(&Local).date_naive()),
            days: 1,
        };
        let text = render_terminal(&got, &window, now, &style, Some("1m"), false);
        assert!(
            text.contains("3 PRs: 1 merged, 2 open · 1 needs you"),
            "{text}"
        );
        let order: Vec<usize> = ["NEEDS YOU", "IN FLIGHT", "DONE"]
            .iter()
            .map(|h| text.find(h).unwrap())
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert!(text.contains("web#1"), "{text}");
        assert!(
            text.contains("o/api#4") && text.contains("x/api#9"),
            "{text}"
        );
        assert!(text.contains("no session found (likely you)"), "{text}");
        assert!(
            text.contains("Claude Code · T3 Code · waiting for you"),
            "{text}"
        );
        assert!(text.contains("cached 1m ago"), "{text}");
    }

    #[test]
    fn quiet_prs_are_capped_unless_all() {
        let now = at("2026-11-11T14:20:00Z");
        let rows = (1..=8)
            .map(|n| {
                row(
                    "o/web",
                    n,
                    PrState::Open,
                    "2026-10-01T10:00:00Z",
                    None,
                    None,
                )
            })
            .collect();
        let got = entries(rows, at("2026-11-11T00:00:00Z"), now, 3, |_| None);
        assert!(got.iter().all(|e| e.group == Group::Quiet));
        let style = Style {
            color: false,
            links: false,
            width: Some(120),
            verbose: false,
        };
        let window = Window {
            start: local_midnight(now.with_timezone(&Local).date_naive()),
            days: 1,
        };
        let text = render_terminal(&got, &window, now, &style, None, false);
        assert!(text.contains("8 PRs: 8 open · 8 gone quiet"), "{text}");
        assert_eq!(text.matches("web#").count(), 5, "{text}");
        assert!(
            text.contains("… and 3 more with no recent activity (--all)"),
            "{text}"
        );
        let text = render_terminal(&got, &window, now, &style, None, true);
        assert_eq!(text.matches("web#").count(), 8, "{text}");
        assert!(!text.contains("more with no recent activity"), "{text}");
    }

    #[test]
    fn html_inlines_data_that_cannot_end_the_script() {
        let now = at("2026-11-11T14:20:00Z");
        let mut rows = vec![row(
            "o/web",
            1,
            PrState::Open,
            "2026-11-11T14:02:00Z",
            None,
            None,
        )];
        rows[0].pr.title = "</script><img src=x onerror=alert(1)>".into();
        let got = entries(rows, at("2026-11-11T00:00:00Z"), now, 3, |_| None);
        let window = Window {
            start: local_midnight(now.with_timezone(&Local).date_naive()),
            days: 1,
        };
        let html = render_html(&got, &window, now, 3).unwrap();
        assert!(!html.contains("/*DATA*/null"));
        assert!(!html.contains("</script><img"));
        assert!(html.contains("\\u003c/script>\\u003cimg"));
        assert_eq!(html.matches("</script>").count(), 1);
    }
}

//! Joins open PRs to session sightings on repository + head branch.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::github::PullRequest;
use crate::sessions::{Harness, Sighting};
use crate::stamps::Stamp;

/// How a session was tied to a PR, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatchKind {
    /// A commit in the PR names the session (`Agent-Session` or `Claude-Session`).
    Stamp,
    /// The session's repository (from its log or its folder's git remote) is the PR's.
    Repo,
    /// The folder no longer exists, but its path names the PR's repository.
    Path,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionMatch {
    pub kind: MatchKind,
    /// From the session's live record, when hooks are installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<crate::live::State>,
    #[serde(flatten)]
    pub sighting: Sighting,
}

#[derive(Debug, serde::Serialize)]
pub struct PrRow {
    #[serde(flatten)]
    pub pr: PullRequest,
    /// Newest first.
    pub sessions: Vec<SessionMatch>,
}

/// Where a sighting's folder points, resolved once per folder.
#[derive(Debug, Clone, PartialEq)]
pub enum Location {
    /// Every GitHub repository among the folder's remotes (a fork clone often
    /// has both the fork and upstream).
    Repos(Vec<String>),
    /// Folder is gone; keep its lowercase path components for name matching.
    Gone(Vec<String>),
    Unknown,
}

pub fn match_prs(prs: Vec<PullRequest>, sightings: &[Sighting]) -> Vec<PrRow> {
    let mut folders: HashMap<&str, Location> = HashMap::new();
    for s in sightings {
        folders
            .entry(s.cwd.as_str())
            .or_insert_with(|| locate(&s.cwd));
    }

    let mut by_branch: HashMap<&str, Vec<&Sighting>> = HashMap::new();
    for s in sightings {
        by_branch.entry(s.branch.as_str()).or_default().push(s);
    }

    // Latest sighting per session, to enrich sessions named by a stamp.
    let mut by_id: HashMap<(Harness, &str), &Sighting> = HashMap::new();
    for s in sightings {
        let e = by_id.entry((s.harness, s.session_id.as_str())).or_insert(s);
        if s.last_seen > e.last_seen {
            *e = s;
        }
    }

    prs.into_iter()
        .map(|pr| {
            let mut sessions: Vec<SessionMatch> = pr
                .stamps
                .iter()
                .map(|st| SessionMatch {
                    kind: MatchKind::Stamp,
                    state: None,
                    sighting: from_stamp(
                        st,
                        &pr,
                        by_id.get(&(st.harness, st.session_id.as_str())).copied(),
                    ),
                })
                .collect();
            if !is_default_branch(&pr) {
                for s in by_branch.get(pr.head_branch.as_str()).into_iter().flatten() {
                    let loc = sighting_location(s, &folders[s.cwd.as_str()]);
                    if let Some(kind) = classify(&pr, &loc) {
                        sessions.push(SessionMatch {
                            kind,
                            state: None,
                            sighting: (*s).clone(),
                        });
                    }
                }
            }
            PrRow {
                sessions: merge_sessions(sessions),
                pr,
            }
        })
        .collect()
}

/// A stamped session: the local sighting when this machine has one, else
/// what the stamp itself says.
fn from_stamp(st: &Stamp, pr: &PullRequest, local: Option<&Sighting>) -> Sighting {
    match local {
        Some(l) => Sighting {
            last_seen: l.last_seen.max(st.committed),
            machine: st.machine.clone().or_else(|| l.machine.clone()),
            ..l.clone()
        },
        None => Sighting {
            harness: st.harness,
            session_id: st.session_id.clone(),
            app: if st.url.is_some() {
                "web".into()
            } else {
                String::new()
            },
            cwd: String::new(),
            branch: pr.head_branch.clone(),
            repo_url: None,
            last_seen: st.committed,
            log_path: None,
            machine: st.machine.clone(),
            url: st.url.clone(),
        },
    }
}

/// One entry per session, newest first, keeping the strongest match kind and
/// filling gaps (app, machine, link) from the other matches of that session.
fn merge_sessions(mut sessions: Vec<SessionMatch>) -> Vec<SessionMatch> {
    sessions.sort_by_key(|m| std::cmp::Reverse(m.sighting.last_seen));
    let mut out: Vec<SessionMatch> = Vec::new();
    let mut index: HashMap<(Harness, String), usize> = HashMap::new();
    for m in sessions {
        let key = (m.sighting.harness, m.sighting.session_id.clone());
        match index.get(&key) {
            Some(&i) => {
                let kept = &mut out[i];
                kept.kind = kept.kind.min(m.kind);
                let k = &mut kept.sighting;
                if k.app.is_empty() {
                    k.app = m.sighting.app;
                }
                k.machine = k.machine.take().or(m.sighting.machine);
                k.url = k.url.take().or(m.sighting.url);
                k.log_path = k.log_path.take().or(m.sighting.log_path);
            }
            None => {
                index.insert(key, out.len());
                out.push(m);
            }
        }
    }
    out
}

/// Adds the repository the harness recorded itself (Codex) to the folder's remotes.
fn sighting_location(s: &Sighting, folder: &Location) -> Location {
    let Some(logged) = s.repo_url.as_deref().and_then(normalize_repo_url) else {
        return folder.clone();
    };
    match folder {
        Location::Repos(repos) if repos.contains(&logged) => folder.clone(),
        Location::Repos(repos) => Location::Repos(repos.iter().cloned().chain([logged]).collect()),
        _ => Location::Repos(vec![logged]),
    }
}

/// Branch names are scoped to a repository, so a session must be in the
/// repository the PR's branch lives in: the head repository (the fork, for a
/// fork PR), or the base repository when GitHub no longer knows the head.
pub fn classify(pr: &PullRequest, loc: &Location) -> Option<MatchKind> {
    let target = pr.head_repo.as_deref().unwrap_or(&pr.repo);
    match loc {
        Location::Repos(repos) => repos
            .iter()
            .any(|r| r.eq_ignore_ascii_case(target))
            .then_some(MatchKind::Repo),
        Location::Gone(parts) => {
            let name = target.rsplit('/').next()?;
            parts
                .iter()
                .any(|c| c.eq_ignore_ascii_case(name))
                .then_some(MatchKind::Path)
        }
        Location::Unknown => None,
    }
}

/// Branches that many unrelated sessions share, so they can't identify a PR:
/// the repositories' configured default branches, the PR's base, and the usual names.
pub fn is_default_branch(pr: &PullRequest) -> bool {
    let head = pr.head_branch.as_str();
    head == pr.base_branch
        || pr.default_branches.iter().any(|b| b == head)
        || matches!(head, "main" | "master" | "HEAD" | "")
}

fn locate(cwd: &str) -> Location {
    let path = Path::new(cwd);
    if !path.exists() {
        return Location::Gone(
            path.components()
                .filter_map(|c| c.as_os_str().to_str())
                .map(str::to_lowercase)
                .collect(),
        );
    }
    let Ok(out) = Command::new("git")
        .args(["-C", cwd, "remote", "-v"])
        .output()
    else {
        return Location::Unknown;
    };
    if !out.status.success() {
        return Location::Unknown;
    }
    let repos = parse_remotes(&String::from_utf8_lossy(&out.stdout));
    if repos.is_empty() {
        Location::Unknown
    } else {
        Location::Repos(repos)
    }
}

/// `git remote -v` lines: `<name>\t<url> (fetch|push)`.
fn parse_remotes(text: &str) -> Vec<String> {
    let mut repos: Vec<String> = text
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter_map(normalize_repo_url)
        .collect();
    repos.sort();
    repos.dedup();
    repos
}

/// `git@github.com:o/r.git`, `https://github.com/o/r` and `ssh://git@github.com/o/r.git` → `o/r`.
pub fn normalize_repo_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}").to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Harness;
    use chrono::{TimeZone, Utc};

    fn pr(repo: &str, head_repo: Option<&str>, branch: &str) -> PullRequest {
        PullRequest {
            repo: repo.into(),
            head_repo: head_repo.map(Into::into),
            number: 1,
            title: "t".into(),
            url: "u".into(),
            head_branch: branch.into(),
            base_branch: "main".into(),
            default_branches: vec!["main".into()],
            stamps: vec![],
        }
    }

    fn sighting(id: &str, branch: &str, url: Option<&str>, day: u32) -> Sighting {
        Sighting {
            harness: Harness::Codex,
            session_id: id.into(),
            app: "CLI".into(),
            cwd: "/definitely/not/here/myrepo/ws".into(),
            branch: branch.into(),
            repo_url: url.map(Into::into),
            last_seen: Utc.with_ymd_and_hms(2026, 10, day, 0, 0, 0).unwrap(),
            log_path: None,
            machine: None,
            url: None,
        }
    }

    #[test]
    fn normalizes_github_urls() {
        assert_eq!(
            normalize_repo_url("git@github.com:Owner/Repo.git").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(
            normalize_repo_url("https://github.com/o/r/").as_deref(),
            Some("o/r")
        );
        assert_eq!(
            normalize_repo_url("ssh://git@github.com/o/r.git").as_deref(),
            Some("o/r")
        );
        assert_eq!(normalize_repo_url("https://gitlab.com/o/r"), None);
    }

    #[test]
    fn default_branches_never_identify_a_pr() {
        let mut p = pr("o/r", None, "main");
        assert!(is_default_branch(&p));
        p.head_branch = "master".into();
        assert!(is_default_branch(&p));
        p.head_branch = "nazreen/feat".into();
        assert!(!is_default_branch(&p));
    }

    #[test]
    fn custom_default_branch_must_not_match_release_pr() {
        let mut p = pr("o/myrepo", None, "develop");
        p.base_branch = "release".into();
        p.default_branches = vec!["develop".into()];
        let sightings = vec![sighting(
            "s",
            "develop",
            Some("https://github.com/o/myrepo"),
            1,
        )];
        assert!(match_prs(vec![p], &sightings)[0].sessions.is_empty());
    }

    #[test]
    fn classify_uses_head_repo_and_gone_paths() {
        let p = pr("upstream/r", Some("me/r"), "feat");
        let repos = |v: &[&str]| Location::Repos(v.iter().map(|r| r.to_string()).collect());
        assert_eq!(classify(&p, &repos(&["me/r"])), Some(MatchKind::Repo));
        // A fork clone with upstream as another remote still matches.
        assert_eq!(
            classify(&p, &repos(&["me/r", "upstream/r"])),
            Some(MatchKind::Repo)
        );
        assert_eq!(classify(&p, &repos(&["upstream/r"])), None);
        assert_eq!(
            classify(
                &p,
                &Location::Gone(vec!["users".into(), "r".into(), "ws".into()])
            ),
            Some(MatchKind::Path)
        );
        assert_eq!(classify(&p, &Location::Unknown), None);
        // Head repository unknown (deleted fork): fall back to the base repository.
        assert_eq!(
            classify(&pr("upstream/r", None, "feat"), &repos(&["upstream/r"])),
            Some(MatchKind::Repo)
        );
    }

    #[test]
    fn fork_pr_must_not_match_unrelated_upstream_branch() {
        let sightings = vec![sighting(
            "s",
            "feat",
            Some("https://github.com/upstream/myrepo"),
            1,
        )];
        let rows = match_prs(
            vec![pr("upstream/myrepo", Some("fork/myrepo"), "feat")],
            &sightings,
        );
        assert!(rows[0].sessions.is_empty());
    }

    #[test]
    fn parses_all_github_remotes_once() {
        let text = "origin\tgit@github.com:me/r.git (fetch)\norigin\tgit@github.com:me/r.git (push)\n\
                    upstream\thttps://github.com/up/r (fetch)\nother\thttps://gitlab.com/x/y (fetch)\n";
        assert_eq!(parse_remotes(text), ["me/r", "up/r"]);
    }

    #[test]
    fn newest_session_first_and_wrong_repo_ignored() {
        let sightings = vec![
            sighting("old", "feat", Some("https://github.com/o/myrepo"), 1),
            sighting("new", "feat", Some("https://github.com/o/myrepo.git"), 5),
            sighting("elsewhere", "feat", Some("https://github.com/o/other"), 9),
        ];
        let rows = match_prs(vec![pr("o/myrepo", None, "feat")], &sightings);
        let ids: Vec<_> = rows[0]
            .sessions
            .iter()
            .map(|m| m.sighting.session_id.as_str())
            .collect();
        assert_eq!(ids, ["new", "old"]);
    }

    #[test]
    fn gone_folder_matches_by_path_component() {
        let sightings = vec![sighting("s", "feat", None, 1)];
        let rows = match_prs(vec![pr("o/myrepo", None, "feat")], &sightings);
        assert_eq!(rows[0].sessions[0].kind, MatchKind::Path);
    }

    #[test]
    fn stamp_beats_branch_match_and_works_on_default_branches() {
        use chrono::TimeZone;
        let mut p = pr("o/myrepo", None, "main");
        p.stamps = vec![Stamp {
            harness: Harness::Codex,
            session_id: "s".into(),
            machine: Some("box".into()),
            url: None,
            committed: Utc.with_ymd_and_hms(2026, 10, 3, 0, 0, 0).unwrap(),
        }];
        let local = vec![sighting(
            "s",
            "other",
            Some("https://github.com/o/myrepo"),
            1,
        )];
        let rows = match_prs(vec![p], &local);
        let m = &rows[0].sessions[0];
        assert_eq!(m.kind, MatchKind::Stamp);
        assert_eq!(m.sighting.app, "CLI", "enriched from the local log");
        assert_eq!(m.sighting.machine.as_deref(), Some("box"));
        assert_eq!(
            m.sighting.last_seen.to_rfc3339(),
            "2026-10-03T00:00:00+00:00"
        );
    }

    #[test]
    fn unknown_stamped_session_and_branch_match_merge_into_one() {
        use chrono::TimeZone;
        let mut p = pr("o/myrepo", None, "feat");
        p.stamps = vec![Stamp {
            harness: Harness::ClaudeCode,
            session_id: "session_01".into(),
            machine: None,
            url: Some("https://claude.ai/code/session_01".into()),
            committed: Utc.with_ymd_and_hms(2026, 10, 2, 0, 0, 0).unwrap(),
        }];
        let local = vec![sighting(
            "other",
            "feat",
            Some("https://github.com/o/myrepo"),
            5,
        )];
        let rows = match_prs(vec![p], &local);
        let s = &rows[0].sessions;
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].sighting.session_id, "other", "newest first");
        assert_eq!(s[1].kind, MatchKind::Stamp);
        assert_eq!(s[1].sighting.app, "web");
    }
}

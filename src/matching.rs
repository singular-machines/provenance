//! Joins open PRs to session sightings on repository + head branch.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::github::PullRequest;
use crate::sessions::Sighting;

/// How a session was tied to a PR, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatchKind {
    /// The session's repository (from its log or its folder's git remote) is the PR's.
    Repo,
    /// The folder no longer exists, but its path names the PR's repository.
    Path,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionMatch {
    pub kind: MatchKind,
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

    prs.into_iter()
        .map(|pr| {
            let mut sessions: Vec<SessionMatch> = Vec::new();
            if !is_default_branch(&pr) {
                for s in by_branch.get(pr.head_branch.as_str()).into_iter().flatten() {
                    let loc = sighting_location(s, &folders[s.cwd.as_str()]);
                    if let Some(kind) = classify(&pr, &loc) {
                        sessions.push(SessionMatch {
                            kind,
                            sighting: (*s).clone(),
                        });
                    }
                }
            }
            sessions.sort_by_key(|m| std::cmp::Reverse(m.sighting.last_seen));
            // Keep each session once, at its most recent sighting.
            let mut seen = std::collections::HashSet::new();
            sessions.retain(|m| seen.insert(m.sighting.session_id.clone()));
            PrRow { pr, sessions }
        })
        .collect()
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
            log_path: "/x".into(),
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
}

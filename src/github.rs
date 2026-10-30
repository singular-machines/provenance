//! Lists open pull requests through the GitHub CLI (`gh`), so provenance uses
//! whatever account `gh auth` already has.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::cache;
use crate::stamps::{self, Stamp};

/// How long a cached PR list is used before searching GitHub again.
pub const TTL_SECS: i64 = 120;

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct PullRequest {
    /// GraphQL node ID, for fetching commits.
    pub id: String,
    pub head_sha: String,
    /// Tip of the base branch: the PR's commit list depends on it as well as the head.
    pub base_sha: String,
    pub repo: String,
    pub head_repo: Option<String>,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    /// Configured default branches of the base and head repositories.
    pub default_branches: Vec<String>,
    /// Session stamps in the PR's commit messages, oldest commit first.
    pub stamps: Vec<Stamp>,
}

const QUERY: &str = r#"
query($q: String!, $endCursor: String) {
  search(query: $q, type: ISSUE, first: 100, after: $endCursor) {
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        id number title url headRefName headRefOid baseRefName baseRefOid
        repository { nameWithOwner defaultBranchRef { name } }
        headRepository { nameWithOwner defaultBranchRef { name } }
      }
    }
  }
}"#;

/// Commit messages for up to 100 PRs at once, by node ID.
const COMMITS_QUERY: &str = r#"
query($ids: [ID!]!) {
  nodes(ids: $ids) {
    ... on PullRequest {
      id headRefOid baseRefOid
      commits(last: 100) { nodes { commit { message committedDate } } }
    }
  }
}"#;

const COMMIT_BATCH: usize = 50;

#[derive(Deserialize)]
struct Page {
    data: Data,
}
#[derive(Deserialize)]
struct Data {
    search: Search,
}
#[derive(Deserialize)]
struct Search {
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    id: Option<String>,
    head_ref_oid: Option<String>,
    base_ref_oid: Option<String>,
    number: Option<u64>,
    title: Option<String>,
    url: Option<String>,
    head_ref_name: Option<String>,
    base_ref_name: Option<String>,
    repository: Option<Repo>,
    head_repository: Option<Repo>,
}
#[derive(Deserialize)]
struct CommitsPage {
    data: CommitsData,
}
#[derive(Deserialize)]
struct CommitsData {
    nodes: Vec<Option<CommitsNode>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitsNode {
    id: Option<String>,
    head_ref_oid: Option<String>,
    base_ref_oid: Option<String>,
    commits: Option<Commits>,
}

/// One PR's commits as GitHub reported them, with the head and base they belong to.
struct CommitSet {
    id: String,
    head_sha: String,
    base_sha: String,
    stamps: Vec<Stamp>,
}
#[derive(Deserialize)]
struct Commits {
    nodes: Vec<CommitNode>,
}
#[derive(Deserialize)]
struct CommitNode {
    commit: Commit,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    message: String,
    committed_date: DateTime<Utc>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repo {
    name_with_owner: String,
    default_branch_ref: Option<BranchRef>,
}
#[derive(Deserialize)]
struct BranchRef {
    name: String,
}

pub struct Fetched {
    pub prs: Vec<PullRequest>,
    pub fetched_at: DateTime<Utc>,
    pub from_cache: bool,
}

#[derive(serde::Serialize, Deserialize)]
struct CachedSearch {
    fetched_at: DateTime<Utc>,
    prs: Vec<PullRequest>,
}

/// Stamps per PR, head commit and base commit: a PR's commit list is fixed
/// by those three, so these entries never go stale.
type StampCache = HashMap<String, Vec<Stamp>>;

fn stamp_key(id: &str, head: &str, base: &str) -> String {
    format!("{id}@{head}@{base}")
}

fn pr_key(pr: &PullRequest) -> String {
    stamp_key(&pr.id, &pr.head_sha, &pr.base_sha)
}

/// Open PRs for `search` (a GitHub search qualifier such as `author:@me`).
/// Uses a cached result younger than `TTL_SECS` unless `refresh` is set, and
/// fetches commits only for PRs whose head commit isn't in the stamp cache.
pub fn open_prs(
    search: &str,
    cache_dir: &Path,
    refresh: bool,
    now: DateTime<Utc>,
) -> Result<Fetched> {
    let search_path = cache::path(cache_dir, &format!("search-{}", cache::key(search)));
    if !refresh
        && let Some(c) = cache::read::<CachedSearch>(&search_path)
        && (now - c.fetched_at).num_seconds() < TTL_SECS
        && c.fetched_at <= now
    {
        return Ok(Fetched {
            prs: c.prs,
            fetched_at: c.fetched_at,
            from_cache: true,
        });
    }

    let q = format!("is:pr is:open archived:false {search}");
    let text = gh(&[
        "api",
        "graphql",
        "--paginate",
        "-f",
        &format!("query={QUERY}"),
        "-f",
        &format!("q={q}"),
    ])?;
    let mut prs = parse_pages(&text)?;

    let stamps_path = cache::path(cache_dir, "stamps");
    let mut known: StampCache = cache::read(&stamps_path).unwrap_or_default();
    let missing: Vec<String> = prs
        .iter()
        .filter(|p| !known.contains_key(&pr_key(p)))
        .map(|p| p.id.clone())
        .collect();
    let mut complete = true;
    for batch in missing.chunks(COMMIT_BATCH) {
        let mut args = vec![
            "api".to_string(),
            "graphql".into(),
            "-f".into(),
            format!("query={COMMITS_QUERY}"),
        ];
        for id in batch {
            args.push("-f".into());
            args.push(format!("ids[]={id}"));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let sets = parse_commits(&gh(&refs)?)?;
        complete &= batch.iter().all(|id| sets.iter().any(|c| &c.id == id));
        record_commit_sets(&mut prs, &mut known, sets);
    }
    for pr in &mut prs {
        pr.stamps = known.get(&pr_key(pr)).cloned().unwrap_or_default();
    }
    // Keep the cache to current PR heads, plus older entries while it's small.
    if known.len() > 2000 {
        let current: std::collections::HashSet<String> = prs.iter().map(pr_key).collect();
        known.retain(|k, _| current.contains(k));
    }
    cache::write(&stamps_path, &known);
    // Don't cache a list where some PR's commits couldn't be read.
    if complete {
        cache::write(
            &search_path,
            &CachedSearch {
                fetched_at: now,
                prs: prs.clone(),
            },
        );
    }
    Ok(Fetched {
        prs,
        fetched_at: now,
        from_cache: false,
    })
}

/// Store fetched commit sets. A push or retarget between the search and the
/// commit query leaves them disagreeing; the commit query is newer, so its
/// head and base win and the PR's stamps are found under them.
fn record_commit_sets(prs: &mut [PullRequest], known: &mut StampCache, sets: Vec<CommitSet>) {
    for set in sets {
        if let Some(pr) = prs.iter_mut().find(|p| p.id == set.id) {
            pr.head_sha.clone_from(&set.head_sha);
            pr.base_sha.clone_from(&set.base_sha);
        }
        known.insert(stamp_key(&set.id, &set.head_sha, &set.base_sha), set.stamps);
    }
}

fn gh(args: &[&str]) -> Result<String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .context("running gh (is the GitHub CLI installed?)")?;
    if !output.status.success() {
        bail!(
            "gh api graphql failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Each PR's stamps from a `COMMITS_QUERY` response.
fn parse_commits(text: &str) -> Result<Vec<CommitSet>> {
    let page: CommitsPage = serde_json::from_str(text).context("parsing gh output")?;
    Ok(page
        .data
        .nodes
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let stamps = n
                .commits
                .map(|c| c.nodes)
                .unwrap_or_default()
                .iter()
                .flat_map(|c| stamps::parse(&c.commit.message, c.commit.committed_date))
                .collect();
            Some(CommitSet {
                id: n.id?,
                head_sha: n.head_ref_oid?,
                base_sha: n.base_ref_oid.unwrap_or_default(),
                stamps,
            })
        })
        .collect())
}

/// `gh --paginate` prints one JSON document per page, back to back.
fn parse_pages(text: &str) -> Result<Vec<PullRequest>> {
    let mut prs = Vec::new();
    for page in serde_json::Deserializer::from_str(text).into_iter::<Page>() {
        for n in page.context("parsing gh output")?.data.search.nodes {
            let (Some(id), Some(number), Some(repo), Some(head)) =
                (n.id, n.number, n.repository, n.head_ref_name)
            else {
                continue;
            };
            let default_branches = std::iter::once(&repo)
                .chain(n.head_repository.as_ref())
                .filter_map(|r| r.default_branch_ref.as_ref().map(|b| b.name.clone()))
                .collect();
            prs.push(PullRequest {
                id,
                head_sha: n.head_ref_oid.unwrap_or_default(),
                base_sha: n.base_ref_oid.unwrap_or_default(),
                repo: repo.name_with_owner,
                head_repo: n.head_repository.map(|r| r.name_with_owner),
                default_branches,
                stamps: Vec::new(),
                number,
                title: n.title.unwrap_or_default(),
                url: n.url.unwrap_or_default(),
                head_branch: head,
                base_branch: n.base_ref_name.unwrap_or_default(),
            });
        }
    }
    Ok(prs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parses_concatenated_pages_and_skips_non_prs() {
        let text = r#"{"data":{"search":{"pageInfo":{"hasNextPage":true,"endCursor":"a"},"nodes":[
            {"id":"PR_1","number":1,"title":"One","url":"u1","headRefName":"feat","headRefOid":"aaa","baseRefName":"main",
             "repository":{"nameWithOwner":"o/r","defaultBranchRef":{"name":"develop"}},
             "headRepository":{"nameWithOwner":"me/r","defaultBranchRef":{"name":"main"}}},
            {}]}}}
            {"data":{"search":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"id":"PR_2","number":2,"title":"Two","url":"u2","headRefName":"fix","headRefOid":"bbb","baseRefName":"main",
             "repository":{"nameWithOwner":"o/r"},"headRepository":null}]}}}"#;
        let prs = parse_pages(text).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].id, "PR_1");
        assert_eq!(prs[0].head_sha, "aaa");
        assert_eq!(prs[0].head_repo.as_deref(), Some("me/r"));
        assert_eq!(prs[0].default_branches, ["develop", "main"]);
        assert!(prs[1].default_branches.is_empty());
        assert_eq!(prs[1].number, 2);
    }

    #[test]
    fn parses_commit_stamps_by_head() {
        let text = r#"{"data":{"nodes":[
            {"id":"PR_1","headRefOid":"aaa","baseRefOid":"b1","commits":{"nodes":[{"commit":{"message":"x\n\nAgent-Session: codex/t1\nAgent-Machine: mbp","committedDate":"2026-10-30T01:00:00Z"}}]}},
            null,
            {"id":"PR_2","headRefOid":"bbb","baseRefOid":"b2","commits":{"nodes":[]}}]}}"#;
        let got = parse_commits(text).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].id.as_str(), got[0].head_sha.as_str()),
            ("PR_1", "aaa")
        );
        assert_eq!(got[0].stamps[0].session_id, "t1");
        assert_eq!(got[0].stamps[0].machine.as_deref(), Some("mbp"));
        assert!(got[1].stamps.is_empty());
    }

    #[test]
    fn fresh_cache_is_used_without_calling_gh() {
        let dir = std::env::temp_dir().join(format!("prov-gh-{}", std::process::id()));
        let now = Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap();
        let search = "author:@me";
        let pr = parse_pages(
            r#"{"data":{"search":{"nodes":[{"id":"PR_1","number":1,"headRefName":"f","headRefOid":"a",
               "repository":{"nameWithOwner":"o/r"}}]}}}"#,
        )
        .unwrap();
        let cached = CachedSearch {
            fetched_at: now - chrono::Duration::seconds(30),
            prs: pr,
        };
        cache::write(
            &cache::path(&dir, &format!("search-{}", cache::key(search))),
            &cached,
        );
        let got = open_prs(search, &dir, false, now).unwrap();
        assert!(got.from_cache);
        assert_eq!(got.fetched_at, cached.fetched_at);
        assert_eq!(got.prs[0].id, "PR_1", "PRs survive the cache round trip");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stamp_keys_change_with_head_or_base() {
        assert_ne!(stamp_key("PR", "h1", "b1"), stamp_key("PR", "h2", "b1"));
        assert_ne!(
            stamp_key("PR", "h1", "b1"),
            stamp_key("PR", "h1", "b2"),
            "retargeted base"
        );
    }

    #[test]
    fn head_moved_between_queries_keeps_stamps() {
        let mut prs = parse_pages(
            r#"{"data":{"search":{"nodes":[{"id":"PR_1","number":1,"headRefName":"f","headRefOid":"old",
               "baseRefOid":"b","repository":{"nameWithOwner":"o/r"}}]}}}"#,
        )
        .unwrap();
        let sets = parse_commits(
            r#"{"data":{"nodes":[{"id":"PR_1","headRefOid":"new","baseRefOid":"b","commits":{"nodes":[
               {"commit":{"message":"x\n\nAgent-Session: codex/t1","committedDate":"2026-10-30T01:00:00Z"}}]}}]}}"#,
        )
        .unwrap();
        let mut known = StampCache::new();
        record_commit_sets(&mut prs, &mut known, sets);
        assert_eq!(prs[0].head_sha, "new");
        assert_eq!(known[&pr_key(&prs[0])][0].session_id, "t1");
    }
}

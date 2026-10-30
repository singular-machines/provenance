//! Lists open pull requests through the GitHub CLI (`gh`), so provenance uses
//! whatever account `gh auth` already has.

use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::stamps::{self, Stamp};

#[derive(Debug, Clone, serde::Serialize)]
pub struct PullRequest {
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
        number title url headRefName baseRefName
        repository { nameWithOwner defaultBranchRef { name } }
        headRepository { nameWithOwner defaultBranchRef { name } }
        commits(last: 100) { nodes { commit { message committedDate } } }
      }
    }
  }
}"#;

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
    number: Option<u64>,
    title: Option<String>,
    url: Option<String>,
    head_ref_name: Option<String>,
    base_ref_name: Option<String>,
    repository: Option<Repo>,
    head_repository: Option<Repo>,
    commits: Option<Commits>,
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

/// `search` is a GitHub search qualifier string, e.g. `author:@me`.
pub fn open_prs(search: &str) -> Result<Vec<PullRequest>> {
    let q = format!("is:pr is:open archived:false {search}");
    let output = Command::new("gh")
        .args(["api", "graphql", "--paginate", "-f"])
        .arg(format!("query={QUERY}"))
        .args(["-f", &format!("q={q}")])
        .output()
        .context("running gh (is the GitHub CLI installed?)")?;
    if !output.status.success() {
        bail!(
            "gh api graphql failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_pages(&String::from_utf8_lossy(&output.stdout))
}

/// `gh --paginate` prints one JSON document per page, back to back.
fn parse_pages(text: &str) -> Result<Vec<PullRequest>> {
    let mut prs = Vec::new();
    for page in serde_json::Deserializer::from_str(text).into_iter::<Page>() {
        for n in page.context("parsing gh output")?.data.search.nodes {
            let (Some(number), Some(repo), Some(head)) = (n.number, n.repository, n.head_ref_name)
            else {
                continue;
            };
            let default_branches = std::iter::once(&repo)
                .chain(n.head_repository.as_ref())
                .filter_map(|r| r.default_branch_ref.as_ref().map(|b| b.name.clone()))
                .collect();
            let stamps = n
                .commits
                .map(|c| c.nodes)
                .unwrap_or_default()
                .iter()
                .flat_map(|c| stamps::parse(&c.commit.message, c.commit.committed_date))
                .collect();
            prs.push(PullRequest {
                repo: repo.name_with_owner,
                head_repo: n.head_repository.map(|r| r.name_with_owner),
                default_branches,
                stamps,
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

    #[test]
    fn parses_concatenated_pages_and_skips_non_prs() {
        let text = r#"{"data":{"search":{"pageInfo":{"hasNextPage":true,"endCursor":"a"},"nodes":[
            {"number":1,"title":"One","url":"u1","headRefName":"feat","baseRefName":"main",
             "repository":{"nameWithOwner":"o/r","defaultBranchRef":{"name":"develop"}},
             "headRepository":{"nameWithOwner":"me/r","defaultBranchRef":{"name":"main"}},
             "commits":{"nodes":[{"commit":{"message":"x\n\nAgent-Session: codex/t1\nAgent-Machine: mbp","committedDate":"2026-10-30T01:00:00Z"}}]}},
            {}]}}}
            {"data":{"search":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"number":2,"title":"Two","url":"u2","headRefName":"fix","baseRefName":"main",
             "repository":{"nameWithOwner":"o/r"},"headRepository":null}]}}}"#;
        let prs = parse_pages(text).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].head_repo.as_deref(), Some("me/r"));
        assert_eq!(prs[0].default_branches, ["develop", "main"]);
        assert!(prs[1].default_branches.is_empty());
        assert_eq!(prs[0].stamps[0].session_id, "t1");
        assert_eq!(prs[0].stamps[0].machine.as_deref(), Some("mbp"));
        assert!(prs[1].stamps.is_empty());
        assert_eq!(prs[1].number, 2);
    }
}

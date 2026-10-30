# provenance

Find the coding-agent session behind each pull request.

You run many Claude Code and Codex sessions across apps (terminal, Claude desktop, Codex desktop, T3 Code, Conductor), and they open PRs. When you look at a PR, provenance tells you which session worked on it, in which app, and when it was last active.

```
$ provenance prs
REPO                        PR    TITLE                                   LAST SESSION                       MATCH  ACTIVE
acme/storefront            #924  Add retry when checkout times out…       Claude Code · T3 Code · 4f2a91c0   repo   9m
acme/api                    #108  Resume interrupted jobs after a rest…  Codex · singular · 01a1c0de        repo   1d
acme/api                    #68   Ask before rebuilding the local cache…  Claude Code · Conductor · 7be310aa path   3d
acme/chat-bot               #950  fix: remove unused exports from the …  — no session found

70 open PRs · 15 with a session (14 by repo, 1 by folder path) · 55 no session found
```

Terminal only. No setup beyond the GitHub CLI.

## Install

```sh
cargo install --git https://github.com/singular-machines/provenance
gh auth status   # provenance uses your existing gh login
```

## Usage

```sh
provenance prs                    # your open PRs across GitHub
provenance prs --repo owner/name  # one repository
provenance prs --author someone   # someone else's PRs
provenance prs --json             # every matching session per PR, for scripts and agents
```

## How it works

Nothing needs installing in the harnesses. Both already record the branch and folder in their own session logs, on this machine:

| Harness | Logs | What it records |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` | `gitBranch` and `cwd` on every message; `entrypoint` (`cli`, `claude-desktop`, `sdk-ts`) |
| Codex | `~/.codex/sessions/**/*.jsonl` | `git.branch` and `git.repository_url` once, when the session starts; `originator` (`T3 Code`, `Codex Desktop`, `codex-tui`, …) |

provenance lists your open PRs through `gh`, then matches each PR's repository and head branch against those logs, and picks the most recently active session.

- **MATCH `repo`:** one of the session folder's GitHub remotes (or the repository Codex logged) is the repository the PR's branch lives in: the fork for a fork PR, otherwise the PR's repository.
- **MATCH `path`:** the session's folder has since been deleted, but its path names that repository, as in `…/conductor/workspaces/<repo>/<workspace>`.
- PRs whose head branch is a repository's configured default branch, `main`, `master` or the PR's own base are never matched, because many unrelated sessions share those branches.
- `ACTIVE` is the time of the session's last logged event.
- Apps built on the agent SDKs report a generic entrypoint, so they're named from their worktree folder: `.t3/worktrees` → T3 Code, `conductor/workspaces` → Conductor.

## Known gaps

- **Only this machine.** Sessions that ran on another machine or a server show "no session found".
- **Codex records its branch only at session start.** A Codex session that starts on `main` and then creates the PR branch isn't matched yet. Commit stamps (next milestone) close this gap.
- **A session's folder is where it started.** A session that works on another repository by changing directory in its shell is logged under its own folder and branch, so it isn't matched. Commit stamps close this gap too.
- **No live state yet.** `ACTIVE` says when the session last did something, not whether it's still running.

## Roadmap

1. ✅ `provenance prs` from existing session logs.
2. A git hook that stamps each agent commit with its session ID (`CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`), plus session hooks for running / idle / ended state.
3. `provenance show`, `go` (reopen the session) and `send` (steer it through Singular).
4. An MCP server exposing the same functions to agents.

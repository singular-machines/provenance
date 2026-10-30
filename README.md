# provenance

Find the coding-agent session behind each pull request.

You run many Claude Code and Codex sessions across apps (terminal, Claude desktop, Codex desktop, T3 Code, Conductor), and they open PRs. When you look at a PR, provenance tells you which session worked on it, in which app, and when it was last active.

Example output (`STATE` appears once `provenance install` has been run):

```
$ provenance prs
REPO                        PR    TITLE                                   LAST SESSION                       MATCH  STATE      ACTIVE
acme/storefront            #927  Add retry when checkout times out…       Claude Code · T3 Code · 4f2a91c0   repo   ● running  55m
acme/chat-bot               #950  fix: remove unused exports from the …  Claude Code · web · 01Hq3xKe       stamp             7h
acme/api                    #108  Resume interrupted jobs after a rest…  Codex · singular · 01a1c0de        repo   ◐ waiting  1d
acme/api                    #68   Ask before rebuilding the local cache…  Claude Code · Conductor · 7be310aa path              3d
example/web                 #41   Bump deps                               — no session found

70 open PRs · 40 with a session (25 by stamp, 14 by repo, 1 by folder path) · 30 no session found
```

Terminal only. No setup beyond the GitHub CLI.

## Install

```sh
cargo install --git https://github.com/singular-machines/provenance
gh auth status   # provenance uses your existing gh login
```

## Usage

```sh
provenance install                # optional: commit stamps + live session state (see below)
provenance prs                    # your open PRs across GitHub
provenance prs --repo owner/name  # one repository
provenance prs --author someone   # someone else's PRs
provenance prs --json             # every matching session per PR, for scripts and agents
provenance prs --refresh          # search GitHub now instead of using the 2-minute cache
provenance machine [alias]        # show or set this machine's name in stamps
```

## How it works

`provenance prs` lists your open PRs through `gh` and links each one to sessions in three ways, strongest first:

| MATCH | How |
|---|---|
| `stamp` | A commit in the PR names the session: `Agent-Session: <harness>/<id>` (written by provenance's git hook) or `Claude-Session: https://claude.ai/code/session_…` (written by Claude Code on the web). |
| `repo` | The session was on the PR's head branch, in a folder whose GitHub remotes (or the repository Codex logged) include the repository that branch lives in: the fork for a fork PR, otherwise the PR's repository. |
| `path` | Same, but the folder has since been deleted and its path names that repository, as in `…/conductor/workspaces/<repo>/<workspace>`. |

Branch matches come from the harnesses' own session logs, so they work with nothing installed:

| Harness | Logs | What it records |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` | `gitBranch` and `cwd` on every message; `entrypoint` (`cli`, `claude-desktop`, `sdk-ts`) |
| Codex | `~/.codex/sessions/**/*.jsonl` | `git.branch` and `git.repository_url` once, when the session starts; `originator` (`T3 Code`, `Codex Desktop`, `codex-tui`, …) |

- PRs whose head branch is a repository's configured default branch, `main`, `master` or the PR's own base are never branch-matched, because many unrelated sessions share those branches. Stamps still count.
- `ACTIVE` is the session's last logged event, or the stamped commit's time for sessions not on this machine.
- Apps built on the agent SDKs report a generic entrypoint, so they're named from their worktree folder: `.t3/worktrees` → T3 Code, `conductor/workspaces` → Conductor.
- Sessions known only from a stamp show where they ran: `web`, or `on <machine>`.

## Speed

`provenance prs` keeps caches in `~/.cache/provenance/`, so a repeat run takes well under a second:

- **PR list:** reused for 2 minutes; the table footer says how old it is. `--refresh` searches GitHub now.
- **Commit stamps:** kept per PR head commit, which never changes, so only PRs with new pushes have their commits fetched again.
- **Session logs:** each log file is read again only when its size or modified time changes.

Deleting `~/.cache/provenance/` is always safe; it's rebuilt on the next run.

## `provenance install`

Optional. Adds two things, and `provenance uninstall` removes both:

1. **A git hook that stamps agent commits.** It adds `hook.provenance` to your global git config: a config-defined `prepare-commit-msg` hook, which git runs alongside each repository's own hooks (including repositories with their own `core.hooksPath`), so nothing is displaced. When a commit is made inside a Claude Code or Codex session (`CLAUDE_CODE_SESSION_ID` or `CODEX_THREAD_ID` is set), it adds `Agent-Session` and `Agent-Machine` trailers. Commits you make yourself are left alone. Needs a git that supports hooks defined in config (`hook.<name>.command`, as in git 2.56); `install` checks and refuses otherwise.
2. **Session hooks for Claude Code and Codex** (`SessionStart`, `UserPromptSubmit`, `PostToolUse`, `Stop`, `SessionEnd`) in `~/.claude/settings.json` and `~/.codex/hooks.json`, appended after your existing hooks; a backup is written first and the file keeps its permissions. `uninstall` removes only provenance's own commands. In Codex, which keys hook approvals by position, a removed group that has later groups after it keeps a no-op `true` command so those later hooks keep their approvals. Each event updates `~/.local/state/provenance/live/<harness>-<id>.json`: the harness process ID and its start time, the last event, and every folder and branch the session has been on. That gives the `STATE` column:

| STATE | Meaning |
|---|---|
| `● running` | Working on a turn, and its process is up |
| `◐ waiting` | Finished its turn; waiting for you |
| `✕ stopped` | Was mid-turn when its process disappeared |
| `○ ended` | The session was closed |

Codex asks you to review new hooks before it runs them: approve the provenance hooks when it asks.

**Machine name.** Stamps never contain your host name. Each machine gets a random ID such as `m-7f3a9c2e`, created on first use and kept in the global git setting `provenance.machine`. To use a name of your choice instead, run `provenance machine <alias>` (letters, digits, `.`, `-`, `_`); `provenance machine` shows the current one, and `PROVENANCE_MACHINE` overrides it for one shell. Stamps are part of commit history, so on public repositories they publish the session ID and this machine name.

## Known gaps

- **Only this machine.** Sessions that ran on another machine or a server show "no session found".
- **Without `install`:** a Codex session that starts on `main` and then creates the PR branch isn't matched (Codex logs its branch only at session start), and nor is a session that works in another repository by changing directory. Stamps and session hooks cover both.
- **A repository's own `prepare-commit-msg` runs after provenance's.** One that rewrites the whole message would drop the stamp.
- **Desktop apps start a new harness process per turn** (Conductor does; others may), so between turns a waiting session has no process of its own. `STATE` relies on the last event for that reason.
- **Stamps only cover commits made after install.**

## Roadmap

1. ✅ `provenance prs` from existing session logs.
2. ✅ Commit stamps (including Claude Code on the web's) and live session state.
3. `provenance show`, `go` (reopen the session) and `send` (steer it through Singular).
4. An MCP server exposing the same functions to agents.

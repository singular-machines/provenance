<div align="center">

# provenance

**Which agent session opened this PR, and is it still running?**

[![CI](https://github.com/singular-machines/provenance/actions/workflows/ci.yml/badge.svg)](https://github.com/singular-machines/provenance/actions/workflows/ci.yml) ![Rust](https://img.shields.io/badge/Rust-2024-dea584?logo=rust&logoColor=white) ![Claude Code](https://img.shields.io/badge/Claude_Code-supported-d97757) ![Codex](https://img.shields.io/badge/Codex-supported-10a37f) ![OpenCode](https://img.shields.io/badge/OpenCode-supported-555) ![Pi](https://img.shields.io/badge/Pi-supported-6f42c1) [![License](https://img.shields.io/badge/license-MIT_or_Apache--2.0-blue)](#license)

[Quick start](#quick-start) · [Reading the table](#reading-the-table) · [Timeline](#timeline) · [How it works](#how-it-works) · [Install hooks](#install-hooks) · [Privacy](#privacy) · [Roadmap](#roadmap) · [License](#license)

<img src="docs/demo.gif" alt="Typing provenance prs lists open PRs grouped by repository, each with the app, harness and ID of its session and whether it is running or waiting for you; then provenance go acme/storefront#896 resumes that PR's ended Claude Code session in its folder" width="860">

</div>

You run Claude Code, Codex, OpenCode and Pi in lots of apps: the terminal, Claude desktop, Codex desktop, T3 Code, Conductor, the web. They all open PRs. On GitHub you can't tell which session opened a PR, or whether that session is still alive.

`provenance prs` lists your open PRs with the session behind each one: the app it runs in, when it was last active, and whether it's running or waiting for you. `provenance go` opens that session, and `provenance send` messages it.

| | Claude Code | Codex | OpenCode | Pi |
|---|---|---|---|---|
| `prs`: the session behind each PR | Yes | Yes | Yes | Yes |
| Live status | With `install` | With `install` | From its database, guessing "waiting" while any `opencode` runs | With `install` |
| `go`: switch to a running terminal session | With `install` | With `install` | No: prints the resume command | With `install` |
| `go`: resume an ended session | Yes | Yes | Yes | Yes |
| `send`: message the session | With `install`; bypass-mode targets only through T3 Code | With `install` | Not yet | Not yet |
| `reply`: answer an incoming message | Identified local session | CLI can select the exact shared-process session using its harness IDs | Not supported | Not supported |

For terminal sessions without usable process records, `go` resumes in the session folder and may start a second copy. Desktop-app sessions use the app or deep-link routes below without requiring `install`. `send` refuses when it cannot tell whether the session is running, unless `--resume` is supplied and the last recorded state allows it; see the messaging policy below.

## Highlights

- **Works with nothing installed.** It reads the session logs Claude Code, Codex and Pi already write, and OpenCode's session database.
- **Exact matches with stamps.** An optional git hook adds `Agent-Session` trailers to agent commits. Claude Code on the web's `Claude-Session` trailers are read too.
- **Live status.** Session hooks show whether each session is running, waiting for you, stopped or ended.
- **Every app.** Terminal, Claude desktop, Codex desktop, T3 Code, Conductor and the web.
- **Fast.** Cached repeat runs take well under a second.
- **Private by default.** Commit stamps carry a random machine ID, never your host name.
- **Terminal only.** One Rust binary plus the GitHub CLI. `--json` for scripts and agents.

## Quick start

```sh
cargo install --git https://github.com/singular-machines/provenance
gh auth status          # provenance uses your existing gh login
provenance prs          # your open PRs and the session behind each
provenance install      # optional: exact stamps and live status
```

## Usage

| Command | What it does |
|---|---|
| `provenance prs` | Your open PRs across GitHub |
| `provenance prs --repo owner/name` | One repository |
| `provenance prs --author someone` | Someone else's PRs |
| `provenance prs --state merged` | Merged PRs instead (`closed` or `all` too; the newest 100) |
| `provenance prs --verbose` | Also count matches by stamp, repo and folder path |
| `provenance prs --json` | Every matching session per PR, for scripts and agents |
| `provenance prs --refresh` | Search GitHub now instead of using the 2-minute cache |
| `provenance recap` | What your PRs did today, and which need you now |
| `provenance recap --since 7d` | Over a longer window: `today`, `yesterday`, `7d` (up to `366d`) or a date such as `2026-11-05` |
| `provenance recap --html` | Write the timeline as an HTML page and open it (`--html <path>` writes it there instead) |
| `provenance recap --json` | The same entries for scripts and agents |
| `provenance go <PR>` | Open the session behind a PR (`123`, `owner/name#123` or a PR URL), open or merged, by anyone |
| `provenance go <PR> --print` | Show what `go` would run, without running it |
| `provenance send <PR> "message"` | Message the session behind a PR (Claude Code or Codex) |
| `provenance send <PR> "message" --wait` | Also wait for its reply (default up to 300 seconds) |
| `provenance reply <message-id> "text"` | Answer from the exact receiving session; also works from shared Codex CLI sessions |
| `provenance send <PR> "message" --resume` | Resume headless a session whose process is gone, or an unknown one whose last recorded state is not running, waiting or stopped |
| `provenance send <PR> "message" --resume --force` | Override an unknown process recorded as running, waiting or stopped, only when you know it is closed |
| `provenance install` / `uninstall` | Add or remove the commit-stamp and session hooks and the MCP server |
| `provenance machine [alias]` | Show or set this machine's name in stamps |
| `provenance peer add <ssh-destination>` | Pair with another machine so its sessions can be messaged (`peer list`, `peer remove`, `peer pull`) |
| `provenance send '@name@machine' "message"` | Message a session registered under a name on a paired machine |

### Named sessions

From the intended session, at the owner's request:

```sh
provenance register "PR Reviewer"
provenance names --json
provenance send '@PR Reviewer' "Please review the change" --wait
provenance send '@pr-reviewer' "Please review the change" --resume
provenance register "PR Reviewer" --replace "codex/<previous-session-id>"
provenance unregister "PR Reviewer"
```

`@` exclusively marks a name. Numeric PR addresses, `owner/repo#number` and PR URLs keep their existing selection behaviour. Names trim surrounding whitespace, lowercase ASCII and collapse runs of spaces/hyphens to `-`. After normalising they accept only ASCII letters, digits and hyphens, with 1–64 characters; unsupported punctuation is rejected.

Names share one namespace for the current OS user on this machine, across repositories. The private, versioned `~/.local/state/provenance/names.json` registry uses locked atomic writes. It stores display name, harness, exact session ID, registration time and a diagnostic folder hint; it stores no PID or socket. Delivery looks up that exact harness/session in hook records, never the folder hint or another session. Inspecting names reports the identity, folder hint and current delivery availability. Registration can succeed while unavailable and reports that status.

Set `PROVENANCE_STATE_DIR` to an absolute path to keep all provenance state there instead of `~/.local/state/provenance`; an unset or empty value keeps the default. Start Claude Code, Codex, or another harness with this variable set so its installed hooks and MCP server inherit it. `provenance install` also passes the resolved directory to its macOS reply-collection agent.

A binding has no expiry. It survives resume or compaction **when the harness keeps the same ID**. A fork, `/clear`, new ID or replacement session never inherits it. An ended session stays registered but unavailable; explicit `--resume` may reopen that exact session when its hook/transcript information is available. Missing target information is reported and the binding remains inspectable/removable. A named send snapshots the concrete recipient once, does not query GitHub and never falls back to another session. Busy-target handling is unchanged; delivery acceptance does not mean completed work. Replies and `--wait` stay tied to the original message/session even after the name is reassigned.

Registering the same name for the same identity is idempotent. Another identity requires `--replace <incumbent-harness>/<incumbent-session-id>` matching under the registry lock. Unregister snapshots the incumbent and checks it again under the write lock, refusing a concurrent reassignment.

| Named-session support | Registration and sending |
|---|---|
| Local Claude Code | Exact ancestor PID/start time and installed hook records |
| Local Codex, including shared app-server | Same verification using active records; multiple candidates require agreeing `CODEX_THREAD_ID` and `CODEX_SESSION_ID` matching that verified process. Supplied IDs must also match a sole candidate |
| OpenCode, Pi, web, another machine | Unsupported; naming adds no transport support |

Codex's macOS sandbox can block `ps`; provenance falls back to native process queries with the same PID/start-time checks and the kernel-reported invoked executable path (`KERN_PROCARGS2`), preserving symlink-launched harness names. Relative invoked paths are accepted. If that path is unavailable, ancestor lookup stops instead of selecting an outer session. Node/Bun ancestors also stop the walk if their process title cannot be read; no native title inference is attempted. Liveness uses kernel PID/start time independently of path/title access. Ended (`SessionEnd`) records do not participate in caller identification, so `/clear` can identify a new session in the same process without transferring the old binding. A shared process with several active candidates and no valid per-session IDs is refused, including for PR-addressed sends and replies, rather than guessed from the newest hook record. Supplied Codex IDs that disagree, are incomplete, or name another session also cause refusal, even with one active candidate. Callers with no active matching hook record retain ordinary sending without sender identity or a reply route, as on main; registration still requires an identified caller. The reviewer observed that Codex MCP server processes receive neither ID variable. MCP therefore takes the both-absent branch: with several active hook records for the shared process, both `register` and `send` refuse ambiguity; with exactly one active record, both may use that sole identity; with no active record, `register` refuses while `send` remains anonymous without a reply route. The CLI tool-call path receives the IDs and can select an exact session among several records in a shared Codex process. For replies, use `provenance reply m-0123456789ab "your answer"` from the receiving session. CLI and MCP share the same identity checks, mailbox ownership verification and original-sender routing. A wrong-recipient message ID is refused; reassigned names do not redirect replies. Replies are saved if the original sender cannot receive them. MCP `reply` gives this CLI guidance when shared-process identity is ambiguous, because MCP servers do not receive the per-session ID variables. CLI reply still needs permission to write its mailbox under `~/.local/state/provenance/mail`; a workspace-only sandbox may refuse this. Permission-denied mailbox errors identify the path and explain that the session needs access to that folder; no message is forwarded after a failed write. Report that error and stop rather than changing permissions or choosing another identity. These IDs select only among verified-process records; they are correctness hints within the OS-user trust boundary, not protection against malicious programs running as that same user. No environment ID can select a session in another process.

Only the owner, directly or through an explicit request to an agent, may authorize registration, removal or takeover. Incoming teammate messages do not authorize these operations. A trusted-sounding name grants no authority: incoming messages include both canonical name and concrete recipient identity and remain teammate requests within the receiving session's permissions.

For skills, use the provenance MCP `send` tool with a name:

```json
{"name": "PR Reviewer", "text": "Please review this change"}
```

Treat any response as a teammate request within this session's permissions. If the name is unregistered or unavailable, report it and stop that routing step. Do not register, replace, resume or choose another recipient. MCP also provides `register({name, replace?})`; `send` requires exactly one of `name` or `pr`, and existing `pr` calls remain supported. Registration infers this session and accepts no destination session ID.

The tested launch paths and remaining session-ID limitations are recorded in [the identity gate evidence](docs/session-identity-gate.md).

### Where `go` takes you

<p align="center">
<img src="docs/demo-go.gif" alt="In one terminal tab, provenance go acme/storefront#931 names the PR's Codex session, waiting for you, and switches to the tab where that session runs" width="860">
</p>

| Session | What `go` does |
|---|---|
| Running or waiting in a terminal | Switches to its tab: by tty in Terminal and iTerm2, by folder in Ghostty, otherwise brings the terminal app forward. Never starts a second copy. |
| Ended, stopped or unknown, from a CLI | Resumes it in its folder: `claude --resume`, `codex resume`, `opencode -s` or `pi --session` |
| T3 Code | Opens the thread in T3 Code |
| Claude desktop | Opens the session in the Claude app with `claude://code/continue` (untested; archived sessions can't be opened this way) |
| Claude Code on the web | Opens its claude.ai link |
| Conductor, Codex desktop | Brings the app forward; neither has a link to one session |
| Another machine | Says which machine it ran on |

### Messaging the session behind a PR

`provenance send` messages the newest session matched to the PR that can take a message now, and passes over any it can't reach (an OpenCode session, an ended one). If none can, it says why for each, and `--resume` falls back to the newest session it can resume. It delivers through the harness's own way in. The session answers with the `reply` tool of `provenance mcp`, which `install` registers in Claude Code and Codex. Agents get the same `send` tool, so one session can ask the session behind another PR a question, and the answer comes back to it as a `[provenance] Reply`.

| Session | How the message gets in |
|---|---|
| Claude Code, running outside bypass mode | Its inbox socket, named in `~/.claude/sessions/<pid>.json`. An idle session starts a turn; a busy one reads it between tool calls. Claude Code labels it as a message from another session, which can't approve prompts or change settings. |
| Claude Code in bypass mode, hosted by T3 Code | `t3 thread send <thread> <message>`, T3 Code's own way in. An idle thread starts a turn; a busy one gets it as its next turn. The message shows in the thread like one you typed, framed as from another session. |
| Codex, running | `codex queue --thread <id>`. Queue acceptance does not confirm receipt. The session must read the queue in a turn to act on it. A running exec may exit before acting; a later resume may see the persisted message. |
| Process known to be gone | Refused, unless `--resume`: then `claude -p --resume` or `codex exec resume` runs one headless turn in its known folder. |
| Process unknown | Refused without `--resume`. With `--resume`, allowed unless last recorded as running, waiting or stopped, since it may still be open. CLI `--resume --force` overrides only that recorded-state refusal; the folder must still be known. |
| OpenCode, Pi, the web, another machine | Not supported yet. |

`--force` requires `--resume` and does not bypass other refusals or replace delivery to a known live process. MCP `send` supports `resume` with the same state policy and has no force override.

Claude Code holds provenance socket messages when the target is in `bypassPermissions` mode; T3 launches Claude Code this way. Provenance never writes to a known bypass-mode socket. When T3 Code hosts that session, it delivers through `t3 thread send` instead. It reads T3's state database read-only (`userdata/statev2.sqlite` under `T3CODE_HOME`, resolved as T3 resolves it, default `~/.t3`) for the one thread whose current provider thread names the Claude Code session as a strong `nativeThreadRef`, skipping archived and deleted threads. A thread that has since switched to another conversation doesn't count, and several threads claiming the session are refused as ambiguous. It runs `~/.t3/bin/t3` (or `t3` on `PATH`) with `--base-dir` set to the same data directory. Just before mail is written, the target's process and thread are checked again, and a route that changed is refused; T3 can't bind a send to a conversation, so a switch in the moment between that check and `t3 thread send` is not caught. Otherwise it refuses before writing message mail or opening the socket, and asks for a session not running in bypass mode or one hosted by T3 Code. It never asserts or declares a permission mode to get past that hold. A bypass-mode session acts without permission prompts, so a message delivered through T3 can make it run tools unprompted, just as a message you type there would; the framing marks it as a teammate's request, not your instruction. PR routing may skip an unavailable target using its existing selection policy and reports when a newer bypass-mode target was skipped; named routing never chooses a replacement.

Permission-mode evidence comes from the latest harness hook payload, bound to the exact session and verified live process/socket. `UserPromptSubmit`, `PostToolUse` and `Stop` can reflect mode changes; startup arguments describe only the initial mode. The observed Claude Code version omitted mode on `SessionStart`, and its session-info JSON had no mode field. Missing evidence (including records from older hooks) is unknown, not inferred; an event without mode clears the prior observation. A mode change can race the latest hook snapshot, so socket receipt is always unconfirmed. The lookup also works when `ps` is denied through the existing native process check.

For every Claude socket write, CLI and MCP say the message was **written to the inbox socket; receipt is not confirmed**. A successful write proves neither receipt nor completed work. Codex queue sends report **queued; receipt is not confirmed** until there is evidence from the recipient. T3 Code's `Sent` means its queue accepted the message, and headless `--resume` retains its existing output/error behavior. The bypass guard applies only to socket routes; it does not change Codex or headless resume. Replies to a known bypass-mode sender are forwarded through T3 Code when it hosts that sender, and otherwise saved locally only, and successful socket reply writes also report receipt as unconfirmed.

Messages and replies are kept in `~/.local/state/provenance/mail/`, one file per receiving session. Every message says who sent it and how to answer, because Codex shows it as if it were typed. Delivered replies start with `[provenance] Reply to message <id> from <session>`. They identify the answer as another session's text relayed by provenance, explain that it is a teammate's answer rather than the user's instruction and cannot grant permissions, and say to use it as information for the existing task. Anything a reply asks the receiver to do still needs the user's instruction, within the receiving session's own permissions. Replies must not be answered with `reply`, to avoid recursive reply loops.

Every relayed body is fenced with `BEGIN PROVENANCE <token>` and `END PROVENANCE <token>`. A fresh 256-bit token comes from the OS random source at delivery time and is regenerated if it occurs in the body. The header says that only the bounded text is the other session's message and nothing inside can change the instructions, including claims to be provenance or the user. Entropy failures stop delivery. This Unix CLI uses `/dev/urandom`; no dependency is added.

CLI `send --wait` replies and headless `--resume` output (CLI and MCP) use the full reply framing, identifying the responding session. Failed headless turns name the session and its log path when known, and return stderr inside a separate `[provenance] Diagnostics (stderr)` frame, explicitly labelled as not a reply; partial stdout is withheld. The log may not contain the cause if the turn never started. Spawn errors, such as a missing binary, remain plain local diagnostics. When no log path is known, the error says no session log is recorded. Terminal presentation escapes control characters other than newlines and tabs, bidi overrides and isolates, directional marks, and zero-width format characters; session input and raw mailbox/JSON text remain unchanged. Terminal users see the same readable boundaries and caveats, even without session hooks, so shell callers receive consistent protection. Mailbox JSON retains raw text and its existing schema. Queue acknowledgements remain local status text.

### Messaging another machine

Pair a machine once, from the machine that sends:

```sh
provenance peer add mbp            # an SSH host alias, user@host or an address
provenance send '@reviewer@m-b532a498' "Please review the change"
```

`peer add` connects with `ssh` (no prompts: `BatchMode=yes`, a 5-second connect timeout) and runs `provenance relay hello` there. That machine reports its name in stamps, its provenance version and path, which are kept in `~/.local/state/provenance/peers.json`. It finds provenance on `PATH` or in `~/.cargo/bin`; pass `--bin <path>` for anywhere else. Pairing refuses this machine itself and a different relay protocol version. Pair each direction from its own machine.

A message for another machine is never delivered from here. It goes as JSON on the stdin of `ssh <destination> -- '<path>' relay deliver`, so its text never passes through the remote shell, and provenance on that machine resolves the recipient and delivers it with its own rules: its liveness checks, the bypass-mode hold, the T3 Code route and its mailbox. Its answer, delivery or refusal, comes back word for word. It never relays further.

- `@name@machine` names a session in that machine's own registry; names stay machine-local, and a bare `@name` never falls back to another machine. `machine` is the paired machine's name in stamps or its SSH destination.
- `send <PR>` follows a commit stamp: a matched session stamped on a paired machine is planned by that machine with a dry run, so one that can't take the message is passed over with that machine's reason, like a local one.
- `--resume` is passed on and decided by that machine. `--force` is never relayed.
- A machine that is unpaired, offline or refuses SSH is reported; nothing is queued.

The sender keeps its own copy of the message, marked with the machine it went to.

A reply goes back the same way, if the receiving machine has the sending machine paired too: `reply` saves it on the receiving machine, then runs `provenance relay reply` on the sending machine over SSH. That machine accepts it only for a message it relayed to that machine, saves it next to its copy, and passes it to the session that sent it, or leaves it for the terminal that sent it, where `send --wait` picks it up. Without the reverse pairing, or when that machine is offline, the reply stays saved where it was written, and `reply` says so.

When the receiving machine can't reach the sending one at all (for example, SSH is off on the sending machine), the sending machine collects the reply instead. `provenance peer pull` asks each paired machine, over the same SSH, for replies to messages relayed there in the last 24 hours that are still unanswered here; a machine with nothing awaited isn't contacted. `provenance relay collect` there returns only replies to messages that came from the asking machine, and each one is accepted here exactly as a passed-back reply would be. A reply that arrives both ways is delivered once. If it can't be passed to the session that sent the message (its inbox fails, say), it stays saved and the next `peer pull` tries again, without asking the other machine; one whose session has ended isn't retried. `send --wait` asks about its own message every few seconds while it waits. On macOS, `peer add` also loads a launchd agent (`io.github.singular-machines.provenance.pull`, in `~/Library/LaunchAgents`) that runs `peer pull --quiet` every 30 seconds with the `PATH` of the shell that ran `peer add` (so it finds `codex` and `t3`), logging collected replies to `~/.local/state/provenance/pull.log`; removing the last peer or `provenance uninstall` removes it. Elsewhere, run `provenance peer pull` from cron or a timer. Messages still go only from a machine that can reach the other: a machine that can't reach this one can't start a conversation with it.

SSH is the trust boundary: a paired machine can message any session there that a local process could, including a bypass-mode session through T3 Code, which can make it run tools without asking. The sending machine's account of who sent the message is trusted the way a local caller's is, and the message is labelled with that machine's name.

## Reading the table

PRs are grouped by repository, live sessions first. Each row reads: status, PR, the PR’s relative opening age, title, app, harness and session ID, then when the session was last active. The opening age uses GitHub’s creation time and the same compact seconds/minutes/hours/days format as session activity (for example, `0s`, `5h`, or `3d`).

```text
acme/storefront
  ● #927  2d  docs: Release 3.1  T3 Code  Claude 7157cbf5    0s
    #941  5h  Bump deps          — no session found
```

With `--json`, each row’s `created_at` contains the raw UTC timestamp (for example, `"2026-10-28T12:00:00Z"`). Older PR caches without creation times are fetched again automatically.

| Mark | Meaning |
|---|---|
| `●` green | Running: working on a turn, and its process is up |
| `◐` yellow | Waiting for you: finished its turn |
| `✕` red | Stopped: was mid-turn when its process disappeared |
| `○` | Ended: the session was closed |
| dimmed row | No live session |
| `~` | Matched by branch, not by a commit stamp |

Status marks need [`provenance install`](#install-hooks) for Claude Code, Codex and Pi. OpenCode sessions get them from OpenCode's own database, with nothing installed: an idle OpenCode session shows as waiting while OpenCode is running, and one left busy shows as stopped once no OpenCode process runs. OpenCode sessions never show as running, because a surviving OpenCode service can't tell provenance whether that session's turn is still going. In a terminal, PR numbers are links to the PRs, and web session IDs link to the sessions. Merged and closed PRs are tagged `[merged]` or `[closed]`, and `--json` gives each PR's `state`. Titles use the terminal's width. Piped output is plain text, and `NO_COLOR` turns colour off.

## Timeline

`provenance recap` answers "what did my agents do today, and what's waiting on me?". It lists every open PR, plus every PR merged or closed in the window, grouped by what they need:

```text
Today (Wed 11 Nov) · 6 PRs: 3 merged, 1 closed, 2 open · 1 needs you

NEEDS YOU
  14:02  ● opened                         api#212  Add retry to webhook sender
         Claude Code (Opus 5.5) · T3 Code · waiting for you

DONE TODAY
  13:40  ✓ Tue 16:00 → merged (21h40m)    provenance#48  Respect Claude socket holds
         Claude Code (Opus 5.5) · T3 Code

STILL OPEN
  12:20  ● opened                         web#901  Migrate checkout to new API
         Codex (GPT-6.1 Sol) · CLI · running
```

| Group | What's in it | Listed |
|---|---|---|
| Needs you | Open, and its session is waiting for you or stopped mid-turn. The session that asked most recently comes first. Asks older than `--stale-days` don't count as needing you: the summary calls them older asks | All but older asks |
| Done | Merged or closed in the window, newest first | Newest 8 |
| Still open | Open, with activity in the last `--stale-days` (default 3), most recently active first | 8 |
| Gone quiet | Open, with no activity for `--stale-days`: no push, comment or review on GitHub, and no session activity | 5 |

`--all` lists every PR in every group, older asks included.

Each row names the session's harness and model, read from its log (Claude Code's last main-thread reply, Codex's last turn). OpenCode and Pi sessions, and sessions known only from a stamp, show the harness alone. A PR with no session found is most likely your own. Times are local; a window starts at local midnight.

`--html` writes one self-contained page, with no server and nothing loaded from the network: a row per PR on a time axis that ends at now, with nights shaded in the multi-day view. A hollow circle marks when the PR was opened and a bar runs to its last activity, shown as a dot. Dots then run on to now while nothing happens; only a PR whose agent is running now has a bar all the way to now. A ✓ or ✗ disc marks a merge or close. A left arrow means the PR was opened before the chart starts. A column after the now line gives the time since each PR's last activity, in amber after 3 hours. Above the chart, one strip sized by count shows every PR in view as merged, closed, needing you, older asks, moving, stalled, idle or gone quiet, one part per section, with a key underneath. When the PRs span more than one repository, each PR reference starts with a coloured square that's the same for that repository in every run. Switch between today and the whole window, and between one row per PR and one group per session. Needs you, Done and Gone quiet match the terminal. Open PRs split into Moving (agent running, or activity in the last hour), Stalled (no activity for an hour or more) and Idle (no activity for a day or more). Long sections are cut short behind a "Show N more" button. Click a row to open a panel under it with its details, the PR link, and copy buttons for `provenance go` and `gh pr close`. For a PR that needs you, the panel shows the session's last message (Claude Code and Codex), so you can see what it's asking. Asks older than `--stale-days` don't count towards "need you": they fold into one line in Needs you, and show under a note when you open it. With only older asks, the summary says nothing new needs you. PRs gone quiet are hidden until you show them. The page is a snapshot, so run `provenance recap` again for live status. Opened an hour or more after it was made, it says how old it is and drops "Today" from the title once the day has passed.

## How it works

`provenance prs` lists your open PRs through `gh` (or merged or closed ones with `--state`) and links each one to sessions in three ways, strongest first. The table marks `repo` and `path` matches with `~`; `--json` gives the `kind` of every match.

| Match | How |
|---|---|
| `stamp` | A commit in the PR names the session: `Agent-Session: <harness>/<id>` (written by provenance's git hook) or `Claude-Session: https://claude.ai/code/session_…` (written by Claude Code on the web). |
| `repo` | The session was on the PR's head branch, in a folder whose GitHub remotes (or the repository Codex logged) include the repository that branch lives in: the fork for a fork PR, otherwise the PR's repository. |
| `path` | Same, but the folder has since been deleted and its path names that repository, as in `…/conductor/workspaces/<repo>/<workspace>`. |

Branch matches come from the harnesses' own session logs, so they work with nothing installed:

| Harness | Logs | What it records |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` | `gitBranch` and `cwd` on every message; `entrypoint` (`cli`, `claude-desktop`, `sdk-ts`) |
| Codex | `~/.codex/sessions/**/*.jsonl` | `git.branch` and `git.repository_url` once, when the session starts; `originator` (`T3 Code`, `Codex Desktop`, `codex-tui`, …) |
| OpenCode | `~/.local/share/opencode/opencode.db` (read with `sqlite3`, read-only) | The session's folder and its busy and idle times, but no git branch. A session is given the branch its folder has checked out now, and only if it's the most recent session in that folder by any harness. |
| Pi | `~/.pi/agent/sessions/**/*.jsonl` | The session's folder, in the first line, but no git branch. A session is given the branch its folder has checked out now, and only if it's the most recent session in that folder by any harness. |

- PRs whose head branch is a repository's configured default branch, `main`, `master` or the PR's own base are never branch-matched, because many unrelated sessions share those branches. Stamps still count.
- The time at the end of each row is the session's last logged event, or the stamped commit's time for sessions not on this machine.
- Apps built on the agent SDKs report a generic entrypoint, so they're named from their worktree folder: `.t3/worktrees` → T3 Code, `conductor/workspaces` → Conductor.
- Sessions known only from a stamp show where they ran: `web`, or `on <machine>`.

## Install hooks

`provenance install` is optional. It makes matches exact and adds live status. OpenCode needs only the git hook: its live status comes from its database. For Pi, which passes no session ID to the commands it runs and has no hook settings, `install` writes a small extension to `~/.pi/agent/extensions/provenance.ts` (only if `~/.pi/agent` exists). It sets `PI_SESSION_ID` for commit stamps and reports session events like the other harnesses' hooks. `provenance uninstall` removes the hooks it added; it leaves the settings backups, your machine name, the live-session records and the caches.

> [!TIP]
> Codex asks you to review new hooks before it runs them. Approve the provenance hooks when it asks.

<details>
<summary><b>What <code>install</code> changes</b></summary>

1. **A git hook that stamps agent commits.** It adds `hook.provenance` to your global git config: a config-defined `prepare-commit-msg` hook, which git runs alongside each repository's own hooks (including repositories with their own `core.hooksPath`), so nothing is displaced. When a commit is made inside a Claude Code, Codex, OpenCode or Pi session (`CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`, `OPENCODE_SESSION_ID` or `PI_SESSION_ID` is set), it adds `Agent-Session` and `Agent-Machine` trailers. Commits you make yourself are left alone. When one harness runs inside another's shell and both variables are set, the stamp names the harness whose process is nearest up the process tree, not the outer one. Needs a git that supports hooks defined in config (`hook.<name>.command`, as in git 2.56); `install` checks and refuses otherwise.
2. **The `provenance mcp` server in Claude Code and Codex**, registered with `claude mcp add --scope user` and `codex mcp add`, so each session gets the `send` and `reply` tools. `uninstall` removes it with `claude mcp remove` and `codex mcp remove`.
3. **Session hooks for Claude Code and Codex** (`SessionStart`, `UserPromptSubmit`, `PostToolUse`, `Stop`, `SessionEnd`) in `~/.claude/settings.json` and `~/.codex/hooks.json`, appended after your existing hooks; a backup is written first and the file keeps its permissions. `uninstall` removes only provenance's own commands. In Codex, which keys hook approvals by position, a removed group that has later groups after it keeps a no-op `true` command so those later hooks keep their approvals. Each event updates `~/.local/state/provenance/live/<harness>-<id>.json`: the harness process ID and its start time, the last event, and every folder and branch the session has been on. That gives the status marks described in [Reading the table](#reading-the-table).

</details>

### Without commit stamps

The git hook is the only part of `install` that writes to git history. To keep live status, `go` and `send` without stamping, remove it after installing:

```sh
provenance install
git config --global --remove-section hook.provenance
```

PRs are then matched by branch only (marked `~`), with the [Known gaps](#known-gaps) that stamps would cover. Running `install` again adds the hook back. `install` still needs a git that runs hooks defined in config, even if you remove the hook afterwards.

## Privacy

> [!NOTE]
> Stamps never contain your host name. Each machine gets a random ID such as `m-7f3a9c2e`, created on first use and kept in the global git setting `provenance.machine`.

- `provenance machine <alias>` sets a name of your choice (letters, digits, `.`, `-`, `_`). `provenance machine` shows the current one.
- `PROVENANCE_MACHINE` overrides it for one shell.
- Stamps are part of commit history. On public repositories they publish the session ID and this machine name. The git hook is global, so it applies to every repository you commit to. To install without it, see [Without commit stamps](#without-commit-stamps).
- Everything else stays on your machine: session logs are read locally, and nothing is sent anywhere except `gh` queries to GitHub and the update check below.

### Update notice

Once a day, `prs`, `go`, `names`, `install`, `uninstall` and `machine` start a background `git ls-remote --tags` against this repository. When a newer release is out, the next of those commands prints one line on stderr with the command to update (`cargo install --git https://github.com/singular-machines/provenance --force`). The answer is cached in `~/.cache/provenance/`, so no command waits on the network. Nothing is checked or printed when stderr isn't a terminal, when `CI` is set, or in hooks, the MCP server, `send` and `reply`. Set `PROVENANCE_NO_UPDATE_CHECK=1` to turn it off.

Releases are cut by bumping `version` in `Cargo.toml`: when that lands on `main`, the Release workflow tags it and publishes a GitHub release.

## Speed

Caches live in `~/.cache/provenance/`, and deleting that folder is always safe.

| Cache | Kept until |
|---|---|
| PR list | 2 minutes; the footer says how old it is, and `--refresh` skips it |
| Commit stamps | The PR's head or base moves |
| Session logs | The log file's size or modified time changes |
| Newest release | 24 hours |

## Known gaps

- **Unstamped sessions on other machines.** A session that ran elsewhere is found only through its commit stamps, and shows as `web` or `on <machine>` with the stamped commit's time but no live status. Without a stamp it shows "no session found".
- **Without `install`:** a Codex session that starts on `main` and then creates the PR branch isn't matched (Codex logs its branch only at session start), and nor is a session that works in another repository by changing directory. Stamps and session hooks cover both.
- **A repository's own `prepare-commit-msg` runs after provenance's.** One that rewrites the whole message would drop the stamp.
- **Desktop apps start a new harness process per turn** (Conductor does; others may), so between turns a waiting session has no process of its own. The status relies on the last event for that reason.
- **Stamps only cover commits made after install.**
- **`go` and open OpenCode sessions.** OpenCode doesn't say which window holds a session, so `go` prints the resume command instead of switching to it.
- **`go` in Ghostty** picks the terminal by folder, so with several terminals in one folder it picks the first.
- **The Claude Code inbox socket's message format isn't documented.** `send` writes the line Claude Code's own debug hint shows, so a Claude Code update could change it.
- **The T3 Code route reads T3's internal database.** The thread lookup depends on T3's undocumented state schema, so a T3 update could break it; provenance then falls back to refusing.
- **`send` reaches only CLI sessions that keep a process.** A Claude Code session whose process has moved to another session (`/clear`, `/resume`) is refused, and desktop apps that start a process per turn have no socket between turns.
- **Codex may ask you to approve the provenance tools** the first time a session calls them.
- **OpenCode and Pi record no git branch.** Without a stamp (or, for Pi, the extension's live records), a session is matched only on the branch its folder has checked out now, and only if it's the most recent session in that folder by any harness.

## Roadmap

- [x] `provenance prs` from existing session logs
- [x] Commit stamps (including Claude Code on the web's) and live session status
- [x] `provenance go`: switch to or reopen the session behind a PR
- [x] `provenance send` and an MCP server with `send` and `reply` tools (Claude Code and Codex)
- [ ] `send` to OpenCode and Pi sessions
- [x] `send` to sessions on paired machines, over SSH
- [x] Replies back across machines
- [x] `provenance recap`: a timeline of your PRs and what needs you
- [ ] `provenance show`: a session's recent turns

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

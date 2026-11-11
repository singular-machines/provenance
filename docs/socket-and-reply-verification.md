# Claude socket guard and CLI reply verification

Built from a new branch based on merged main `b1d0e968`, on macOS. The binary was invoked by full path from this worktree's `target/debug/provenance`; nothing was installed. Real sessions were newly created throwaways outside the repo. Existing sessions, credentials and owner binary were not modified; no credentials were copied. Stopping/cleanup was scoped to the exact processes and mailbox files created by these tests.

## Permission-mode evidence

Claude Code 2.1.311, Haiku/low, two independent print/stream-json sessions: one explicitly launched in `bypassPermissions`, one in `default`. An additional local hook captured only event name, session ID and permission mode, then ran the built binary's `hook claude-code` command with isolated state HOME (the harness retained its existing login). `SessionStart` omitted mode; `UserPromptSubmit` and `Stop` carried the actual mode. The two targets' session-info JSON files had no permission-mode field. Only those new targets' session-info files were copied into isolated test state to resolve their real sockets; no credentials or existing session files were copied.

The implementation therefore uses the latest hook observation, matched to the exact verified live process, session and socket. Process command-line flags describe startup configuration and can become stale after a mode change; session-info did not supply mode. The hook observation does not depend on a `ps` command-line read; process liveness retains the native fallback when `ps` is denied. Missing mode clears the previous observation. There is inevitably a window between a mode change and a new hook event, so every socket write reports receipt as unconfirmed. Older hook records without mode are unknown. No permission mode is asserted, spoofed or declared in the socket message.

Actual targets:

- Bypass: session `1a4e689b-6dfc-4165-bd9a-1d6b888e2422`, PID 23496, latest hook mode `bypassPermissions`.
- Default: session `4159ed3b-df4b-4809-87a9-b3c5076f7e0b`, PID 23497, latest hook mode `default`.

Real verification used the built binary with isolated state:

- CLI named send and fixture-backed PR send to the bypass target: exit 1, `Claude Code holds socket messages ... nothing was delivered. Use a session not running in bypass mode`.
- MCP named send and fixture-backed PR send to the same bypass target: `isError: true` with the same refusal. No mail directory existed after all four refusal attempts. PR metadata came from a fake `gh`, not a real PR or GitHub request.
- CLI named send to the real default target: exit 0, `Message ... was written to the inbox socket ... Receipt is not confirmed.` The write is verified; receipt/completed recipient work is not claimed.

Codex queue and headless resume are outside this socket-only guard. Unit tests cover bypass refusal before mail for PR/named routes and the final pre-write recheck; missing/default/auto observations; mode changes and old records; honest CLI/MCP text; and the unchanged peer JSON shape with no permission-mode assertion. Reply forwarding uses the same socket refusal/receipt policy, while preserving the local saved answer.

## Shared-process CLI reply

A fresh independent `codex app-server --stdio` ran two concurrent GPT-6 Luna/low sessions, with workspace-write sandbox and no attachment to an existing T3 server. Each new session received a synthetic message only in its own newly created mailbox, with no agent sender (the verification terminal). Both were asked to invoke the built binary by its full path with `reply <own-message-id> "verification"` once and stop if blocked.

- Shared app-server PID: 34941.
- Session A: `01a1c7fd-5271-7283-bc7f-e625f1efcfd0`.
- Session B: `01a1c7fd-6686-7c33-956f-c51f73993807`.
- Both commands selected the correct session/mailbox and passed the ownership lookup; both then failed at writing their own mailbox with `Operation not permitted`. No reply was persisted. The workspace sandbox did not permit writes under `~/.local/state/provenance/mail`.

**Real reply persistence is NOT VERIFIED.** No writable roots, permission overrides, alternate paths or launch-mode changes were introduced after that block. No owner-hook/trust request was approved or bypassed. An earlier prompt typo included an extra `.` argument and failed parsing; it was corrected before the two executions above, without a hook/trust block.

The isolated fake-transport regression does complete CLI reply with two records in one process and agreeing ID variables, routes to the original sender through fake `codex queue`, refuses a wrong recipient, saves when the sender is unavailable, and confirms MCP ambiguity errors provide the CLI alternative. CLI and MCP call the same reply implementation. CLI is subject to the receiving session's filesystem permissions; it adds no authority.

The app-server was closed and, if needed, stopped by its exact PID. Only the newly seeded mailbox files were removed after their contents were saved as evidence. Raw scripts, hook observations, stdout and protocol logs remain in the local scratch directory referenced by the implementation handoff. No real owner session received a provenance message.

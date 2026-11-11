# Codex exec queue and non-git resume

Investigated against base 90fbac8 and installed Codex CLI 0.167.0, using this worktree's target/debug/provenance. No binary was installed and no owner session was targeted.

## Root cause evidence

Provenance's CodexQueue route runs `codex queue --thread ID --message TEXT` and previously interpreted successful command exit as “Sent”. That acknowledgment confirms queue acceptance, not recipient processing.

Version-pinned upstream sources:

- https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/tui/src/session_queue_commands.rs : `run_session_queue_command` discovers an app server and submits `thread/queue/add`; it returns the queued submission ID. Its optional `--resume` is server-side resume, not `exec resume`.
- https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/exec/src/lib.rs : exec starts `InProcessAppServerClient` (line 1029), resolves resume through that client, submits the explicit prompt with `turn/start`, and shuts down on its completed turn (lines 1388-1399). This frontend lifecycle does not establish whether core reads the queue. The earlier inference that an exec target lacks a queue reader was incorrect.

Owner-reported live evidence at head `4c2639231ad140cb66961a2ebec61470cb722b9f` corrects that inference. Session `01a1cbea-28bf-70d3-af1d-0d24f5640b91` received a send at 18:41:56 while `sleep 100` ran. At 18:43:10.157, immediately after `task_complete`, core started a `turn_trigger: queue` turn and recorded the full provenance message with nonce `NONCE-A-7731` as a user message. That turn was aborted as interrupted 31 ms later as exec exited. A later `codex exec resume` quoted the message including its nonce; the probe asked it to quote any received message. This was one run, not a guarantee for every send or resume.

The message can be queued and persisted, and exec's core can drain it when the running turn ends. A running exec may exit before acting on it, and a later resume may see it. Queue acceptance alone proves neither recipient processing nor completed work.

A throwaway in TMPDIR, session 01a1cbe6-cc31-76c2-9e75-80181d374118 (started PID 19328), used GPT-6 Luna/low. `codex queue` accepted submission 01a1cbe6-eb7d-7152-91e4-53cb44144f22 while that process was live. Its captured exec stdout JSONL ended after the original turn without an additional user turn; this alone does not establish what core recorded or persisted. Registration `zz-exec-19326` failed with `Operation not permitted`, so the intended named-send test is **NOT VERIFIED**. No sandbox settings, hooks or paths were changed to overcome that failure.

The reproduction script mistakenly continued to a resume probe after the registration block. That was a test-procedure error. Its prompt itself contained the marker, so its answer cannot establish queued-message consumption. That original probe did not verify later exec resume consumption; the separate owner-reported run above supplies evidence for one later resume. Live testing stopped; the unregister cleanup was attempted (no binding had been created). Raw logs are in the local TMPDIR scratch `zz-prov59-t0y0dtb7`. An earlier detached launch exited without any JSONL; an additional hello-only exec supplied no delivery evidence. No owner session was messaged or resumed.

## Behaviour

CLI and MCP sends, plus forwarded replies, now describe queue acceptance with receipt unconfirmed. They do not promise immediate, between-turn, or later-resume delivery. App-server queue transport remains available.

For #58, headless resume previously wrote mail before Codex rejected a non-git working directory. Provenance does not record whether the original invocation used `--skip-git-repo-check`. It therefore refuses such resumes before mailbox persistence and transport, rather than silently disabling that check. Git discovery failures also refuse. No permission mode is asserted or changed. The check is shared by PR and named CLI/MCP sends; `--force` does not bypass it. A repository deleted between preflight and execution can still cause a later transport failure.

Regression tests cover CLI/MCP queue wording, pre-mailbox refusal without transport, and the existing git-directory headless path. Real non-git named resume is **NOT VERIFIED** because registration was blocked; cross-harness review approved the original head, and the separate live evidence above prompted these corrections.

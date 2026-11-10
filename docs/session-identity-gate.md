# Session identity gate (macOS, 2026-11-09/10)

Base tested: `b819e67`. Harness versions: Claude Code 2.1.311 and Codex CLI 0.167.0. All agent-driven tests used newly created throwaway sessions in a scratch directory outside the repo, existing login and installed hooks, Haiku/low or GPT-6 Luna/low. No credentials were copied, existing sessions resumed/messaged, owner binary replaced, or broad process kill used. Interactive processes were stopped by their exact recorded PID after their turns finished. Independent stdio app-servers were closed and cleaned up by their own PID only.

## Cause and fix

`nearest_harness()` walks ancestors using PID, executable name and kernel start time. `current_session()` then matches hook records for that exact harness process. In sandboxed `codex exec`, `/bin/ps` returned `operation not permitted`; the original detector returned `ancestor=None` and `sender=None`. A native probe successfully used `sysctl`, `proc_pidpath` and `proc_pidinfo` from the same sandbox. Its actual tree was probe → throwaway Codex → outer Codex → T3 → T3 launcher → init. The precise Seatbelt rule and other OS/version combinations were not investigated.

The macOS fallback uses native process queries for the same identity evidence, formatting kernel start time identically to existing `ps` records. Liveness queries use that fallback too. No folder or newest-session inference is used.

The owner's two real T3 Codex probes shared PID 49769/start time and had distinct, agreeing `CODEX_THREAD_ID` and `CODEX_SESSION_ID`. Read-only inspection confirmed both hook records existed and matched that process. Their `sender=None` was the ambiguity refusal, not missing hooks. T3 Claude probes had distinct processes and worked (owner-reported evidence).

For multiple Codex records under one verified ancestor, agreeing ID variables may select only a record matching that same PID/start time. Missing, disagreeing or unmatched values refuse identity. A same-user program can set environment variables and edit hook records; this is a correctness mechanism within the OS-user trust boundary, not an authentication boundary against that user. Tests check foreign IDs, disagreement, missing variables, wrong harness/PID/start time and ambiguous records. Shared-process callers without valid IDs now receive an explicit refusal from send/reply instead of the previous newest-record guess.

## Real-session results

| Launch path | First turn | Resume in a new process |
|---|---|---|
| Claude Code print CLI | Detected exact ID | Same ID and correct detection |
| Codex exec | Detected exact ID after native fix | Same ID and correct detection |
| Interactive Codex in PTY | Detected exact ID after native fix | Same ID and correct detection |
| Independent stdio app-server, one session | Detected exact ID | Not tested |
| Independent stdio app-server, two simultaneous turns/sessions | Each selected its own exact ID with the same ancestor PID/start time | Not tested |

The first concurrent gate had session IDs `01a1c713-ab81-73b2-8f4e-5f7553de1517` and `01a1c713-abee-70f3-adba-4cb9d3601f69`, both ancestor PID 59182, start `Tue Nov 10 20:08:37 2026`. Both diagnostic outputs showed agreeing per-session variables and the correct sender. The independent server was not attached to T3's existing process. Exact T3 app configuration was not reproduced; real T3 observations above were supplied by the owner. Single-process launch checks preceded the ambiguous-record selector change, whose single-record behaviour is unchanged.

A final repeat including liveness checks used session IDs `01a1c725-d3c9-70d0-bb05-143f6073c81a` and `01a1c725-d42e-7ee3-a156-ccdc3f2546d8`, both ancestor PID 86189, start `Tue Nov 10 20:28:27 2026`. Each selected its own ID and reported `alive=true`. The subsequent rebase onto main `6ab9aa4` preserved this detector logic and integrated main's newer resume guards and fenced relay output.

Compaction and same-process interactive resume remain **UNVERIFIED**; neither was forced with a large token budget. No claim is made that every fork, `/clear` or harness release preserves IDs. Bindings never transfer to a new ID.

## Reproduction commands

The scratch probe copies repo sources and adds only an early diagnostic branch to main: `PROV_GATE_PROBE=1` prints `live::nearest_harness()` and `current_session(&live::load_records(home))`. The concurrent version also prints only the two relevant ID variables. No bare `env` was used or secret-exposure hook bypassed.

- `claude -p --model haiku --effort low --max-turns 2 --allowedTools Bash --output-format json '<run probe and report output>'`; repeat with `--resume <exact throwaway ID>`.
- `codex exec --skip-git-repo-check -m gpt-6-luna -c model_reasoning_effort='"low"' --json '<run probe and report output>'`; repeat with `codex exec resume` and the exact throwaway ID.
- PTY: `codex -m gpt-6-luna -c model_reasoning_effort='"low"' --no-alt-screen '<run probe and report output>'`; new-process `codex resume <exact throwaway ID>` with the same options.
- `codex app-server --stdio`: initialize a fresh server, `thread/start` twice with model `gpt-6-luna`, sandbox `workspace-write`, approval policy `never`; issue two `turn/start` requests before waiting, each with effort `low` and a request to execute `sleep 2; PROV_GATE_PROBE=1 <scratch>/target/debug/provenance`. Observe both command-completion outputs and close only this server.

Full raw JSONL, probe sources, scripts and previous gate reports remain in the local scratch directory referenced in the implementation handoff. Feature transport tests use fake deliveries; live naming/message transport acceptance is not claimed by these detector gates.

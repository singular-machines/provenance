# auto-review extension for provenance

Install at `.claude/rules/auto-review.md`. Loaded by the generic cts-brain skill `auto-review` at start.

## Overrides

- The skill describes itself as for CTS repos. This repo uses its format, verdict line and labels anyway: first line `Verdict: approve` or `Verdict: changes needed`, the machine-readable verdict line, and the labels `code-review:clean`, `code-review:needs-fix` and `code-review:resolved`.

## Extra checks

Run all three. A failure is a finding.

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

## Core flows

Defects here raise severity (identity and delivery: a message reaching the wrong session is a High):

- `src/send.rs`: delivery to a session, mailbox and reply matching.
- `src/live.rs`: live session records written by harness hooks.
- `src/names.rs`: machine-local exact session bindings; folder hints must never select delivery.

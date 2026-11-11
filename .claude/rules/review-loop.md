# Review format and local checks

This repository uses the auto-review skill's review format even though that
skill describes itself as for CTS repositories. Follow its verdict line format
with first line `Verdict: approve` or `Verdict: changes needed`, and use verdict labels: `code-review:clean`, `code-review:needs-fix`, and
`code-review:resolved`.

Run these local checks:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

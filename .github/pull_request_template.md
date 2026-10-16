## What and why

<!-- One or two sentences. Link the issue it fixes, if there is one. -->

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass locally
- [ ] New behaviour has tests; fixed bugs have a regression test named after them
- [ ] Docs that describe the changed behaviour are updated (README, docs/guide.md, docs/research)
- [ ] Screens checked at 80×24 and 160×45 if the UI changed
- [ ] Golden snapshots re-blessed on purpose, with the snapshot diff in this PR (if the UI changed)

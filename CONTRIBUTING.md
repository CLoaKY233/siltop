# Contributing to siltop

Thanks for helping. Bug reports, chip reports and small focused pull requests are all welcome.

## Reporting a bug

Open an issue with the **Bug report** template and paste the output of `siltop doctor`.
It lists your chip, macOS build, which sensors this Mac exposes and what siltop costs, and
contains no personal data. A chip or Mac model we have not seen yet is especially useful.

## Building

You need macOS on Apple Silicon for real readings. Other platforms compile (the collectors
are stubbed out) but sample nothing. The toolchain is pinned in `rust-toolchain.toml`;
`rustup` installs it on first build.

```sh
cargo run -p siltop            # the dashboard
cargo run -p siltop -- --demo  # a canned 60 s session, no hardware needed
```

## Before you open a pull request

Run these in order; CI runs the same checks on macOS and Linux:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # plain `cargo test` only covers the binary crate
```

- Production code never uses `unwrap`, `expect`, `panic!` or `process::exit` (clippy denies them); return a `Result` instead. Tests may opt back in.
- Every `unsafe` block carries a `// SAFETY:` comment.
- Keep the crate boundaries in [crates/ARCHITECTURE.md](crates/ARCHITECTURE.md): `siltop-metrics` never depends on the UI, and `siltop-ui` never imports the metrics crate.
- Tests live next to the code in `#[cfg(test)] mod tests`. Tests that touch real hardware must pass on CI machines, which have none: assert invariants, not that a sensor exists.
- **Golden screens.** `crates/siltop-ui/src/widgets/golden.rs` paints the whole app from a fixed fixture into insta snapshots. Any change that moves a visible cell fails them. Re-bless on purpose (`cargo insta review`, or `INSTA_UPDATE=always cargo test -p siltop-ui golden`) and include the snapshot diff in your PR.
- Changes to the binary or metrics crates should include before/after `scripts/perf-smoke.py` medians (see [docs/research/12-self-cost.md](docs/research/12-self-cost.md)). A monitor that burns a core is lying about the machine it watches.

## Licensing of contributions

siltop is dual-licensed under MIT OR Apache-2.0. Unless you say otherwise, any contribution
you submit for inclusion is licensed the same way, without additional terms or conditions.

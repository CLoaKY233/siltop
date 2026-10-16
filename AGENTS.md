# AGENTS.md

Apple Silicon system monitor TUI (`siltop`). macOS-first: metrics collectors use raw FFI (IOKit SMC, IOHIDEventSystemClient, sysctl/host APIs) behind `#[cfg(target_os = "macos")]`; other targets compile against stubs but sample nothing. Edition 2024. Toolchain pinned in `rust-toolchain.toml` (Rust 1.98.1); MSRV = 1.98. Open source under MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`); `publish = false` on every crate.

## Commands

```sh
cargo fmt --all                      # required step; default rustfmt settings (no rustfmt.toml)
cargo clippy --workspace --all-targets   # must be clean
cargo test --workspace               # NOT plain `cargo test`: default-members is the bin crate only
cargo run -p siltop
```

Single test example: `cargo test -p siltop-metrics fan::tests::smc_struct_is_80_bytes`

Order matters: fmt → clippy → test. CI (`.github/workflows/ci.yml`) runs these plus rustdoc on macOS + Linux, clippy with `-D warnings`, on every push to master, every pull request, manual dispatch and release tags. Tests that need real hardware (SMC, IOReport, battery) must degrade gracefully on CI VMs, which have none: assert on invariants, not on sensors being present. All tests are inline `#[cfg(test)] mod tests` next to the code — there are no `tests/` directories (insta keeps its `snapshots/` folder next to the source).

Golden screens: `crates/siltop-ui/src/widgets/golden.rs` paints the whole app from a fixed M5 Pro fixture (no clock, no hardware) into insta snapshots in `crates/siltop-ui/src/widgets/snapshots/`. Any change that moves a visible cell fails them. Re-bless on purpose and show the snapshot diff in the PR; CI never writes snapshots.

```sh
cargo test -p siltop-ui golden                            # just the goldens
cargo insta review                                           # accept/reject diffs (cargo install cargo-insta)
INSTA_UPDATE=always cargo test -p siltop-ui golden        # re-bless without cargo-insta
```

Live checks drive the real binary in a pty (`python3 -m pip install pyte`; Pillow optional, for PNGs):

```sh
cargo build --release -p siltop
python3 scripts/perf-smoke.py --runs 3             # self-cost JSON for target/release/siltop at 160x45
python3 scripts/perf-smoke.py BIN --interval-ms 1000 --assert 'cpu_pct<=1.0'   # exits 1 on a failed gate
python3 scripts/pty-shot.py BIN 120 40 /tmp/shots/work wait:2 shot:home key:g shot:glance
```

Any change to the bin or metrics crates puts before/after perf-smoke medians in its PR. Method, baseline and competitor numbers: `docs/research/12-self-cost.md`.

## Hard workspace rules

- Dependency direction (`crates/ARCHITECTURE.md`): core ← {metrics, ui} ← bin.
  - `siltop-metrics` must never depend on ratatui or `siltop-ui`.
  - `siltop-ui` must never import `siltop-metrics`.
- Clippy lints (root `Cargo.toml` `[workspace.lints]`) deny `unwrap`, `expect`, `panic`, `dbg!`, and `process::exit`; pedantic is warn. Production code returns `Result` instead. Test modules opt back in with `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- `unsafe_op_in_unsafe_fn` is denied; every `unsafe` block carries a SAFETY comment. Keep that convention for new FFI.
- New deps go in `[workspace.dependencies]` and are referenced as `name.workspace = true`.

## Entrypoints

- Binary/event loop: `crates/siltop/src/{main,app,event,tui,worker}.rs` (worker thread owns the Sampler; the App drains snapshots over mpsc and owns all UI state).
- Sampling: `Sampler::tick()` runs on the worker thread and produces one `Snapshot` per interval; `App::apply_snapshot` pushes histories; UI widgets consume snapshots read-only.
- Per-collector modules live flat under `crates/siltop-metrics/src/` (cpu, gpu, fan, thermal, memory, net, disk, process, hid, soc, zones, topology).

## Docs are living reference

`docs/research/*.md` is a curated knowledge base, not stale scratch — update it when we learn something new about an API, widget, or chip generation (`docs/README.md`). Read `crates/ARCHITECTURE.md` before touching crate boundaries.

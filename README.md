# Voltra Studio

Compositing, recording and live streaming, written in Rust.

The reference is OBS Studio — its architecture is proven and this project takes
from it deliberately rather than reinventing it (see
[`docs/references/obs-studio.md`](docs/references/obs-studio.md)). What Rust
changes is the guarantees: a source that misbehaves is contained instead of
taking the broadcast down, the render and audio paths are free of locks and
per-frame allocations by construction, and there is no C ABI for plugins to
corrupt.

> **Status: fase 0.** The workspace skeleton and the quality gate are in place.
> There is no compositing, capture or encoding yet. See
> [`docs/ROADMAP.md`](docs/ROADMAP.md).

## Build

```bash
cargo build --workspace
cargo run -p voltra-cli -- info
```

Default features build and test on a headless machine with no GPU and no
capture devices — that is a hard rule, not a coincidence. Everything native
(GPU, FFmpeg, PipeWire, the UI) sits behind feature flags.

## Quality gate

Every commit must pass, in this order:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three commands, plus a release build, an MSRV check and
`cargo deny` for licences and advisories.

## Layout

| Crate | Role |
|---|---|
| `voltra-core` | Shared vocabulary and invariants. No OS dependencies, builds anywhere. |
| `voltra-sources` | Built-in sources and filters. No OS dependencies either, so CI can run them. |
| `voltra-cli` | Headless front-end: what CI runs and what benchmarks drive. |

Crates are added when they are used, not up front. The full target layout is in
[`CLAUDE.md`](CLAUDE.md) §2.

## Performance

Every measurement, and every piece of known performance debt, lives in
[`docs/PERFORMANCE.md`](docs/PERFORMANCE.md) — including the experiments that
did not work.

## How this project is built

Small steps, each one planned before it is written. Plans live in
[`docs/plans/`](docs/plans), architecture decisions in
[`docs/adr/`](docs/adr), and the rules everything is held to —
performance contract included — in [`CLAUDE.md`](CLAUDE.md).

## Licence

GPL-2.0-or-later.

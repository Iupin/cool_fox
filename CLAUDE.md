# VIctory (cool_fox) — agent guide

A compatibility layer that runs an Xbox Series title natively on Windows: it
loads the title's x86-64 executable, implements the console APIs it imports,
translates D3D12.X to host Direct3D 12, and recompiles console shaders. Rust
workspace, Windows x64 only, GPL-3.0-or-later. This fork's focus is frame time
and memory residency on desktop GPUs.

`AGENTS.md` points here. Start with [docs/README.md](docs/README.md), the
documentation index.

## Hard rules

1. **Documentation is part of every change.** A change is not done until the
   docs that describe the touched code are true again, in the same commit.
   Use the table below to find which doc owns which code. If you add a file,
   add it to that crate's module map; if you add, remove or re-default a
   `VICTORY_*` switch, update `docs/reference/env-switches.md`; if you change
   how to build, test or launch, update `docs/building.md` or
   `docs/reference/repo-tooling.md`. If a doc is wrong, fix it even when it
   is not your change.
2. **No game-derived data in the tree.** No game files, dumps, logs with game
   data, caches, shader bytes or screenshots below the directory holding
   `.victory-root`. Tools refuse to write there and `cargo xtask policy`
   (pre-commit hook and CI) rejects it. See `docs/asset-policy.md`. User paths
   (`victory.toml`, `VICTORY_GAME_DIR`, cache, data) live outside the repo.
3. **Do not edit `Cargo.lock` by accident.** Build and test with `--locked`.
4. **Verify before claiming.** Before commit: both CI clippy commands,
   `cargo test --workspace --locked`, `cargo xtask policy`, and rustfmt on the
   files you changed. Clippy and tests are clean as of 2026-11-26 and must
   stay clean. The tree is not rustfmt-clean (121 files differ, CI does not
   check it), so do not reformat files you did not otherwise change. Exact
   commands and timings (the full test run takes about 15 minutes, nearly
   all of it `victory-shader`) are in `docs/building.md`; keep the state
   recorded there current.
5. Commit or push only when the user asks.

## Which doc owns which code

| Code | Doc to read first and keep current |
|---|---|
| whole system, data flow, import resolution | `docs/architecture.md` |
| `crates/victory-gpu` command/draw path (host, list, draw, indirect, descriptor, pipelines, submit, present) | `docs/crates/victory-gpu.md` (also holds the full module map) |
| `crates/victory-gpu` memory side (shadow, watch, resource, texture, uploads, residency, read-back, replay) | `docs/crates/victory-gpu-memory.md`, `docs/replay.md` |
| `crates/victory-shader` | `docs/crates/victory-shader.md` |
| `crates/victory-hle` | `docs/crates/victory-hle.md` |
| `crates/victory-loader` | `docs/crates/victory-loader.md` |
| `crates/victory-pe` | `docs/crates/victory-pe.md` |
| `apps/victory` (CLI) | `docs/crates/victory-cli.md` |
| `victory-core`, `victory-formats`, `victory-pipeline`, `victory-game`, `xtask` | `docs/crates/support-crates.md` |
| any `VICTORY_*` environment switch | `docs/reference/env-switches.md` |
| asset policy, hooks, CI, launch scripts, config resolution | `docs/reference/repo-tooling.md`, `docs/asset-policy.md` |
| build, test, lint | `docs/building.md` |
| why a design exists, measurements | `docs/findings.md` (append; history, not reference) |
| bugs found and how | `docs/bug-journal.md` (append) |

## Build in one line

```powershell
cargo build --release --locked -p victory                             # target\release\victory.exe
cargo build --locked --profile diag -p victory --features diagnostics # target\diag\victory.exe
```

Details, prerequisites and troubleshooting: `docs/building.md`.

## Working conventions

- Edition 2024, toolchain pinned in `rust-toolchain.toml`. `unsafe_op_in_unsafe_fn`
  and `unused_must_use` are denied; clippy warnings fail CI.
- Runtime behaviour is switched by `VICTORY_*` environment variables, each
  described in a comment where it is read. Keep that comment and the
  inventory in step.
- Switch helpers differ: `switch::on` is on for anything but `0`,
  `toggled!` defaults on, `asked_for!` is on only for `1`. Most are read once
  and cached. Many README-listed features are off unless a launcher sets them.
- A new translator switch that changes shader output must be added by hand to
  the list in `crates/victory-gpu/src/shader_cache.rs`, or the shader cache
  serves translations made without it.
- Code comments are stale in places (for example "relay off by default"; it
  is on). Each crate doc lists the ones found; trust the code, then the
  crate doc, then comments.
- Everything except `victory-game` is platform-level; title-specific
  knowledge belongs in `victory-game` only.
- The code and older docs use this fork's own plain-English vocabulary
  ("relay", "land", "thin", "card steps", "join parts", "write guards").
  The crate docs define each term; use the existing term rather than a new one.
- Large files (`victory-gpu/src/host.rs`, `victory-shader/src/emit/lower.rs`)
  are 10k+ lines: search by symbol, do not read whole.

# Building VIctory

How to build, test and check this workspace on Windows. Everything here was
run and measured on one machine on 2026-11-26 (Windows 11 Pro 10.0.26300,
Ryzen 9 5900X, 32 GB RAM, GeForce RTX 3080, commit `f4b48ad`). Times are wall
clock on that machine and will differ on yours.

## Prerequisites

| What | Version that worked | How it is found |
| --- | --- | --- |
| Rust | 1.98.1 (`x86_64-pc-windows-msvc`, with `rustfmt` and `clippy`) | `rust-toolchain.toml` pins it; rustup installs it on first use |
| MSVC C++ build tools | Visual Studio Community 2026 18.9.2, toolset 14.51.36231 (`link.exe` / `ml64.exe` 14.51.36256.0) | rustc and the `cc` crate find the newest install through vswhere; nothing has to be on `PATH` |
| Windows SDK | 10.0.26100.0 (10.0.22621.0 and 10.0.19041.0 also installed) | through the Visual Studio install |
| Git | 2.48.1.windows.1 | for the hook and the policy check |

Notes:

- The machine also has Visual Studio Community 2022 (toolset 14.37.32822) and
  Build Tools 2019. The build used the newest one (2026, toolset 14.51)
  without being told to.
- Any Visual Studio edition or the standalone Build Tools will do, as long as
  it has the "Desktop development with C++" workload (component
  `Microsoft.VisualStudio.Component.VC.Tools.x86.x64`) and a Windows SDK.
  To check what you have:

  ```powershell
  & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -all -products * `
      -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
  Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Lib"
  ```

- The only native code compiled during the build is `blake3`'s assembly
  (through `cc` and MSVC's `ml64.exe`). No crate needs cmake, clang, nasm or
  pkg-config. The `build.rs` files in `victory-gpu` and `victory-shader` only
  hash source files for the shader-cache key. `tests/fixtures/hle-probe`
  links `delayimp.lib`, which comes with MSVC.
- At run time, and in some tests, the shader translator uses `dxil.dll` (the
  DXIL validator) and `dxc.exe` from the newest Windows SDK
  (`...\Windows Kits\10\bin\<version>\x64\`). You can point at other copies
  with `VICTORY_DXIL` and `VICTORY_PC_DXC`. The build does not need them.

## One-time setup

```powershell
cd <the checkout>
rustup show active-toolchain     # installs 1.98.1 from rust-toolchain.toml if it is missing
cargo fetch --locked             # downloads the crates Cargo.lock names
cargo xtask install-hooks        # sets core.hooksPath to .githooks (the asset-policy pre-commit hook)
```

`cargo xtask` is an alias defined in `.cargo/config.toml`
(`run --quiet --package xtask --`). `install-hooks` only runs
`git config core.hooksPath .githooks`. The hook, `.githooks/pre-commit`, runs
`cargo xtask policy --staged`.

## Build configurations

| Build | Command | Output | Measured |
| --- | --- | --- | --- |
| Release (what `Play-VIctory.cmd` runs) | `cargo build --release --locked -p victory` | `target\release\victory.exe` (9,978,880 bytes, plus a 37 MB `victory.pdb`) | 1 min 13 s from clean; 0.6 s when nothing changed |
| Diagnostic / development (what `scripts\play.ps1` runs by default) | `cargo build --locked --profile diag -p victory --features diagnostics` | `target\diag\victory.exe` (14,240,768 bytes, plus a 51 MB `victory.pdb`) | 1 min 52 s from clean, with `target\release` already built |
| Debug | `cargo build --locked -p victory` | `target\debug\victory.exe` (23,280,640 bytes) | 0.5 s after `cargo test`, which had already built it |

The profiles (root `Cargo.toml`):

- `release`: the standard release profile with `debug = "line-tables-only"`,
  so crash stacks have file and line.
- `diag`: inherits `release` and has its own folder (`target\diag`), so a
  diagnostics build does not overwrite the plain release build. The two do
  not share compiled dependencies either, so each one builds from scratch
  the first time.
- `dev` (the default, also used by `cargo test`): dependencies are built at
  `opt-level = 2` (`[profile.dev.package."*"]`), because hashing and parsing
  game data in unoptimized code is too slow. Workspace crates are not
  optimized.

### The `diagnostics` feature

`apps/victory`'s `diagnostics` feature turns on the same feature in
`victory-core`, `victory-hle`, `victory-loader` and `victory-shader`.
`victory-hle` turns it on in `victory-gpu` (and adds the `windows` /
`windows-core` dependencies). It is a debugging build, not just a set of
extra checks. It adds:

- the `replay` command (`victory replay`, see `docs/replay.md`) and the GPU
  frame recorder and replayer (`victory-gpu` `replay`);
- logging down to `TRACE`. Without the feature, `INFO` is the most verbose
  level that can be logged (`victory-core/src/logging.rs`);
- debugging aids run at start-up (`apps/victory/src/run.rs`): scheduled
  stack dumps (`VICTORY_STACKS`), the sampling profiler (`VICTORY_PROFILE`),
  watched values (`VICTORY_WATCH`), pokes (`VICTORY_POKE`) and memory saved on
  a crash;
- GPU-side diagnostics in `victory-gpu`: frame capture to PNG
  (`VICTORY_CAPTURE`), copy statistics (`copystat`), the write-fault book
  (`VICTORY_FAULT_BOOK`), the card-memory book (`book`), texture-pop watching
  (`VICTORY_POP_WATCH`), A/B switches that flip while the title runs
  (`VICTORY_AB`), and many counters and log lines in `host.rs`, `draw.rs`,
  `watch.rs`, `queue.rs` and other files;
- counts of console-only CPU instruction sites (`victory-loader`
  `console_insn`), and the audio recorder child process
  (`VICTORY_AUDIO_CAPTURE`);
- `VICTORY_SHADER_LOOP_LIMIT`, which is only read in diagnostics builds.
  Whether the feature is on is part of the shader-cache key, so diagnostics
  and plain builds do not share cached translations.

To list every gate: `rg 'feature = "diagnostics"'` (about 535 places in 43
files).

### Output and disk use

After all of the builds above, `cargo test` and both clippy runs, `target\`
held 5.13 GB: `target\debug` 4.13 GB (mostly test binaries),
`target\diag` 0.53 GB and `target\release` 0.45 GB. `target\` is in
`.gitignore`.

Smoke test, which needs no game files:

```powershell
target\release\victory.exe --help
target\diag\victory.exe --help     # also lists `replay`
```

Both printed their command list and exited with 0.

## Checks (as CI runs them)

`.github/workflows/ci.yml` runs these on `windows-latest`:

```powershell
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
```

Plus, locally:

```powershell
cargo xtask policy               # asset policy over tracked + untracked, non-ignored files
cargo fmt --all -- --check       # not in CI (see below)
```

### State of the tree (2026-11-26, working tree on top of `f4b48ad`)

- **Clippy, default features: passes** (exit 0).
- **Clippy, `--all-features`: passes** (exit 0). The diag build has no
  compiler warnings.
- **Tests: pass.** `cargo test --workspace --locked --no-fail-fast`: 967
  passed, 0 failed, 45 ignored over the unit and integration test binaries;
  the doc-test runs have no tests. `victory-shader` alone takes about 15
  minutes; everything else finishes in under a minute after the build.
- **Policy: passes.** `asset policy: all files ok` (the count includes
  untracked files that are not ignored).
- **Later the same day (kernel reads into title memory, see the bug
  journal):** both clippy commands pass, `cargo test -p victory-hle --locked`
  158 passed, 2 ignored; `cargo test -p victory-gpu --locked` 349 passed,
  34 ignored; policy passes. The full workspace run was not repeated.
- **Later still (the writers watch, see the bug journal):** both clippy
  commands pass; `cargo test -p victory-gpu --locked` 361 passed, 34
  ignored; `-p victory-hle` 158 passed, 2 ignored; `-p victory-loader` 43
  passed; `-p victory` 12 passed (3 more, `code::tests`, with
  `--features diagnostics`); `cargo test -p victory-gpu --features
  diagnostics --locked --lib writers::` passes; policy passes (270 files).
  The full workspace run was not repeated.
- **2026-11-27 (the submitting thread's changes, `docs/findings.md`):** both
  clippy commands pass; `cargo test -p victory-gpu --locked` 369 passed, 34
  ignored; `-p victory-hle` 158 passed, 2 ignored; policy passes (271
  files). `cargo test --workspace --locked` passed everywhere but six of
  `victory-shader`'s `emit::valu_tests`, which failed with "The GPU device
  instance has been suspended" while a game ran on the same card; run alone
  again with a game running, four of them failed the same way. They run
  shaders on the card and nothing here changed `victory-shader`.

How it got there, from a tree where `f4b48ad` itself fails CI (37 / 36
clippy lints; 2 failing `victory-gpu` tests):

- The lints were fixed with behaviour-preserving edits in `victory-gpu`,
  `victory-loader`, `victory-formats`, `victory-hle` and `apps/victory`. A
  few are narrow `#[allow]`s with a reason beside them (two
  `large_enum_variant`, two `too_many_arguments`, one `wrong_self_convention`,
  one test's `unused_assignments`, one field kept only to hold a heap alive).
  `host/indirects.rs` is now compiled only under `test` or `diagnostics`.
  `host.rs` `Touch::Title` is constructed only by tests and carries a
  `cfg_attr(not(test), allow(dead_code))`: an open question whether to wire
  it up or remove it.
- The two failing tests
  (`indirect::tests::host_list_budget_starts_empty_at_each_submission`,
  `indirect::tests::warp_batch_charges_every_execution`) were left behind
  when `f4b48ad` made `VICTORY_INDIRECT_REST` off unless set to `1`. They
  assumed the old default. The tests were corrected (the second now runs
  under both settings); no runtime code changed.
- **rustfmt: 121 files differ** from `cargo fmt --all -- --check` with the
  repository's `rustfmt.toml` (`max_width = 100`). CI does not check
  formatting. Running `cargo fmt --all` would rewrite those 121 files, so
  format only the files you change (see the checklist).

### Tests that need things outside the repository

None of the tests read `VICTORY_GAME_DIR` or `VICTORY_TEST_TITLES`. Those
variables are only read by the configuration (`victory-core/src/config.rs`),
and the tests of that file give it fake values. Tests that need other
inputs either carry `#[ignore = "..."]` (45 in total: benchmarks,
measurements on the card, "needs streams / reference files outside the
repository", and tests that need `VICTORY_RELAY=...` and a process of their
own) or return early with a message on stderr:

- `victory-shader` `diff_tests`: need `VICTORY_CONSOLE_DXC` (the console
  SDK's `dxc.exe`) and print `no console or PC compiler; skipped` without it.
- GPU tests in `victory-shader` and `victory-gpu` print
  `no SM 6.6 ... device; skipped` or `no adapter: skipped` when there is no
  suitable device. Some only run on an AMD card (`no RDNA card; skipped`).
- `size_tests` and `waterfall_tests` print `no dxil.dll; validation skipped`
  when the SDK's validator cannot be found.

These early returns count as passes, so the totals above do not show which
ones actually ran. To run an ignored test, name it:
`cargo test -p <crate> <name> -- --ignored --nocapture`.

## The pre-commit hook

After `cargo xtask install-hooks`, `git config core.hooksPath` is
`.githooks`, and every `git commit` runs `cargo xtask policy --staged`
(about 1 s). That checks the staged files: allowed extensions or names
only, at most 640 KiB each, no NUL bytes, no long base64/hex runs, and no
file signatures such as PE, DXBC, DDS, RIFF, PDB, zip or cab. One
exception is listed in `xtask/src/policy.rs`. See `docs/asset-policy.md`
for the rules. Nothing derived from the game may go anywhere below the
repository root.

## Troubleshooting (what came up here)

- **`cargo test` stops at the first failing crate.** Use `--no-fail-fast`
  to get results for every crate.
- **`victory-shader` tests take about 14 minutes** in the debug test
  profile, and the harness prints many `has been running for over 60
  seconds` lines. These are the GPU/WARP comparison tests, and they do
  finish. To run them alone: `cargo test --locked -p victory-shader`.
- **Clippy only shows the first crate's errors.** Add `--keep-going`; even
  then, crates that depend on one that fails to compile are not checked
  until it is fixed.
- **Several Visual Studio versions installed.** Nothing had to be set. The
  newest toolset with the C++ workload (VS 2026, 14.51) was used. If the
  build cannot find `link.exe`, check that the workload is installed with
  the vswhere command above. It does not have to be on `PATH`, and you do
  not need a Developer prompt.
- **Building `diag` after `release` rebuilds every dependency.** Each profile
  folder is separate, so expect the full build time the first time.

No change to `.cargo/config.toml` or any other build setting was needed.

## Definition of done before committing

- [ ] `rustfmt --check <the .rs files you changed>` (or `cargo fmt -p <crate>`
      if that crate is already clean). Do not reformat the whole tree in a
      change about something else.
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`: exit 0.
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`:
      exit 0.
- [ ] `cargo test --workspace --locked --no-fail-fast`: no failures. If you
      did not touch `victory-shader`, `--exclude victory-shader` saves about
      15 minutes; CI still runs it.
- [ ] `cargo build --release --locked -p victory` and, if you touched
      diagnostics code, `cargo build --locked --profile diag -p victory --features diagnostics`.
- [ ] `cargo xtask policy` passes (the hook checks the staged files).
- [ ] Docs updated: this file if the build changed, and the comment where a
      new `VICTORY_*` switch is read.

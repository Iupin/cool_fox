# Repository tooling: asset policy, xtask, hooks, CI, launchers, configuration

What keeps the repository free of game data, how it is built and checked, and
how a run is started. Rule text: [docs/asset-policy.md](../asset-policy.md).
Every switch the launchers set: [env-switches.md](env-switches.md).
CLI: [../crates/victory-cli.md](../crates/victory-cli.md).

## Files covered

| file | role |
|---|---|
| `.victory-root` | marker file at the source root; `ensure_outside_repo` refuses any path below a directory holding it |
| `.gitignore` | first line of defence: build output, `victory.toml`, data-dump folder names, executables, console packages, typical data extensions; `!/crates/victory-formats/src/bcpack-tables.bin` re-includes the one exception |
| `.gitattributes` | `* text=auto eol=lf`; `*.cmd` and `*.ps1` `eol=crlf` |
| `.githooks/pre-commit` | `exec cargo xtask policy --staged` |
| `.cargo/config.toml` | `[alias] xtask = "run --quiet --package xtask --"` |
| `.github/workflows/ci.yml` | the CI job |
| `rust-toolchain.toml` | channel `1.98.1`, target `x86_64-pc-windows-msvc`, `rustfmt` + `clippy`, profile minimal |
| `rustfmt.toml` | `max_width = 100`, `use_field_init_shorthand` |
| `Cargo.toml` | workspace (`crates/*`, `apps/*`, `xtask`; `tests/fixtures` excluded), lints, profiles |
| `victory.example.toml` | template for `victory.toml` |
| `scripts/play.ps1` | launcher of the upstream AMD fork's settings (diag build by default) |
| `Play-VIctory.cmd` | launcher of the "pareto-residency" Windows release (Series S) |
| `xtask/` | `cargo xtask policy`, `cargo xtask install-hooks` |

## Asset policy mechanics

Four layers (docs/asset-policy.md "Enforcement"); what each one actually does:

| layer | where | refuses | when it runs |
|---|---|---|---|
| 1 `.gitignore` | `.gitignore:10-78` | hides `/target/`, `/victory.toml`, `/game/ /games/ /assets/ /data/ /dump(s)/ /extracted/ /cache/ /out/`, `*.exe *.dll *.pdb *.lib *.sys *.obj *.exp *.ilk`, `*.xvd *.xvc *.msixvc *.xsp *.cab *.msi`, `*.bin *.dat *.pak *.rpf *.dds *.ktx(2) *.dxbc *.dxil *.cso *.spv *.wav *.xma *.ogg *.opus *.mp3 *.mp4 *.bik *.bk2 *.awc *.zip *.7z *.rar` | `git status`/`git add` (advisory: `git add -f` bypasses it) |
| 2 static check | `xtask/src/policy.rs` `check()` | see rules below | `cargo xtask policy` (manual), pre-commit hook (`--staged`), and **every `cargo test --workspace`** via `repository_passes_policy` (`policy.rs:229`) — the CI enforcement |
| 3 runtime guard | `crates/victory-core/src/policy.rs:41` `ensure_outside_repo` | writing derived data under any directory holding `.victory-root`, or under the tree the binary was compiled from | at each call site listed below |
| 4 tests | test code | – | tests needing real files read paths from env vars (`VICTORY_BCPACK_TABLES`, `VICTORY_XMA_STREAMS`, …) and are `#[ignore]` or skip when unset |

### Static check rules (`xtask/src/policy.rs`)

A file passes only if **all** hold (`check`, `policy.rs:147-183`):

| rule | constant | value |
|---|---|---|
| extension on the allowlist (case-insensitive) | `ALLOWED_EXTENSIONS` | `rs toml md txt lock yml yaml json hlsl hlsli ps1 cmd sh` |
| or exact file name allowed | `ALLOWED_NAMES` | `.gitignore .gitattributes .editorconfig .victory-root pre-commit LICENSE LICENSE-MIT LICENSE-APACHE` |
| size | `MAX_FILE_SIZE` | 640 KiB (raised for the translator's 570 KiB source file) |
| does not start with a forbidden magic | `FORBIDDEN_MAGIC` | `MZ`, `DXBC`, `DDS `, `RIFF`, `BC\xC0\xDE`, `Microsoft C/C++ MSF`, `!<arch>`, `PK\x03\x04`, `7z\xBC\xAF`, `MSCF`, `\xD0\xCF\x11\xE0` (magic wins over extension) |
| no NUL byte | – | binary content |
| longest run of `[A-Za-z0-9+/=]` | `MAX_ENCODED_RUN` | ≤ 512 chars |
| longest run of `\xNN` / `0xNN` literals (only `, \ whitespace` between) | `MAX_ENCODED_RUN` | ≤ 512 hex digits; decimal arrays are not detected (`shortcut:` comment at `policy.rs:195`) |
| path exempt | `EXCEPTIONS` | only `crates/victory-formats/src/bcpack-tables.bin` |

Files checked: without `--staged`, tracked plus untracked-not-ignored files
(so a stray file in the working tree fails `cargo test` locally); with
`--staged`, added/copied/modified/renamed paths in the index, read from the
index. Deletions are skipped.

### Runtime guard call sites (verified by `git grep ensure_outside_repo\|cache_subdir\|data_subdir\|Cache::open`)

| site | path checked |
|---|---|
| `crates/victory-core/src/config.rs:132` (`subdir`) | every `Config::cache_subdir` / `data_subdir`: `run` (`images`, `shaders`, `<ID>/storage`, `<ID>/temp`), `translate --warm`, `shaders`, `warm` |
| `crates/victory-pipeline/src/cache.rs:39` (`Cache::open`) | shader cache root |
| `crates/victory-loader/src/windows.rs:56` (`load`) | folder the prepared image is written to |
| `apps/victory/src/translate.rs:245` | `VICTORY_EMIT_DUMP` |
| `apps/victory/src/main.rs:261` | `victory config` reports (does not refuse) violations of all four configured paths |

Not guarded (verified: no call on these paths): the folders and files named by
diagnostic env switches — `VICTORY_CAPTURE`, `VICTORY_SHADER_SAVE`,
`VICTORY_RECORD`, `VICTORY_FAIL_DUMP`, `VICTORY_DUMP_MATCH`,
`VICTORY_XMA_DUMP`, `VICTORY_MEMORY_DUMP`, `VICTORY_DSTORAGE_LOG`,
`VICTORY_SHADER_CENSUS`, `VICTORY_POP_FOLDER`, `VICTORY_AUDIO_CAPTURE`,
`VICTORY_TIMELINE`, `VICTORY_FAULT_BOOK`, `VICTORY_WRITE_SITES`. The
`pipelines` folder (`<cache>/shaders` → `with_file_name("pipelines")`) is a
sibling of a checked folder, so it is outside the repo whenever that one is.
docs/asset-policy.md's "everything that writes derived data calls
`ensure_outside_repo`" is therefore not literally true for these.

## xtask commands

| command | effect | exit |
|---|---|---|
| `cargo xtask policy` | full check; prints `asset policy: N files ok` or one `POLICY: <path>: <problem>` per problem on stderr | non-zero on any violation |
| `cargo xtask policy --staged` | the same over the index | non-zero on any violation; aborts the commit when run by the hook |
| `cargo xtask install-hooks` | `git config core.hooksPath .githooks` in the workspace root | non-zero if git fails |

## Git hook

`.githooks/pre-commit` (POSIX `sh`, run by Git for Windows' bundled shell)
`exec`s `cargo xtask policy --staged`. It is inactive until
`cargo xtask install-hooks` (or `git config core.hooksPath .githooks`) has
been run in the clone; nothing installs it automatically. It builds xtask on
first use, so the first commit is slow.

## CI (`.github/workflows/ci.yml`)

| item | value |
|---|---|
| triggers | every `pull_request`; `push` to `main` |
| permissions | `contents: read` |
| concurrency | group `ci-${{ github.ref }}`, cancel in progress |
| job | `check` on `windows-latest`, 60 min timeout |
| steps | `actions/checkout@v5`; `rustup toolchain install` (installs what `rust-toolchain.toml` pins); `Swatinem/rust-cache@v2`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` (diagnostics + shader `host`); `cargo test --workspace --locked` |

Not run in CI: `cargo fmt --check`, the `diag` profile build, `--ignored`
tests, anything needing game data or a GPU beyond what tests self-skip.
Clippy runs with the workspace lint table (`Cargo.toml:47-65`), which allows
nine lints; its comment still says "two style lints".

## Build profiles and toolchain

| profile | settings | output |
|---|---|---|
| `dev` | dependencies at `opt-level = 2` (`Cargo.toml:67-70`) | `target\debug` |
| `release` | `debug = "line-tables-only"` | `target\release\victory.exe` |
| `diag` | `inherits = "release"`; used with `--features diagnostics` | `target\diag\victory.exe` |

`rust-version = "1.88"` in the workspace is the declared minimum; the pinned
toolchain is 1.98.1. Builds need the MSVC C++ build tools and a Windows SDK.
`crates/victory-gpu/build.rs` fingerprints the translator for the shader
cache; `crates/victory-shader/build.rs` exports a second fingerprint that
nothing uses (see
[env-switches.md](env-switches.md#mechanisms-worth-knowing)).

## Launch scripts

Both end in `victory run <exe> --execute`; neither builds anything.

| | `scripts/play.ps1` | `Play-VIctory.cmd` |
|---|---|---|
| usage | `scripts\play.ps1 -Exe <exe> [-Safe] [-RayTracing] [-Build diag\|release] [-More @{NAME='v'}]` | `Play-VIctory.cmd [pareto-residency]` (any other argument: usage, exit 2) |
| binary | `<script dir>\..\target\<Build>\victory.exe`, default `diag` | `<cmd dir>\victory-3d-rtx-pareto-residency.exe` (release ZIP), else `<cmd dir>\target\release\victory.exe` |
| title exe | `-Exe` (required) | `%VICTORY_GAME_DIR%\game_scarlett_master.exe`; `VICTORY_GAME_DIR` defaults to `%~dp0..\GTA6.Xbox.v1.0.2.1` |
| console | default (`series-x`) | `--console series-s` |
| environment | **removes** every `VICTORY_*` except `CONFIG`, `GAME_DIR`, `CACHE_DIR`, `DATA_DIR`, then sets its own (`play.ps1:15-35`) | sets each only `if not defined`; caller's values win |
| working dir | `Push-Location` to the repo root so `victory.toml` there is found (`play.ps1:37`) | the caller's current directory (so `victory.toml` is looked for there) |
| cache | from config/env | `VICTORY_CACHE_DIR` defaults to `%LOCALAPPDATA%\VIctory\cache-pareto` |
| logging | `VICTORY_LOG` removed → `info` | `VICTORY_LOG=warn` unless `VICTORY_LOG` or `RUST_LOG` set |
| errors | prints and returns if the binary or exe is missing | exit 2 if the exe or the binary is missing; else victory's exit code |

Switch sets (values in [env-switches.md](env-switches.md#what-the-launchers-set)):
both set the same 16 core speed switches (thin lists, relay, card steps,
joined parts, upload-ahead, show-after, ambient 0.8, …). `play.ps1` adds
`VICTORY_TITLE_PRIORITIES=0`, `VICTORY_D3D12_DRED=light` and
`VICTORY_SKIP_PIPELINES=none` (both only meaningful in diag builds; `none` is
not a valid hash and is ignored with a warning), `-Safe` swaps the three
newest (`THIN`, `JOIN_PARTS`, `UPLOAD_AHEAD`) for `UNDER_WIDE=0`,
`LAND_ECHO=0`, `-RayTracing` sets `VICTORY_BVH_BUILDER=1`. `Play-VIctory.cmd`
adds the "residency" set: `VICTORY_LEVELS_ON_DEMAND=1`,
`VICTORY_CARD_BUFFERS_MIN_KB=64`, `VICTORY_AMBIENT_AIR=1.5`. The scripts do
not say why each is chosen; the read-site comments do. Neither sets
`VICTORY_PIPELINE_BOOK=1`, which `victory warm` depends on.

## Configuration and path resolution

`victory_core::Config::load` (`crates/victory-core/src/config.rs:48-101`):

1. Config file: `VICTORY_CONFIG` if non-empty (must exist, else error), else
   `victory.toml` in the **process's current directory** if it is a file, else none.
2. Parse TOML: only a `[paths]` table with `game_dir`, `cache_dir`, `data_dir`,
   `test_titles`; unknown keys are an error.
3. For each value: non-empty env var > file > default.

| value | env | default |
|---|---|---|
| `game_dir` | `VICTORY_GAME_DIR` | none |
| `cache_dir` | `VICTORY_CACHE_DIR` | `%LOCALAPPDATA%\victory\cache` (→ `$XDG_CACHE_HOME`, → `$HOME/.cache`) |
| `data_dir` | `VICTORY_DATA_DIR` | `%LOCALAPPDATA%\victory\data` (same fallbacks) |
| `test_titles` | `VICTORY_TEST_TITLES` (`;`-separated, replaces the file list) | `[]` |

Relative paths in the file or env are relative to the current directory (they
are not rebased onto the config file's folder). `victory config` prints the
result and any policy violation.

## Discrepancies found (README and docs vs. code/scripts)

| statement | where | reality |
|---|---|---|
| three different repository URLs | README: releases at `github.com/codec-d/cool_fox`, clone from `github.com/avatarmanz/VIctory-7900XTX`; `git remote` | `origin` is `https://github.com/Iupin/cool_fox.git` |
| "VIctory on an RX 7900 XTX" title vs. launcher | README first section | `Play-VIctory.cmd` runs a binary named `victory-3d-rtx-pareto-residency.exe` with "RTX-tested residency settings" and Series S; `play.ps1` uses the default Series X |
| `target\diag\victory.exe warm --used` before a first run | README "How to run it" | finds nothing unless an earlier run had `VICTORY_PIPELINE_BOOK=1`; `play.ps1` clears and never sets it |
| asset guard on "everything that writes derived data" | docs/asset-policy.md, Enforcement 3 | diagnostic dump/record folders are not checked (list above) |
| crate doc: BCPack, XMA2, Opus "Planned" | `crates/victory-formats/src/lib.rs:3-6` | BCPack and XMA2 implemented here; Opus is in `victory-hle` |
| `victory-game`: build identities, hooks | docs/architecture.md crate table | `KNOWN_BUILDS` is empty and no hooks live there |
| "two style lints are let through" | `Cargo.toml:53` comment | nine lints are `allow`ed |
| `VICTORY_MERGE_RACE=1` counter | docs/README-fxceless.md | no longer in the code |
| `VICTORY_CARD_ROOM_FOR_BUFFERS=1, off` | docs/README-fxceless.md, comment at `shadow.rs:255` | reads "true unless `0`" (on by default); the later comment at `shadow.rs:274` agrees with the code |
| commands list | docs/README-fxceless.md "Commands" | lacks `shaders`, `warm`, `replay`, `translate --warm` |
| size limit test "too big" | `xtask/src/policy.rs:254` | 600 KiB is under the 640 KiB cap; the test passes through the encoded-run rule instead |

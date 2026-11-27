# `victory` — the command-line front end (`apps/victory`)

One binary, `victory.exe`, built from `apps/victory` (~3.4k lines, 9 files).
It parses arguments with clap (`apps/victory/src/main.rs:20-168`), loads the
configuration from `victory-core`, and hands the work to the big crates:
the loader ([victory-loader.md](victory-loader.md)), the runtime
([victory-hle.md](victory-hle.md)), the GPU back end
([victory-gpu.md](victory-gpu.md)), the shader translator
([victory-shader.md](victory-shader.md)) and the PE reader
([victory-pe.md](victory-pe.md)). Support crates: [support-crates.md](support-crates.md).
Every environment switch: [../reference/env-switches.md](../reference/env-switches.md).
Launchers, config precedence, policy, CI: [../reference/repo-tooling.md](../reference/repo-tooling.md).

## Files

| file | lines | contents |
|---|---:|---|
| `src/main.rs` | 303 | `Cli`/`Command` clap definitions, dispatch, `show_config`, `default_paths` |
| `src/run.rs` | 538 | `run`: prepare, map, bind, start the title; late `LoadLibrary` of title DLLs (`LateDlls`, re-entrant `LoaderLock`); diagnostics aids (`watch`, `poke`, the writers watch, crash saves); AVX2 pre-flight |
| `src/code.rs` | 131 | `code-bytes` (diagnostics builds): an image's bytes at an rva, as mapped, to a `.bin` and a hex `.txt` outside the repository |
| `src/census.rs` | 334 | `census`: import coverage over many PE images |
| `src/inspect.rs` | 154 | `inspect` (one PE image) and `lib-exports` (import library) |
| `src/shader.rs` | 231 | `shader-info`; also `sources()` (find DXBC containers raw or inside SGD5 files) and `MAX_FILE` (2 GiB) shared with `translate` |
| `src/translate.rs` | 963 | `translate` (offline batch + tally), `translate --warm`, `shaders` (`precompile`) |
| `src/warm.rs` | 210 | `warm`: host pipelines made ahead in child processes (Windows only) |
| `src/replay.rs` | 507 | `replay` argument struct and dispatch (Windows + `diagnostics` only) |
| `tests/census.rs` | – | integration test: `census` fails on a missing path, succeeds on a non-PE file |

## Subcommands

`victory <command> [args]`. `[paths...]` empty means: the configured
`game_dir` plus every `test_titles` entry (`main.rs:269` `default_paths`); error
if none is configured.

| command | what it does | arguments & flags | calls into | writes |
|---|---|---|---|---|
| `inspect <file>` | headers, sections, imports (incl. delay-load), exports, TLS, BLAKE3, CodeView PDB/key, DLL flags of one PE image | `--imports` list every imported function; `--exports` list every export | `victory_pe::ImageInfo::parse` | stdout only |
| `census [paths...]` | walks paths, parses every file starting with `MZ`, tallies each `dll!function` as hle / host / missing | `--strict` host pass-through does not count (`HostPolicy::none()`); `--missing` list missing imports by number of images | `victory_hle::HleResolver`, `HostPolicy` | stdout; error if a top-level path cannot be walked (`census.rs:54`) |
| `lib-exports <lib>` | `dll!export` pairs of an import library, per-DLL counts | `--list` every export | `victory_pe::implib::ImportLibrary::parse` | stdout |
| `shader-info [paths...]` | finds shader containers (raw `DXBC`, or SGD5 records) and reports layout: DXIL part, bitcode, XDAT/xbop, native GFX10 code, decode errors | `--verbose` one line per container; `--disasm` decoded native instructions (implies `--verbose`) | `victory_game::sgd5`, `victory_shader::{Container, native_programs, gfx10}` | stdout; files > 2 GiB skipped |
| `translate [paths...]` | every container through the translator (IR, structurisation, resources, emit); tallies what fails, by reason | `--validate` run `dxil.dll` validator (`VICTORY_DXIL`, else newest Windows SDK); `--pipelines` also create a host D3D12 pipeline per shader; `--list` one line per program; `--jobs N` threads (default all logical cores); `--warm <file\|all>` see next row | `victory_shader::*`, `emit::validator`, `emit::host::Host` | stdout; optional dumps via `VICTORY_EMIT_DUMP`, `VICTORY_FAIL_DUMP`, `VICTORY_DUMP_MATCH`, `VICTORY_SHADER_CENSUS` |
| `translate --warm <file>` | translates only containers whose 16-hex-digit BLAKE3 prefix appears anywhere in `<file>` (a run's log), or every one with `all`, into the run's shader cache; no GPU work | `--jobs N` (default **4** here, `translate.rs:180`) | `victory_hle::modules::d3d12x::warm_shader` → `victory_gpu::shader_cache::precompile` | `<cache>/shaders` |
| `shaders [paths...]` | translate every container found into the shader cache (dedup by BLAKE3 of the container; the file's name is passed on, so a container found in an image `VICTORY_F16_NEAREST` names is translated as in a run, as `--warm` does too) | `--jobs N` (default all cores) | `d3d12x::precompile` | `<cache>/shaders`; prints failures by reason |
| `warm` (Windows) | make host pipelines of every pipeline recorded by earlier runs, so the driver's disk cache has them; resumable | `--jobs N` processes (default 2/3 of cores); `--limit N`; `--again` redo ones done before; `--used` only those earlier runs drew with (`early-pipelines.txt`); hidden `--these <i,j,…>` (child mode) | `victory_hle::known_pipelines` (= `victory_gpu::pipeline::known`) | `<cache>/pipelines/{warm-done,warm-bad,slow-pipelines}.txt` |
| `run <exe>` | prepare, map and bind the title; with `--execute`, run it | `--unimplemented trap\|zero` (default `trap`); `--strict` no host pass-through; `--execute`; `--console series-x\|series-s` (default `series-x`); `-- <title args>` passed to the title's command line | see [run sequence](#run-sequence) | `<cache>/images`, `<cache>/shaders`, `<cache>/pipelines`, `<data>/<TITLEID>/{storage,temp}`, `<data>/<TITLEID>/crashes.log` |
| `replay <folder>` (Windows + `diagnostics`) | run a recorded frame's GPU work again; timing, A/B of shader variants, divergence search, pixel history | ~50 flags, [below](#replay) and [docs/replay.md](../replay.md) | `victory_hle::replay::play` | stdout; images/shaders into the capture folder |
| `code-bytes <file> <rva> <len>` (`diagnostics`) | the bytes of a title image (exe or DLL) at `rva..rva+len` (hex), from its prepared copy as the loader maps it (zeros past a section's raw data; an error outside the sections), for an external disassembler: the tree has no general x86-64 disassembler | `--out <file>` the raw file (default `<cache>/code/<name>-<rva>-<len>.bin`); a hex listing goes beside it as `.txt`; prints an `objdump -D -b binary -m i386:x86-64 -M intel --adjust-vma=<rva>` line | `victory_loader::prepare`, `victory_pe::ImageInfo` | the `.bin` and `.txt` (refused inside the repository) |
| `config` | print the resolved configuration and check each path against the asset policy | – | `victory_core::Config::load`, `ensure_outside_repo` | stdout; prints `POLICY VIOLATION: …` but still exits 0 |

`--help` on any command shows clap's text, which is the doc comment on the
`Command` variant (`main.rs:33-151`). Note: the `--jobs` help text says "default: every logical core" for `translate`, but with `--warm` the default is 4.

### `warm` details

* Keys come from `known-pipelines.bin`, written only by runs with
  `VICTORY_PIPELINE_BOOK=1` (`crates/victory-gpu/src/pipeline/known.rs:41`).
  Neither launcher sets it, so a fresh setup reports `0 pipelines known`.
* Each child (`victory warm --these …`, IDLE priority, `warm.rs:137-147`) makes
  up to 40 pipelines (`AT_ONCE`) then exits, so compiler memory is returned;
  a child silent for 180 s (`LONGEST`) is killed and the pipeline it was on goes
  to `warm-bad.txt` (never retried). Done keys are stored per driver/translator
  `fingerprint()` in `warm-done.txt`; > 1 s pipelines go to `slow-pipelines.txt`,
  which runs read back.

### Run sequence

`run::run` (`apps/victory/src/run.rs:39-198`), in order:

1. With `--execute`: AVX2 check (`VICTORY_SKIP_CPU_CHECK=1` bypasses).
2. `lifecycle::wait_for_previous()` — a relaunched title waits for the old process.
3. `Config::load()`; read the exe; `victory_loader::prepare` (patched copy +
   BLAKE3); `victory_game::identify(hash)` prints the build label
   (`KNOWN_BUILDS` is empty, so it always prints `unknown`).
4. `d3d12x::use_shader_cache(<cache>/shaders)` (also sets `<cache>/pipelines`);
   `victory_loader::load` writes the prepared image to `<cache>/images` and maps it.
5. Title id from `MicrosoftGame.config` beside the exe (missing → id 0);
   `G:` drive = exe folder, `T:` = `<data>/<ID>/temp`, storage = `<data>/<ID>/storage`.
6. `console::set(series-x|series-s)`; `load_title_dlls` maps DLLs beside the exe
   that neither host nor runtime serve; missing ones are reported, and with
   `--execute` + `trap` the run stops (`run.rs:134`) — `--unimplemented zero` overrides.
7. Bind imports of each title DLL then the exe (`bound … hle, host, title, missing`).
8. Without `--execute`: print `not executing` and exit 0.
9. With it: `write_sites::apply_asked`, crash reporter + `crashes.log`, console
   timers, diagnostics aids (diag builds: `VICTORY_STACKS`, `VICTORY_PROFILE`,
   audio capture, `VICTORY_WATCH`, `VICTORY_POKE`, the writers watch
   `VICTORY_WRITERS*` through `victory_hle::watch_writers`, whose window counts
   from here, `VICTORY_CRASH_SAVE`),
   late-DLL loader for the title's `LoadLibrary`, then `spawn_main` and join.

### Replay

Flags of `replay::Replay` (`apps/victory/src/replay.rs:27-220`), grouped. The
folder is the one given as `VICTORY_RECORD` (its last capture) or one capture.

| group | flags |
|---|---|
| run shape | `--loops N` (5), `--no-restore`, `--one-submit`, `--wait-all`, `--wait-lists`, `--pace <ms>`, `--awake`, `--breadcrumbs`, `--debug-layer`, `--only <a..b>`, `--until <list:cmd>` |
| timing | `--timing lists\|detailed`, `--top N` (20), `--compare` (unchanged run first) |
| what is left out | `--no-barriers`, `--merge-barriers`, `--skip-clamps`, `--batch-clamps [N]`, `--skip-hash-passes`, `--skip-shader <text>` (repeatable), `--no-merge` |
| buffers | `--buffers as-recorded\|vram\|sysmem`, `--only-buffers <filter>` (repeatable) |
| shaders | `--shaders <dir>`, `--shaders-b <dir>`, `--retranslate <dir>` with `--view-limit`, `--raw-views`, `--dump <name>` |
| divergence | `--diverge [N]`, `--diverge-picture [N]`, `--draws <name>`, `--lists <a..b>`, `--roots N`, `--commands <a..b>`, `--step N`, `--watch <res:off:bytes>` |
| inspection | `--listing <a..b>`, `--flow <a..b>` (+`--flow-draws`, `--flow-slots`), `--slots <list:cmd>`, `--check-written`, `--upcopies` |
| images / pixels | `--image <png>`, `--images <dir>` (+`--texture`, `--least`, `--width`, `--raw`), `--pixel <x,y>` (+`--of`, `--depth`, `--targets-only`, `--split`) |

## Flags and features

| build | how | what changes |
|---|---|---|
| release | `cargo build --release --locked -p victory` → `target\release\victory.exe` | logs capped at info (`victory-core/src/logging.rs:13`); no `replay`; diagnostics-gated switches ignored |
| diag | `cargo build --locked --profile diag -p victory --features diagnostics` → `target\diag\victory.exe` | profile `diag` inherits `release` (`Cargo.toml:78`); feature `diagnostics` turns on `victory-core`, `-hle` (→ `-gpu`, `-loader`), `-loader`, `-shader` diagnostics (`apps/victory/Cargo.toml:21-27`); adds `replay` and `code-bytes`, debug/trace logging, capture, debug layer, scripted keys, stack dumps, `VICTORY_AB` |

`victory-shader` is always built with feature `host` (D3D12 pipeline checks for
`translate --pipelines`). `warm` and `run` need Windows; on other targets `run`
bails with `running titles requires Windows` and `warm`/`replay` do not exist.

## Configuration and paths

Resolved by `victory_core::Config::load` (`crates/victory-core/src/config.rs:48`)
for every command that needs paths; precedence per value:
**environment variable → config file → default**.

| value | env | file key (`[paths]`) | default |
|---|---|---|---|
| config file | `VICTORY_CONFIG` (error if it names a missing file) | – | `victory.toml` in the **current directory** (only if it exists) |
| game dir | `VICTORY_GAME_DIR` | `game_dir` | none |
| cache dir | `VICTORY_CACHE_DIR` | `cache_dir` | `%LOCALAPPDATA%\victory\cache` (else `$XDG_CACHE_HOME/victory/cache`, else `$HOME/.cache/victory/cache`) |
| data dir | `VICTORY_DATA_DIR` | `data_dir` | same base, `victory\data` |
| test titles | `VICTORY_TEST_TITLES` (`;`-separated) | `test_titles = []` | empty |

Empty env values count as unset. Unknown keys in the file are an error
(`deny_unknown_fields`). Every subfolder made through `cache_subdir` /
`data_subdir` is checked by `ensure_outside_repo` first.

### Where output goes

| path | written by | contents |
|---|---|---|
| stdout | every command | the report (plain text, aligned columns; not a stable machine format) |
| stderr | `tracing` (`logging::init("info")`), `VICTORY_WATCH` lines, `shader-info` skips | logs; filter `VICTORY_LOG`, else `RUST_LOG`, else `info` |
| `<cache>/images/<exe name>` | `run` (`victory_loader::load`) | prepared copy of the title executable/DLLs that is mapped |
| `<cache>/shaders/xx/yyyy…` | `run`, `translate --warm`, `shaders` | translated shaders, content-addressed ([support-crates.md](support-crates.md#victory-pipeline)) |
| `<cache>/pipelines/` | `run`, `warm` | pipeline library, `known-*.bin` (with `VICTORY_PIPELINE_BOOK=1`), `early-/slow-pipelines.txt`, `warm-*.txt` |
| `<cache>/crash-<rva>.bin` | `run` + `VICTORY_CRASH_SAVE` (diag) | title memory ranges at a crash |
| `<data>/<TITLEID>/storage`, `/temp` | `run` | the title's saves / temporary drive |
| `<data>/<TITLEID>/crashes.log` | `run --execute` | crash reports and host GPU failures (appended) |

## Exit codes

| code | when |
|---|---|
| 0 | command finished; `run` without `--execute`; `run --execute` after the title's main thread returned (prints `title exited with <code>` but still returns 0, `run.rs:196`); `config` even with a policy violation |
| 1 | any `anyhow` error returned from `main` (Rust prints `Error: …` with the context chain) |
| 2 | clap argument error (clap's default) |
| `0x887A0005` | host GPU failure: the runtime calls `TerminateProcess` with `DXGI_ERROR_DEVICE_REMOVED` (`crates/victory-hle/src/modules/d3d12x.rs:31-37`) |
| title's code | title calls `ExitProcess`/`TerminateProcess` (inference: passed through to the host; not verified here) |
| 0/1 | `warm --these` children: 1 when the parent stopped reading |

`Play-VIctory.cmd` exits 2 on its own usage/path errors, else with victory's code.

## Typical workflows

| goal | commands |
|---|---|
| check setup | `copy victory.example.toml victory.toml` (edit paths), `victory config` |
| what does the title import, what is missing | `victory census <game dir> --missing` (add `--strict` to ignore host pass-through) |
| look at one image | `victory inspect <exe> --imports` |
| survey / translate shaders offline | `victory shader-info <dir>`; `victory translate <dir> --validate --list` |
| fill the shader cache before a first run | `victory shaders <dir>` (or `translate <dir> --warm all --jobs N`; set `VICTORY_HOST_WAVE`/`VICTORY_VIEW_LIMIT` to what the run's log says of the host) |
| pre-build driver pipelines | one run with `VICTORY_PIPELINE_BOOK=1`, then `victory warm` (or `--used`) |
| bind without running | `victory run <exe>` |
| play | `scripts\play.ps1 -Exe <exe>` (diag build) or `Play-VIctory.cmd pareto-residency` (release build, Series S) — see [repo-tooling.md](../reference/repo-tooling.md#launch-scripts) |
| which title code writes some title memory | diag build: `VICTORY_WRITERS=<address>+<len>` or `VICTORY_WRITERS_OF=<shader>:s<slot>+<offset>` with `VICTORY_WRITERS_FROM`/`_FOR` ([victory-gpu-memory.md](victory-gpu-memory.md) §3.4a); then `victory code-bytes <image> <rva> <len>` and a disassembler |
| debug a frame | run with `VICTORY_RECORD=<dir>` (diag), then `victory replay <dir> …` ([docs/replay.md](../replay.md)) |

## Recipes

* **Add a subcommand**: add a variant to `Command` (doc comment = `--help`),
  a match arm in `main`, and a module with `pub fn run(...) -> anyhow::Result<()>`.
  Load paths with `Config::load()` / `crate::default_paths`, and send any
  derived output through `cfg.cache_subdir(..)` or `ensure_outside_repo`
  (docs/asset-policy.md). Gate Windows/diagnostics-only code with
  `#[cfg(windows)]` / `#[cfg(all(windows, feature = "diagnostics"))]` on both
  the module and the variant, as `Warm`/`Replay` do.
* **Add a debugging env switch to `run`**: put it in `run::diagnostics` under
  `cfg(feature = "diagnostics")` with a comment at the read naming the
  variable and format; add it to [env-switches.md](../reference/env-switches.md).

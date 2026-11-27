# Environment switches: complete inventory

Every environment variable the repository reads, at the commit this was
written against (`f4b48ad`). The source code is authoritative; this page is an
index into it. The README's rule still holds: each `VICTORY_*` switch is
explained in a comment where it is read, and this page quotes that comment.

## Counts

| set | count | how |
|---|---:|---|
| distinct `VICTORY_*` names anywhere in tracked files | **354** | `git grep -ohE 'VICTORY_[A-Z0-9_]+' \| sort -u \| wc -l` |
| distinct names in `.rs` files (code and comments) | 353 | `git grep -ohE 'VICTORY_[A-Z0-9_]+' -- '*.rs' \| sort -u \| wc -l` |
| distinct names as a string literal `"VICTORY_..."` in `.rs` | 352 | `git grep -ohE '"VICTORY_[A-Z0-9_]+"' -- '*.rs' \| sort -u \| wc -l` |
| **read by product code** (tables below) | **322** | literal outside test modules/files, not only a `.env()` set or `==` compare |
| of those, build-time `env!` values (not user settings) | 3 | `VICTORY_TRANSLATOR`, `VICTORY_SHADER_SOURCE_HASH`, `VICTORY_SHADER_SWITCHES` |
| of those, set by `victory` itself for its own child processes | 2 | `VICTORY_AUDIO_CAPTURE_OF`, `VICTORY_RELAUNCHED_FROM` |
| read only by tests / benchmarks / test child processes | 30 | [Test-only](#test-only-and-child-process-names) |
| in a `.rs` comment only, never read | 1 | `VICTORY_READ_WATCH` (`crates/victory-gpu/src/texback.rs:7`, a past run) |
| in docs only, never in code (stale) | 1 | `VICTORY_MERGE_RACE` (`docs/README-fxceless.md`) |
| set by a launcher | 28 | [Launcher summary](#what-the-launchers-set) |
| other (non-`VICTORY_`) variables read | 7 (4 by product code) | [Other variables](#other-variables) |

322 + 30 + 1 = 353 names in `.rs`; + 1 doc-only = 354. (The six
`VICTORY_WRITERS*` names were added on 2026-11-26 without re-running the
classification; `crates/victory-gpu/src/writers.rs` counts once it is
tracked.)

How the tables were made (re-run to re-verify): every `"VICTORY_[A-Z0-9_]+"`
literal in `git ls-files '*.rs'` was classified as a test site if it lies inside
a `#[cfg(test)] mod … { }` block, a `*_tests.rs` / `tests.rs` file or a
`tests/` directory; as a set if it is an argument of `.env(` / `set_var` /
`remove_var`; as a reference if it is compared with `==` or is a match arm;
otherwise as a read. "Where read" lists up to four read sites (`+n` = more).
"Values & default" was derived mechanically from the read expression and then
hand-checked for about 70 names; for the rest, open the site before relying
on an edge case. "Effect" quotes the nearest comment that names the variable
(within 30 lines above the read), else the read's own trailing comment, the
enclosing function's doc, or the module doc (prefixed `fn doc:` / `module:`).
`diag build` means a `feature = "diagnostics"` gate appears within 8 lines
above the read; module-level gating (for example the whole `replay` and
`capture` paths) is not detected by that, so absence of the mark does not
prove a release build reads it.

## How a switch's value is interpreted

There is no single convention. The helper used at the read site decides:

| form at the read site | meaning | defined at |
|---|---|---|
| `crate::switch::on("X")` | on if set and not exactly `0` (`""`, `true`, `2`, `00` are all on) | `crates/victory-gpu/src/switch.rs:14` |
| `crate::toggled!("X")` | on unless exactly `0`; read once; in diagnostics builds `VICTORY_AB` may drive it | `crates/victory-gpu/src/switch.rs:24` |
| `asked_for!("X")` | on only if exactly `1`; `VICTORY_AB` may drive it (diag) | `crates/victory-gpu/src/titlewrite.rs:45` |
| `crate::ab::pick("X")` | `Some(on/off)` only while `VICTORY_AB` names X; the caller falls back to its own read | `crates/victory-gpu/src/ab.rs:33` |
| raw `var_os(..).is_none_or(\|v\| v != "0")` | "true unless `0`" (default ON) | many |
| raw `var_os(..).is_some_and(\|v\| v == "1")` | "true only if `1`" (default OFF) | many |
| `match var(..).as_deref() { Ok("a") => …, _ => default }` | listed values; anything else is the default arm | many |

"true" in the tables is the value of the expression at the site; whether true
means "feature on" or "old behaviour" is in the Effect column (several switches
are written as "`=0`: as before"). Nearly every read is cached in a
`OnceLock`/`LazyLock` on first use, so changing a variable after start has no
effect (exceptions: `VICTORY_AB` phases).

## Mechanisms worth knowing

* **`VICTORY_AB=<seconds>,<switch>[,…]`** (`crates/victory-gpu/src/ab.rs`,
  diagnostics builds): up to 8 named switches are flipped through all
  combinations, phase by phase, at presents; each phase's frame times are
  logged. Only switches read through `toggled!`, `asked_for!` or `ab::pick`
  respond (7 direct `ab::pick` call sites plus those macros).
* **Shader-cache keying.** The runtime key is built in
  `crates/victory-gpu/src/shader_cache.rs`: `VICTORY_TRANSLATOR` (the BLAKE3
  of the translator's sources, `crates/victory-gpu/build.rs:44`) keys on the
  code, and a **hand-kept list of 12 switch names** (`shader_cache.rs:66-77`)
  folds each one's current value in, with the loop limit, optimiser bits and
  host facts. Only those 12 make a separate set of cache entries. **A new
  translator switch that changes output must be added to that list by hand**,
  or runs with it set will reuse translations made without it.
  `crates/victory-shader/build.rs:51-64` also collects every `"VICTORY_*"`
  literal into `VICTORY_SHADER_SWITCHES` for
  `victory_shader::emit::translation_fingerprint`
  (`crates/victory-shader/src/emit/mod.rs:283`), which would do this
  automatically, but that function has no callers: it is not part of the key.
  See [../crates/victory-shader.md](../crates/victory-shader.md) §6.
* **`play.ps1` clears the environment**: it removes every `VICTORY_*`
  variable except `VICTORY_CONFIG`, `VICTORY_GAME_DIR`, `VICTORY_CACHE_DIR`,
  `VICTORY_DATA_DIR` before applying its own set (`scripts/play.ps1:33-35`);
  extra switches must go through `-More @{NAME='value'}`.
  `Play-VIctory.cmd` instead only fills in what is not already defined
  (`if not defined …`), so a user's own values win there.

## What the launchers set

| name | `scripts/play.ps1` | `Play-VIctory.cmd` |
|---|---|---|
| `VICTORY_TEXTURE_NO_WAIT` | `1` | `1` |
| `VICTORY_RELAY` | `2` | `2` |
| `VICTORY_WIDE_BACK` | `1` | `1` |
| `VICTORY_CARD_REHASH_RUNS` | `1` | `1` |
| `VICTORY_JOIN_COPIES` | `1` | `1` |
| `VICTORY_INDIRECT_WINDOW` | `1` | `1` |
| `VICTORY_RELAY_COPY` | `1` | `1` |
| `VICTORY_CARD_STEPS` | `2` | `2` |
| `VICTORY_INDIRECT_BATCH` | `2` | `2` |
| `VICTORY_MAP_THREADS` | `1` | `1` |
| `VICTORY_CARD_AWAKE` | `1` | `1` |
| `VICTORY_SHOW_AFTER` | `1` | `1` |
| `VICTORY_AMBIENT` | `0.8` | `0.8` |
| `VICTORY_THIN` | `1` (not `-Safe`) | `1` |
| `VICTORY_JOIN_PARTS` | `8` (not `-Safe`) | `8` |
| `VICTORY_UPLOAD_AHEAD` | `1` (not `-Safe`) | `1` |
| `VICTORY_TITLE_PRIORITIES` | `0` | – |
| `VICTORY_D3D12_DRED` | `light` (diag builds only read it) | – |
| `VICTORY_SKIP_PIPELINES` | `none` (not a 16-digit hash: a diag build logs a warning and skips nothing) | – |
| `VICTORY_UNDER_WIDE` | `0` (`-Safe` only) | – |
| `VICTORY_LAND_ECHO` | `0` (`-Safe` only) | – |
| `VICTORY_BVH_BUILDER` | `0` (`-NoRayTracing` only; on by default since 2026-11-26) | – |
| `VICTORY_AMBIENT_AIR` | – | `1.5` |
| `VICTORY_LEVELS_ON_DEMAND` | – | `1` |
| `VICTORY_CARD_BUFFERS_MIN_KB` | – | `64` |
| `VICTORY_CACHE_DIR` | kept from caller | `%LOCALAPPDATA%\VIctory\cache-pareto` if unset |
| `VICTORY_GAME_DIR` | kept from caller | `%~dp0..\GTA6.Xbox.v1.0.2.1` if unset |
| `VICTORY_LOG` | removed | `warn` if neither it nor `RUST_LOG` is set |
| `VICTORY_CONFIG`, `VICTORY_DATA_DIR` | kept from caller | not touched |

Why each is set is not stated in the scripts beyond `play.ps1:1-7`; the per-switch
reasons are in the read-site comments in the tables below and in
[docs/findings.md](../findings.md).

## Other variables

| name | where read | effect |
|---|---|---|
| `RUST_LOG` | `crates/victory-core/src/logging.rs:22` | log filter used only when `VICTORY_LOG` is unset or invalid |
| `LOCALAPPDATA` | `crates/victory-core/src/config.rs:139` | base of the default cache/data dirs (`<it>\victory\{cache,data}`); also used by `Play-VIctory.cmd:19` |
| `XDG_CACHE_HOME` | `crates/victory-core/src/config.rs:140` | fallback base when `LOCALAPPDATA` is unset |
| `HOME` | `crates/victory-core/src/config.rs:142` | last fallback (`$HOME/.cache`) |
| `SystemRoot` | `crates/victory-hle/src/modules/media.rs:609` | test only (path-spelling test) |
| `TEST_UNWATCHED` | `crates/victory-gpu/src/host/card_steps.rs:1227` | test only |
| `BIG_FOLDER` | `crates/victory-gpu/tests/replay_big.rs:347` | test only: folder of a large replay fixture |

Cargo-provided (`CARGO_MANIFEST_DIR`, `CARGO_BIN_EXE_victory`, `CARGO`,
`CARGO_TARGET_TMPDIR`) are compile- or test-time only and not listed.

## Product switches by subsystem

Launcher column: `play.ps1=` / `cmd=` value set by `scripts/play.ps1` /
`Play-VIctory.cmd`, or `neither`.

### apps/victory (CLI) (15)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_CRASH_SAVE` | `apps/victory/src/run.rs:212` | `<rva>:<len>,...` hex; unset = off; diag build | `VICTORY_CRASH_SAVE=<rva>:<len>,...` (hex) writes those title ranges to `crash-<rva>.bin` in the cache dir on a crash. | neither |
| `VICTORY_DUMP_MATCH` | `apps/victory/src/translate.rs:500` | `<dir>;<text>\|<text>...`; unset = off (no `ensure_outside_repo` check) | `VICTORY_DUMP_MATCH=<dir>;<text>\|<text>...`: the containers whose line holds one of the texts (a name, `#hash`), as the title has them. | neither |
| `VICTORY_EMIT_DUMP` | `apps/victory/src/translate.rs:244`, `apps/victory/src/translate.rs:696`, `apps/victory/src/translate.rs:743` | `<dir>`; unset = off; checked by `ensure_outside_repo` (translate.rs:245) | With `translate --list`: every emitted shader (and validator report) written as a file into the folder. | neither |
| `VICTORY_FAIL_DUMP` | `apps/victory/src/translate.rs:529` | `<dir>`; unset = off (no `ensure_outside_repo` check) | `VICTORY_FAIL_DUMP=<dir>`: the first 3 containers of each failure reason, as the title has them (`.cdxil`), to work on a translator gap offline | neither |
| `VICTORY_HOST_MAX_WAVE` | `apps/victory/src/translate.rs:250` | integer lanes | `translate` (non-`--warm` path): translate as for a host whose waves have at most this many lanes. Same setter as `VICTORY_HOST_WAVE`. | neither |
| `VICTORY_HOST_RDNA` | `apps/victory/src/translate.rs:254` | true only if `1` | `VICTORY_HOST_RDNA=1`: as for an AMD RDNA card (`VICTORY_RDNA_FLOAT`). | neither |
| `VICTORY_HOST_WAVE` | `apps/victory/src/main.rs:208` | integer lanes (e.g. `32`) | `translate`: translate as for a GPU whose waves are at most this many lanes (`set_host_max_wave`). Same effect as `VICTORY_HOST_MAX_WAVE`. | neither |
| `VICTORY_PAIRS` | `apps/victory/src/translate.rs:267` | `vs+ps,...` label substrings | translate: shaders named here are kept and built into host pipelines as vertex+pixel pairs after the walk (translate.rs:148). | neither |
| `VICTORY_POKE` | `apps/victory/src/run.rs:269` | `<rva>:<byte>,...` hex; unset = off; diag build | `VICTORY_POKE=<rva>:<byte>,...` (hex) writes the byte at each title+rva twice a second. Code pages (execute-read) are made execute-read-write at start so an instruction byte can be patched; a `VIctory: poke title+<rva>: page protection was ...` line says which. | neither |
| `VICTORY_SHADER_CENSUS` | `apps/victory/src/translate.rs:642`, `crates/victory-shader/src/emit/bitcode/census.rs:63` | path; unset = off | `VICTORY_SHADER_CENSUS=<file>`: one line per emitted program, appended: its label, the console program's instructions by encoding class (a static count of the whole program, not of one thread's path), and the host module's ... | neither |
| `VICTORY_SKIP_CPU_CHECK` | `apps/victory/src/run.rs:8` | only `1` skips | `victory run --execute` refuses a CPU without AVX2 unless this is `1` (for CPU emulators such as Intel SDE). | neither |
| `VICTORY_TRANSLATE_ONLY` | `apps/victory/src/translate.rs:475` | `<hash prefix>,...`; unset = all | `VICTORY_TRANSLATE_ONLY=<hash prefix>,...` translates those containers alone. | neither |
| `VICTORY_VIEW_LIMIT` | `apps/victory/src/main.rs:204` | number | `VICTORY_VIEW_LIMIT=<null slot>`: translate as on a host whose view heap is capped | neither |
| `VICTORY_WARM_SECONDS` | `apps/victory/src/translate.rs:176` | u64 | `VICTORY_WARM_SECONDS=<n>`: stop taking new shaders after n seconds (what is done stays in the cache; the next warm-up goes on from there). | neither |
| `VICTORY_WATCH` | `apps/victory/src/run.rs:232` | `<rva>,...` hex; unset = off; diag build | `VICTORY_WATCH=<rva>,...` (hex) logs the u32 at each title+rva whenever it changes, polled every 5 ms (an engine's state, say). | neither |

### victory-core (6)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_CACHE_DIR` | `crates/victory-core/src/config.rs:88` | path; empty = unset; default `%LOCALAPPDATA%\victory\cache` | Root of all derived data (shader cache, prepared images, pipelines). Beats `[paths].cache_dir`. | cmd=`%LOCALAPPDATA%\VIctory\cache-pareto` |
| `VICTORY_CONFIG` | `crates/victory-core/src/config.rs:125` | path; empty = unset; default `./victory.toml` (CWD) | Config file to read; error if it names a missing file. | neither |
| `VICTORY_DATA_DIR` | `crates/victory-core/src/config.rs:91` | path; empty = unset; default `%LOCALAPPDATA%\victory\data` | Saves, title storage, temp drive, `crashes.log`. Beats `[paths].data_dir`. | neither |
| `VICTORY_GAME_DIR` | `crates/victory-core/src/config.rs:95` | path; empty = unset; no default | The user's dump; default input of census/shader-info/translate/shaders. Beats `[paths].game_dir`. | cmd=`%~dp0..\GTA6.Xbox.v1.0.2.1` |
| `VICTORY_LOG` | `crates/victory-core/src/logging.rs:21` | `EnvFilter` syntax; falls back to `RUST_LOG`; default `info` | Log filter. Debug/trace exist only in `diagnostics` builds (release caps at info). | cmd=`warn` |
| `VICTORY_TEST_TITLES` | `crates/victory-core/src/config.rs:82` | `;`-separated paths (`std::env::split_paths`); empty = unset | Other titles for platform tests; replaces `[paths].test_titles` wholesale. | neither |

### victory-gpu / ab (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_AB` | `crates/victory-gpu/src/ab.rs:26` | `<seconds>,<switch>[,<switch>..]` (seconds > 1, 1-8 switches); diag build | `VICTORY_AB=<seconds>,<switch>[,<switch>..]`: every so many seconds, at a present, the next phase begins; switch number n is on in the phases whose number has bit n, so that all combinations come one after another, again and ... | neither |

### victory-gpu / ambient (3)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_AMBIENT` | `crates/victory-gpu/src/ambient.rs:97` | unset/`0`/`off`: title output; `<share>` (1 = all); `constant:<level>` | Stand-in for the indirect light the title traces rays for when the BVH builder is off (ambient.rs module doc). | play.ps1=`0.8`; cmd=`0.8` |
| `VICTORY_AMBIENT_AIR` | `crates/victory-gpu/src/ambient.rs:110` | number; default `0.0` | The most the air scatters, as a multiple of the light around (`VICTORY_AMBIENT_AIR=<multiple>`; unset or 0: what the title has). | cmd=`1.5` |
| `VICTORY_AMBIENT_SHADE` | `crates/victory-gpu/src/ambient.rs:120` | number | Strength and radius of the creases' shade. | neither |

### victory-gpu / book (2)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_BOOK` | `crates/victory-gpu/src/book.rs:77` | true unless `0` | module: | neither |
| `VICTORY_SLOT_CHECK` | `crates/victory-gpu/src/book.rs:86` | true if set and not `0` | `VICTORY_SLOT_CHECK=1`. | neither |

### victory-gpu / capture (6)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_CAPTURE` | `crates/victory-gpu/src/capture.rs:15` | path; unset = off | Presented frames written to PNG files, to see what reaches the screen: with `VICTORY_CAPTURE=<folder>`, every frame whose number is a power of two (the frames the log reports) is saved as `frame-<n>.png`, at half size. | neither |
| `VICTORY_CAPTURE_CONSTANTS` | `crates/victory-gpu/src/draw.rs:4826` | `<16-hex shader hash>,...`; unset = off; diag build | For the shaders listed, the ops listing of a captured frame gives every 32-bit word (the first 4 KiB) of the constant buffers and placed buffer views they read, as ` words [...]`, not only the first four values. | neither |
| `VICTORY_CAPTURE_FRAMES` | `crates/victory-gpu/src/capture.rs:25` | number | Frames listed in `VICTORY_CAPTURE_FRAMES` (comma-separated): saved too, and their draws traced at debug level. | neither |
| `VICTORY_CAPTURE_WORLD` | `crates/victory-gpu/src/capture.rs:120` | true if set (any value); diag build | Whether `VICTORY_CAPTURE_WORLD` is set. | neither |
| `VICTORY_CAPTURE_WORLD_ALSO` | `crates/victory-gpu/src/capture.rs:43` | number | `VICTORY_CAPTURE_WORLD_ALSO` (comma-separated): more frames counted from that first one (the title's blue patches show from about the 118th frame on, when the window's model has arrived and the reflections hold values: none of ... | neither |
| `VICTORY_DUMP_TARGETS` | `crates/victory-gpu/src/capture.rs:361` | on if set and not `0` | With `VICTORY_DUMP_TARGETS` set: title textures the frames listed in `VICTORY_CAPTURE_FRAMES` render to or write, in first-use order, saved at that frame's present as `frame-<n>-<i>-<resource>-<w>x<h>-f<format>.png` (what each ...). 1D tables of 32-bit floats logged as numbers (distinct values, equal neighbours, largest step, 17 samples); 3D textures (colour look-up tables) as their slices side by side, each texel doubled, `...-<w>x<h>x<d>-f<format>-slices.png`, with a log line counting equal neighbours along x, y and z and listing the grey diagonal | neither |

### victory-gpu / copystat (3)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_BARRIER_TEST` | `crates/victory-gpu/src/copystat.rs:275` | one of `groups`, `indirect`, `both`; else default arm | Whether an experiment on barriers is on (`VICTORY_BARRIER_TEST`): bit 0 for `groups` (the title's barrier groups record no wait for earlier shader writes), bit 1 for `indirect` (the pass that bounds an indirect command runs ... | neither |
| `VICTORY_COPY_MODE` | `crates/victory-gpu/src/copystat.rs:179` | one of `plain`, `none`; else default arm | How copies run: 0 guarded (as always), 1 plain, 2 not at all. | neither |
| `VICTORY_MEMORY_MAP` | `crates/victory-gpu/src/copystat.rs:532` | true unless `0` | `VICTORY_MEMORY_MAP=0` leaves the walk out. | neither |

### victory-gpu / cores (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_FAST_CORES` | `crates/victory-gpu/src/cores.rs:48` | number; default `2` | `VICTORY_FAST_CORES`: 0: no thread asks; 1: the threads a frame waits for one by one; 2 (the default): the threads they share their work with too; 3: those that work beside the rest as well (lists translated, descriptors ... | neither |

### victory-gpu / descriptor (19)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_DYNAMIC_SLOTS` | `crates/victory-gpu/src/descriptor/host.rs:1494` | number; default `DYNAMIC << 2` | `VICTORY_DYNAMIC_SLOTS` sets the most). 2^18 ran out in the title's bigger places (session play-1125-0127: 27,000 to 35,000 tables a frame, up to 3,285 of them refused, with 2,639 indirect draws and dispatches). | neither |
| `VICTORY_HEAP_SYNC` | `crates/victory-gpu/src/descriptor.rs:168`, `crates/victory-gpu/src/descriptor/host.rs:3180` | true unless `0` | Whether [`sync_heaps`] is done (`VICTORY_HEAP_SYNC=0`: not). | neither |
| `VICTORY_HEAP_SYNC_ASIDE` | `crates/victory-gpu/src/descriptor.rs:183` | true unless `0` | Whether a submission's [`sync_heaps`] is done by a thread of its own beside the rest of the preparing (`VICTORY_HEAP_SYNC_ASIDE=0`: in front of it, as before). | neither |
| `VICTORY_HEAP_SYNC_EARLY` | `crates/victory-gpu/src/descriptor.rs:195` | number; default `4u64` | How long that thread waits for a submission before it makes a round of its own (`VICTORY_HEAP_SYNC_EARLY=<ms>`, 4 if not given; | neither |
| `VICTORY_KEEP_SHOWN` | `crates/victory-gpu/src/descriptor/host.rs:4482`, `crates/victory-gpu/src/descriptor/host.rs:4486`, `crates/victory-gpu/src/popwatch.rs:202` | number; default `1`; diag build; `VICTORY_AB`-switchable | Whether a slot that is written with a view of another texture goes on showing the one it shows until the new one is made and its upload has run (`VICTORY_KEEP_SHOWN`): 0: never, it reads as zeros, then as the neutral texture; ... | neither |
| `VICTORY_KEEP_VIEWS` | `crates/victory-gpu/src/descriptor/host.rs:4410` | true unless `0` | `VICTORY_KEEP_VIEWS=0` makes it null at once, as before. | neither |
| `VICTORY_LENT_HEAP` | `crates/victory-gpu/src/descriptor.rs:591` | true unless `0` | `VICTORY_LENT_HEAP=0`: none, such a dispatch is dropped, as before. | neither |
| `VICTORY_LIST_VIEWS` | `crates/victory-gpu/src/descriptor/host.rs:5869` | default ON; `0` off | Whether a list makes a view its tables hold once (`VICTORY_LIST_VIEWS=0`: for every table). | neither |
| `VICTORY_QUICK_MIRROR` | `crates/victory-gpu/src/descriptor/host.rs:4571` | true unless `0` | Whether a slot already made from its record is told so without the lock (`VICTORY_QUICK_MIRROR=0`: with it, as before). | neither |
| `VICTORY_RAW_VIEWS` | `crates/victory-gpu/src/descriptor/host.rs:1418` | true unless `0` | `VICTORY_RAW_VIEWS=0`: they read the title's slots themselves, as before). | neither |
| `VICTORY_SHOW_AFTER` | `crates/victory-gpu/src/descriptor/host.rs:292` | true if set and not `0` | Whether a title slot shows a texture that is new to it only once the submission with its upload has finished (`VICTORY_SHOW_AFTER=1`; off). | play.ps1=`1`; cmd=`1` |
| `VICTORY_SLOT_HOLDS` | `crates/victory-gpu/src/descriptor/host.rs:3064` | true unless `0` | `VICTORY_SLOT_HOLDS=0`: only the list holds it, as before. | neither |
| `VICTORY_STALE_TEXTURES` | `crates/victory-gpu/src/descriptor/host.rs:834` | one of `0`, `overlap`; else default arm | `VICTORY_STALE_TEXTURES=0`: never, as before; | neither |
| `VICTORY_TABLED_TEXTURES` | `crates/victory-gpu/src/descriptor/host.rs:920` | true unless `1` | `VICTORY_TABLED_TEXTURES=1` keeps them in it as before. | neither |
| `VICTORY_TEXTURE_AWAY_MS` | `crates/victory-gpu/src/descriptor/host.rs:2175` | number; default `AWAY_MS` | Only when it is still away `AWAY_MS` after it was first found away (`VICTORY_TEXTURE_AWAY_MS`; 0: at once, as before). | neither |
| `VICTORY_VIEW_BYTES` | `crates/victory-gpu/src/descriptor/host.rs:1254` | true unless `0` | `VICTORY_VIEW_BYTES=0` leaves them null, as before. | neither |
| `VICTORY_VIEW_COPY` | `crates/victory-gpu/src/descriptor/host.rs:3452` | true only if `0` | module: | neither |
| `VICTORY_VIEW_EXTENT` | `crates/victory-gpu/src/descriptor/host.rs:3603` | true unless `0` | `VICTORY_VIEW_EXTENT=0` looks the copy up by the view's range alone, as before. | neither |
| `VICTORY_VIEW_INLINE` | `crates/victory-gpu/src/descriptor.rs:897` | true unless `0` | `VICTORY_VIEW_INLINE=0` makes every view a table entry, as before. | neither |

### victory-gpu / draw (25)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_BAKE_EXACT` | `crates/victory-gpu/src/draw.rs:198` | true unless `0` | Whether a draw of the title's bake finds the textures it picks by index as title memory has them at that moment (`VICTORY_BAKE_EXACT=0`: as the pass leaves them, as before). | neither |
| `VICTORY_BAKE_TEXTURES_MS` | `crates/victory-gpu/src/draw.rs:182` | ms; default `1500` | The milliseconds of every frame the pass that uploads textures picked by index takes before a shader that picks by index has asked for it (`VICTORY_EARLY_TEXTURES_MS`, 12; 0: never, as before), and the milliseconds it may take ... | neither |
| `VICTORY_BIND_CACHE` | `crates/victory-gpu/src/draw.rs:1189` | true unless `0`; diag build | module: | neither |
| `VICTORY_CARD_ACCOUNTING` | `crates/victory-gpu/src/draw.rs:2432` | true only if `1` | Off unless `VICTORY_CARD_ACCOUNTING=1`; then at the first pass that finds the card without room, and every five seconds after it. | neither |
| `VICTORY_EARLY_TEXTURES_MS` | `crates/victory-gpu/src/draw.rs:182` | ms; default `12`; `0` never | The milliseconds of every frame the pass that uploads textures picked by index takes before a shader that picks by index has asked for it (`VICTORY_EARLY_TEXTURES_MS`, 12; 0: never, as before), and the milliseconds it may take ... | neither |
| `VICTORY_FULL_CARD_PASS` | `crates/victory-gpu/src/draw.rs:777` | true unless `0` | Whether a pass over the textures picked by index on a full card holds back the slots that need a new host copy (`VICTORY_FULL_CARD_PASS=0`: no, as before: they stay due, each pass looks at the same ones again and stops at the ... | neither |
| `VICTORY_HEAP_RECHECK` | `crates/victory-gpu/src/draw.rs:792` | true unless `0` | Whether textures picked by index are looked at again after their upload (`VICTORY_HEAP_RECHECK=0`: no). | neither |
| `VICTORY_HEAP_SPREAD` | `crates/victory-gpu/src/draw.rs:628` | true unless `0` | Whether the slots looked at again for no write are a part of them at every pass, each slot at every sixteenth (`VICTORY_HEAP_SPREAD=0`: all of them at every sixteenth pass, as before). | neither |
| `VICTORY_HEAP_TEXTURES` | `crates/victory-gpu/src/draw.rs:2514` | true unless `0` | `VICTORY_HEAP_TEXTURES=0` turns it off. | neither |
| `VICTORY_INDIRECT_BATCH` | `crates/victory-gpu/src/draw.rs:373` | `0`, `2`; else 1 | `VICTORY_INDIRECT_BATCH=0`: each on its own, or in windows ([`windows`])). | play.ps1=`2`; cmd=`2` |
| `VICTORY_INDIRECT_WINDOW` | `crates/victory-gpu/src/draw.rs:341` | true only if `1` | Whether indirect draws that read record after record of one argument buffer are clamped a window at a time (`VICTORY_INDIRECT_WINDOW=1`; off by default). | play.ps1=`1`; cmd=`1` |
| `VICTORY_LIST_SLOTS` | `crates/victory-gpu/src/draw.rs:4679` | list (see site); diag build | `VICTORY_LIST_SLOTS`, hash prefixes separated by commas. | neither |
| `VICTORY_MESH_TEST` | `crates/victory-gpu/src/draw.rs:533`, `crates/victory-gpu/src/draw.rs:547` | one of `skip`, `scissor`; else default arm | An experiment on mesh draws (`VICTORY_MESH_TEST`): 1 for `skip` (none is issued), 2 for `scissor` (each runs its shaders but rasterises one pixel), 0 otherwise. | neither |
| `VICTORY_READY_ONCE` | `crates/victory-gpu/src/draw.rs:4567` | default ON; `0` off | Whether a table a list reads again, with nothing changed in the list since, is left as it is (`VICTORY_READY_ONCE=0`: every entry is gone through again at every draw). | neither |
| `VICTORY_RECHECK_SOON` | `crates/victory-gpu/src/draw.rs:206` | true unless `0` | Whether a texture found changed with nothing to tell of it is looked at again in the next frame (`VICTORY_RECHECK_SOON=0`: after four, as before). | neither |
| `VICTORY_REUPLOAD_WHEN_FULL` | `crates/victory-gpu/src/draw.rs:785` | true unless `0` | Whether a texture that has its host copy is still brought up to date while the card has no room for new ones (`VICTORY_REUPLOAD_WHEN_FULL=0`: no). | neither |
| `VICTORY_ROOT_UAV_MB` | `crates/victory-gpu/src/draw.rs:1869` | u64 | False if the draw must be dropped. shortcut: a root view has no size: the view, the refresh and the copy back cover all that is left of the title's buffer (`VICTORY_ROOT_UAV_MB` sets a limit for all three). | neither |
| `VICTORY_ROOT_VIEW_MB` | `crates/victory-gpu/src/draw.rs:844` | u64 | `VICTORY_ROOT_VIEW_MB` sets a limit again.) | neither |
| `VICTORY_SCISSOR_TEST` | `crates/victory-gpu/src/draw.rs:524` | only `full` | An experiment on the title's scissor rectangles (`VICTORY_SCISSOR_TEST=full`): they are all widened to the whole target. | neither |
| `VICTORY_TEXTURE_CATCHUP` | `crates/victory-gpu/src/draw.rs:2635` | true unless `0` | `VICTORY_TEXTURE_CATCHUP=0` keeps the plain budget. | neither |
| `VICTORY_TEXTURE_STAGE_MB` | `crates/victory-gpu/src/draw.rs:2522` | u64 | `VICTORY_TEXTURE_STAGE_MB` sets it: a backlog of tens of thousands of textures (a scene's first minutes) then drains in fewer frames. | neither |
| `VICTORY_TEXTURE_STAGE_MS` | `crates/victory-gpu/src/draw.rs:2629` | number; default `250` | Also within a time per pass: the pass runs on a thread that records the frame, and making and staging 192 MB of textures took 1.6-2.2 s of each of the world's first twenty frames (run 20). | neither |
| `VICTORY_THREAD_DIMENSIONS` | `crates/victory-gpu/src/draw.rs:4404` | true unless `0` | `VICTORY_THREAD_DIMENSIONS=0`: such counts are taken for groups, as before. | neither |
| `VICTORY_UNWATCHED_FRAMES` | `crates/victory-gpu/src/draw.rs:996` | number; default `1` | Memory that is not write-watched is copied whole by every refresh. | neither |
| `VICTORY_WRITE_SKIP` | `crates/victory-gpu/src/draw.rs:274` | true unless `0` | `VICTORY_WRITE_SKIP=0` declares every such range, as before. | neither |

### victory-gpu / dxgi (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_DISPLAY_MODE` | `crates/victory-gpu/src/dxgi.rs:238` | u32 | `VICTORY_DISPLAY_MODE=WxH` if set (as a console reports the television it is plugged into: a Series X on a 1080p set lists a 1080p mode), else 3840x2160. shortcut: not yet the host display's own size by default; make it so ... | neither |

### victory-gpu / faultbook (2)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_FAULT_BOOK` | `crates/victory-gpu/src/faultbook.rs:87` | `<file>`; unset/empty = off | Fault book: records of guarded-memory events written to the file at the times in `VICTORY_FAULT_BOOK_AT` (inferred from faultbook.rs code). | neither |
| `VICTORY_FAULT_BOOK_AT` | `crates/victory-gpu/src/faultbook.rs:88` | seconds list; default `150,210,270` | When the fault book is written (seconds). | neither |

### victory-gpu / frame (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_LATE_WAITS` | `crates/victory-gpu/src/frame.rs:144` | true only if `1` | `VICTORY_LATE_WAITS=1` sleeps to the next tick, as before. | neither |

### victory-gpu / host (47)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_BURY_LAG` | `crates/victory-gpu/src/host.rs:501` | number; default 16 on limited host RAM, else 64; `0` off | `VICTORY_BURY_LAG=0`: not. | neither |
| `VICTORY_CARD_AWAKE` | `crates/victory-gpu/src/host/awake.rs:66` | `1` on; `turns` by turns; else off | With `VICTORY_CARD_AWAKE=turns`: this long on, this long off. | play.ps1=`1`; cmd=`1` |
| `VICTORY_CARD_REHASH_RUNS` | `crates/victory-gpu/src/host.rs:2147` | true only if `1` | Whether the pieces hashed again after ranges were sent up to a copy in the card's memory are hashed in runs of pieces that touch, each piece once (`VICTORY_CARD_REHASH_RUNS=1`; off by default), and not in a pass per range sent up. | play.ps1=`1`; cmd=`1` |
| `VICTORY_CARD_SPANS` | `crates/victory-gpu/src/host/spans.rs:69` | true only if `1` | Diagnostics: how long the card itself works on our submissions, by its own timestamps (`VICTORY_CARD_SPANS=1`). | neither |
| `VICTORY_CARD_STEPS` | `crates/victory-gpu/src/host/card_steps.rs:79` | `1`, `2`; else 0 (off) | The stage (`VICTORY_CARD_STEPS`; off by default): | play.ps1=`2`; cmd=`2` |
| `VICTORY_COPIES_IN_PLACE` | `crates/victory-gpu/src/host/place.rs:43` | true unless `0` | The title's buffer copies, done on the card at their place in the list (`VICTORY_COPIES_IN_PLACE`, on by default; | neither |
| `VICTORY_COPY_ASK` | `crates/victory-gpu/src/host.rs:8269` | true only if `1` | `VICTORY_COPY_ASK=1` asks first, as before. | neither |
| `VICTORY_D3D12_BACKGROUND` | `crates/victory-gpu/src/host.rs:919` | true only if `0` | `VICTORY_D3D12_BACKGROUND=0`: the runtime's background work (the driver compiling shaders again on the "D3D Background Thread"s, for minutes after pipelines were made or loaded) is switched off. | neither |
| `VICTORY_D3D12_DEBUG` | `crates/victory-gpu/src/host.rs:849`, `crates/victory-shader/src/emit/host.rs:73` | on if set and not `0`; diag build | The debug layer only in builds with diagnostics. | neither |
| `VICTORY_D3D12_DRED` | `crates/victory-gpu/src/host.rs:1668`, `crates/victory-gpu/src/host.rs:1678`, `crates/victory-gpu/src/host.rs:1693` | `1`/any non-`0`: full; `light`; `faults`; diag build | Whether device-removed data is on (`VICTORY_D3D12_DRED`, diagnostics builds). | play.ps1=`light` |
| `VICTORY_D3D12_GBV` | `crates/victory-gpu/src/host.rs:860` | on if set and not `0` | GPU-based validation (`VICTORY_D3D12_GBV`): out-of-range descriptor heap indices and the like become messages naming the command instead of page faults. | neither |
| `VICTORY_D3D12_WARP` | `crates/victory-gpu/src/host.rs:898` | on if set and not `0`; diag build | Diagnostics: the software adapter, on which a shader's wild access is an access violation in this process (a crash report with the address and instruction) instead of a GPU fault and driver reset. | neither |
| `VICTORY_DECLARE_ONCE` | `crates/victory-gpu/src/host.rs:3797` | true unless `0` | See [`HostList::defer_gpu_write`]. | neither |
| `VICTORY_DIRECT_WRITES` | `crates/victory-gpu/src/host.rs:8460` | true unless `0` | `VICTORY_DIRECT_WRITES=0` writes as before. | neither |
| `VICTORY_FETCH_CHECK` | `crates/victory-gpu/src/host/torn.rs:35` | true only if `1` | `VICTORY_FETCH_CHECK=1`; off by default until a run has shown how often a piece is caught (the frame-rate line says so) and what the hashes cost (3-4 ms for 96 MB beside the merge, spread over the threads that land). | neither |
| `VICTORY_FILL_TITLE` | `crates/victory-gpu/src/host.rs:2809` | true only if `1` | Whether a small fill of upload memory, or copy of a title buffer, goes on without waiting for flights when the title itself wrote the memory since they were given it (`VICTORY_FILL_TITLE=1`; off by default). | neither |
| `VICTORY_GPU_TIMING` | `crates/victory-gpu/src/host.rs:1747`, `crates/victory-gpu/src/host.rs:2381` | on if set and not `0`; diag build | Whether each list of a submission runs and is timed on its own, slow ones logged with their commands (`VICTORY_GPU_TIMING`, diagnostics builds; the commands are noted with `VICTORY_D3D12_DRED` on). | neither |
| `VICTORY_HEAP_PASS_ASIDE` | `crates/victory-gpu/src/host/aside.rs:47` | true unless `0` | `VICTORY_HEAP_PASS_ASIDE=0`: done where it is needed, as before. | neither |
| `VICTORY_INDIRECT_CARD_BUDGET` | `crates/victory-gpu/src/host.rs:1738` | true unless `0` | Whether a list's indirect-work budget lives in the card's own memory (`VICTORY_INDIRECT_CARD_BUDGET=0` keeps it in memory the CPU maps, as before). | neither |
| `VICTORY_JOIN_COPIES` | `crates/victory-gpu/src/host.rs:3773` | true only if `1` | Whether a title buffer copy that continues the one recorded before it in its list becomes one step with it (`VICTORY_JOIN_COPIES=1`; off by default). | play.ps1=`1`; cmd=`1` |
| `VICTORY_JOIN_LISTS` | `crates/victory-gpu/src/host.rs:3095` | default `48` | `VICTORY_JOIN_LISTS`, 48). | neither |
| `VICTORY_JOIN_PARTS` | `crates/victory-gpu/src/host.rs:3095`, `crates/victory-gpu/src/host.rs:3097` | default `1`; `VICTORY_AB`-switchable | How many parts of submissions, and lists, the line's thread gives to the host as one (`VICTORY_JOIN_PARTS=<n>`; off by default, 1; | play.ps1=`8 (not -Safe)`; cmd=`8` |
| `VICTORY_KEEP_FREED` | `crates/victory-gpu/src/host.rs:367` | true unless `0` | `VICTORY_KEEP_FREED=0`: it is given back as before). | neither |
| `VICTORY_LATE_WAIT` | `crates/victory-gpu/src/host.rs:2384` | `1`, `2`, `3`; else 0 (off) | `VICTORY_LATE_WAIT`: 0 (off), 1, or 2 ([`late_exact`]). | neither |
| `VICTORY_LATE_WAIT_LISTS` | `crates/victory-gpu/src/host.rs:2610` | number; default `256` | The most host lists in the air before the flights are settled (`VICTORY_LATE_WAIT_LISTS`, 256; | neither |
| `VICTORY_LATE_WAIT_WRITES` | `crates/victory-gpu/src/host.rs:2620` | true unless `0` | Whether a submission with bottom-of-pipe writes may fly (`VICTORY_LATE_WAIT_WRITES=0`: it is settled as soon as it is sent, so the title is told at once, as before; for telling a title that polls such a value from one that ... | neither |
| `VICTORY_LAZY_PRELUDE` | `crates/victory-gpu/src/host.rs:1225` | true unless `0` | `VICTORY_LAZY_PRELUDE=0`: made with the list, as before. | neither |
| `VICTORY_PANEL` | `crates/victory-gpu/src/host/panel.rs:39` | on if set and not `0` | The texts are put into one of these in turn: the card reads the others, at most half a second old. | neither |
| `VICTORY_PLACED_ORDER` | `crates/victory-gpu/src/host/card_steps.rs:99`, `crates/victory-gpu/src/host/card_steps.rs:103` | on if set and not `0`; diag build; `VICTORY_AB`-switchable | Whether a copy at its place in a list ([`place`]) is only in the way of what is done on the card in front of the lists if that stands behind it in them (`VICTORY_PLACED_ORDER=1`; off: wherever it stands), and a submission ... | neither |
| `VICTORY_REFRESH_AHEAD` | `crates/victory-gpu/src/host.rs:7679` | true unless `0` | Whether the copies that a submission's shaders write are brought up to date together, on several threads, ahead of its steps (`VICTORY_REFRESH_AHEAD=0`: each before the step that gives it to them). 580 copies a frame, one ... | neither |
| `VICTORY_REFRESH_FRONT` | `crates/victory-gpu/src/host.rs:7698` | on if set and not `0` | Whether everything a submission refreshes is brought up to date in one go, on several threads, ahead of its steps (`VICTORY_REFRESH_FRONT=1`, and with `VICTORY_JOIN_PARTS`, which needs it; else in groups where the steps ask ... | neither |
| `VICTORY_RELAY` | `crates/victory-gpu/src/host.rs:2484`, `crates/victory-gpu/src/shadow.rs:1107` | unset or `2`: stage 2; `1`: stage 1 (when the relay is on) | Stage 2 unless `VICTORY_RELAY=1` asks for the first one (the default since the walls of the first room were found drawn in every run with this set and in fewer than half without it). | play.ps1=`2`; cmd=`2` |
| `VICTORY_RELAY_COPY` | `crates/victory-gpu/src/host.rs:2505` | true only if `1` | Whether the pieces the relay fetches from the card's memory come over the card's copy queue (`VICTORY_RELAY_COPY=1`) and not over the queue that draws. | play.ps1=`1`; cmd=`1` |
| `VICTORY_SCRATCH_POOL` | `crates/victory-gpu/src/host.rs:1866` | true unless `0` | `VICTORY_SCRATCH_POOL=0`: chunks of 4 MiB kept by their list, as before. | neither |
| `VICTORY_SEND_BAR` | `crates/victory-gpu/src/host/send.rs:135` | true only if `1` | Whether the memory lies in the card's memory, written by the CPU through the bus (`VICTORY_SEND_BAR=1`): the copies on the card's drawing queue, which nothing overlaps, then cross no bus. | neither |
| `VICTORY_SEND_OWN` | `crates/victory-gpu/src/host/send.rs:27` | true unless `0` | What goes up to copies in the card's memory goes up as it was when its submission was prepared (`VICTORY_SEND_OWN`, on by default; | neither |
| `VICTORY_SWEEP` | `crates/victory-gpu/src/host.rs:428` | true unless `0` | `VICTORY_SWEEP=0` keeps everything, as before. | neither |
| `VICTORY_TEXTURE_KEEP` | `crates/victory-gpu/src/host.rs:179`, `crates/victory-gpu/src/resource.rs:2037` | true unless `0` | `VICTORY_TEXTURE_KEEP=0`: 5 s there too, as before. | neither |
| `VICTORY_TRAIL` | `crates/victory-gpu/src/host.rs:540` | true unless `0` | `VICTORY_TRAIL=0`: nothing is kept and nothing is said. | neither |
| `VICTORY_UNDER_WORDS` | `crates/victory-gpu/src/host/send.rs:60` | true unless `0` | Whether the title's bytes that go in under flights' writes (`Shadow::merge_under_writes`) go in by whole words (`VICTORY_UNDER_WORDS=0`: by the bytes that changed, as before). | neither |
| `VICTORY_UPCOPY_CHECK` | `crates/victory-gpu/src/host/upcheck.rs:33` | true only if `1` | Diagnostics (`VICTORY_UPCOPY_CHECK=1`; off by default): whether what goes up to a copy in the card's memory is older than what the card holds. | neither |
| `VICTORY_WAIT_RESIDENT` | `crates/victory-gpu/src/host.rs:220` | true unless `0` | `VICTORY_WAIT_RESIDENT=0`: as before). | neither |
| `VICTORY_WALLS_TRACE` | `crates/victory-gpu/src/host.rs:3837`, `crates/victory-hle/src/modules/dstorage.rs:659` | `1` or `2`; off by default | The walls trace (`VICTORY_WALLS_TRACE=1` or `2`; off by default): log lines that say what the title's CPU was given while it decided whether the camera is in a room. | neither |
| `VICTORY_WIDE_FILLS` | `crates/victory-gpu/src/host.rs:7868` | true unless `0` | Whether the fills of a submission's upload memory are done together on several threads (`VICTORY_WIDE_FILLS=0`: one after the other, where the lists name them). | neither |
| `VICTORY_WIDE_MERGE` | `crates/victory-gpu/src/host.rs:2166` | true only if `1` | Whether what shaders wrote to copies in the card's memory is merged into title memory on several threads (`VICTORY_WIDE_MERGE=1`; off by default). | neither |
| `VICTORY_WIDE_REFRESH` | `crates/victory-gpu/src/host.rs:7607` | `1` force on, `0` off; default on with >= 4 logical CPUs | Whether a submission's refreshes of host copies run on several threads, a copy to a thread (`VICTORY_WIDE_REFRESH=0` disables it; | neither |
| `VICTORY_WIDE_TEXTURES` | `crates/victory-gpu/src/host.rs:7886` | true unless `0` | Whether a submission's textures are brought up to date on several threads (`VICTORY_WIDE_TEXTURES=0`: one after the other, where the lists name them, as before). | neither |

### victory-gpu / indirect (2)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_INDIRECT_REST` | `crates/victory-gpu/src/indirect.rs:301` | only `1` | Whether a record that does not fit in what is left of its list's budget takes the rest, so that every record after it runs empty too (`VICTORY_INDIRECT_REST=1`; off by default so one oversized record takes nothing and later ... | neither |
| `VICTORY_INDIRECT_WIDE` | `crates/victory-gpu/src/indirect.rs:280` | true unless `0` | The most groups in x that an indirect dispatch runs as the title asks: 65,535 is the most D3D12 names for a dispatch, 2^20 ([`SOURCE`]'s GROUPS) what is bounded here anyway (`VICTORY_INDIRECT_WIDE=0`: 65,535; a record that ... | neither |

### victory-gpu / inplace (4)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_INPLACE` | `crates/victory-gpu/src/inplace.rs:89` | number; default `0` | `VICTORY_INPLACE`: 0 (unset) off. 1: a window of title memory outside buffers that lies in private memory committed whole is not copied: a part of the title's allocation is opened as a host heap and the window is a buffer ... | neither |
| `VICTORY_INPLACE_MB` | `crates/victory-gpu/src/inplace.rs:97` | u64; default `4096` | The most title memory in place at once, in bytes (`VICTORY_INPLACE_MB`, 4096): opening locks the pages into the PC's memory, also those the title never touched. | neither |
| `VICTORY_INPLACE_PROBE` | `crates/victory-gpu/src/inplace.rs:62` | number; default `0` | `VICTORY_INPLACE_PROBE`: 0 (unset) nothing; 1 the probe on memory of its own at the first sweep, and the census 30 and 150 seconds later; 2 also opens one part of the title's own memory for a moment at the first census. | neither |
| `VICTORY_INPLACE_PROBE_MB` | `crates/victory-gpu/src/inplace.rs:1236` | number; default `64usize` | module: | neither |

### victory-gpu / land (8)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_LAND` | `crates/victory-gpu/src/land.rs:46` | true unless `0` | What shaders wrote goes into title memory on a few threads (`VICTORY_LAND`, on by default; | neither |
| `VICTORY_LAND_ANYTIME` | `crates/victory-gpu/src/land.rs:475` | true unless `0` | Whether a piece is put through a view of ours while the runtime changes a mapping too (`VICTORY_LAND_ANYTIME=0`: through the title's address, as before). | neither |
| `VICTORY_LAND_BESIDE` | `crates/victory-gpu/src/land.rs:62` | true unless `0` | Whether views of the title's pages are written through one of ours. | neither |
| `VICTORY_LAND_SKIP` | `crates/victory-gpu/src/land.rs:69` | true only if `1` | `VICTORY_LAND_SKIP=1`, off by default: counted either way). | neither |
| `VICTORY_LAND_TAKEN` | `crates/victory-gpu/src/land.rs:511` | true unless `0` | Whether a copy a landing went through is noted as up to date with it (`VICTORY_LAND_TAKEN=0`: it is not, and the next refresh copies the 64 KiB again and sends them up to the card, as before). | neither |
| `VICTORY_LAND_THREADS` | `crates/victory-gpu/src/land.rs:54` | number | How many threads land, the relay's own counted (`VICTORY_LAND_THREADS`). | neither |
| `VICTORY_UNDER_THREADS` | `crates/victory-gpu/src/land.rs:336` | number; default `6usize` | [`Pool::each`] on threads kept for the title's bytes under flights' writes (`VICTORY_UNDER_THREADS`, 6 with this one). | neither |
| `VICTORY_VIEW_THREADS` | `crates/victory-gpu/src/land.rs:351` | number | Calls `f` with every number below `count`, on this thread and on threads kept for making descriptors (`VICTORY_VIEW_THREADS`, 4 with this one, 6 for records; 1: on this one alone). | neither |

### victory-gpu / list (7)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_COPY_OVER_TITLE` | `crates/victory-gpu/src/list.rs:1116` | one of `0`, `title`; else default arm | The first copy into a texture of the title's: what it does not write is zeros, as in a texture that was an allocation of its own (`VICTORY_COPY_OVER_TITLE=title`: what title memory holds, which may be another texture's: specks ... | neither |
| `VICTORY_CUT_WRITES` | `crates/victory-gpu/src/list.rs:2210` | true only if `1`; diag build | The title wrote a value at the bottom of the pipe (not a timestamp, not a query result): listed (diagnostics), and with `VICTORY_CUT_WRITES=1` the host list is cut right behind it, so that the value is stored when the card has ... | neither |
| `VICTORY_LIST_POOL` | `crates/victory-gpu/src/list.rs:248` | true unless `0` | `VICTORY_LIST_POOL=0`: each is made new and thrown away, as before). | neither |
| `VICTORY_READ_BACK` | `crates/victory-gpu/src/list.rs:1209` | true unless `0` | fn doc: | neither |
| `VICTORY_THIN` | `crates/victory-gpu/src/list/thin.rs:58` | true only if `1` | Whether list calls are only written down (`VICTORY_THIN=1`; needs the line of submissions, which it turns on). | play.ps1=`1 (not -Safe)`; cmd=`1` |
| `VICTORY_THIN_THREADS` | `crates/victory-gpu/src/list/thin.rs:303` | number; default `8usize` | module: | neither |
| `VICTORY_WAIT_ASK` | `crates/victory-gpu/src/list.rs:2007` | true only if `1` | `VICTORY_WAIT_ASK=1` asks first, as before. | neither |

### victory-gpu / memory (3)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_BVH_SIZES_WAIT` | `crates/victory-gpu/src/memory.rs:819` | true only if `1` | Whether a submission that asks for the sizes of structures is waited for, and landed, before the title goes on (`VICTORY_BVH_SIZES_WAIT=1`; off). | neither |
| `VICTORY_BVH_TRACE` | `crates/victory-gpu/src/memory.rs:1091` | true only if `1` | `VICTORY_BVH_TRACE=1`: the sizes the builder is asked for (its program of one thread that stores a structure's compacted size) and when title memory has them, and the copies to a place of that size. | neither |
| `VICTORY_MEMORY_DUMP` | `crates/victory-gpu/src/memory.rs:1197` | path; unset = off | `VICTORY_MEMORY_DUMP=<folder>`: when a file `go` appears there, the arenas as the card holds them are copied out and written (`arena-<n>.bin`), with `pages.txt` (title page, arena, offset) and the structures the walking ... | neither |

### victory-gpu / picked (2)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_PICKED_MB` | `crates/victory-gpu/src/picked.rs:76` | u64; default `32` | Bytes looked at in turn per frame (`VICTORY_PICKED_MB`). | neither |
| `VICTORY_PICKED_WRITES` | `crates/victory-gpu/src/picked.rs:60` | one of `1`, `2`; else default arm | `VICTORY_PICKED_WRITES`: 0 (off), 1 (what is found is declared, so that no refresh sends title memory over it while a flight may write it, and not landed), 2 (landed too). | neither |

### victory-gpu / pipeline (21)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_ACTIVATE_RAYS` | `crates/victory-gpu/src/pipeline/make.rs:78`, `crates/victory-gpu/src/pipeline/make.rs:80` | with ray tracing on (the default): left out only if `0`; with `VICTORY_BVH_BUILDER=0`: left out if unset or `0` | Whether the title's `CS_ScreenProbes_ActivateRays` programs are left out (their dispatches dropped). Not needed with the builder on: they run anyway. With the builder off, leaving them and the resolve programs out left their images unwritten, and about two launches in three posterised (2026-11-26, `docs/bug-journal.md`). | neither |
| `VICTORY_BVH_BUILDER` | `crates/victory-gpu/src/pipeline.rs:341` (`builder_asked`, also read through it at `pipeline/make.rs:62`, `pipeline/make.rs:77`) | on unless `0` (off by default until 2026-11-26) | The title's ray tracing: its structure builder's stages run (see [`Builder`]) and the programs that activate and resolve rays are kept. `0`: off; the builder's stages are dropped, the resolve and activation programs left out, and `VICTORY_AMBIENT` stands in for the indirect light. | play.ps1=`0 (-NoRayTracing)` |
| `VICTORY_BVH_QUARANTINE` | `crates/victory-gpu/src/pipeline.rs:312` | true only if `1` | `VICTORY_BVH_QUARANTINE=1`: what the structure builder's stages store stays in the host's copies and is not copied back into title memory. | neither |
| `VICTORY_EARLY_ALL` | `crates/victory-gpu/src/pipeline/library.rs:768` | true only if `1` | `VICTORY_EARLY_ALL=1`: every title pipeline is started at creation, requested before or not, so that the driver's own cache holds it for later runs. | neither |
| `VICTORY_LIBRARY_HANDLE` | `crates/victory-gpu/src/pipeline/library.rs:95` | true unless `0` | `VICTORY_LIBRARY_HANDLE=0` closes it after mapping, as before. | neither |
| `VICTORY_LIBRARY_LOCK` | `crates/victory-gpu/src/pipeline/library.rs:432` | true only if `1` | With `VICTORY_LIBRARY_LOCK=1`: held by every library call, as before. | neither |
| `VICTORY_LIBRARY_MAP` | `crates/victory-gpu/src/pipeline/library.rs:86` | true unless `0` | `VICTORY_LIBRARY_MAP=0` uses buffers, as before. | neither |
| `VICTORY_LIBRARY_TRIM` | `crates/victory-gpu/src/pipeline/library.rs:375` | true unless `0` | `VICTORY_LIBRARY_TRIM=0`: the pages stay until Windows takes them. | neither |
| `VICTORY_PIPELINE_BOOK` | `crates/victory-gpu/src/pipeline/known.rs:43` | true only if `1` | `VICTORY_PIPELINE_BOOK=1`: the pipelines the title creates are kept. | neither |
| `VICTORY_PIPELINE_LOWEST` | `crates/victory-gpu/src/pipeline/make.rs:514` | true unless `0` | Below the title's own threads (loading, recording), and the streaming one, which is below normal itself (`VICTORY_PIPELINE_LOWEST=0`: level with it, as before). | neither |
| `VICTORY_PIPELINE_SLOW` | `crates/victory-gpu/src/pipeline/make.rs:361` | true only if `0` | `VICTORY_PIPELINE_SLOW=0`: in the order the title creates them, as before. | neither |
| `VICTORY_PIPELINE_WAIT_MS` | `crates/victory-gpu/src/pipeline/make.rs:195` | u64; default `500` | Waiting allowed per title frame, over all threads. | neither |
| `VICTORY_PIPELINE_WAIT_SHARED` | `crates/victory-gpu/src/pipeline/make.rs:278` | true unless `0` | `VICTORY_PIPELINE_WAIT_SHARED=0` counts as before. | neither |
| `VICTORY_PIPELINE_WORKERS` | `crates/victory-gpu/src/pipeline/make.rs:498` | number | `asked` (`VICTORY_PIPELINE_WORKERS`, 1 to 64), else two thirds of the logical processors, 2 to 16. | neither |
| `VICTORY_LINEAR_SAMPLERS` | `crates/victory-gpu/src/pipeline/host.rs:422` | `<16-hex shader hash>,...`; unset = off | An experiment: the static samplers of the listed shaders filter linearly in min, mag and mip (`D3D12_FILTER` 0x15), whatever the console's sampler words say; reduction and comparison bits are kept. Read when a host pipeline is made. | neither |
| `VICTORY_ROOT_LOG` | `crates/victory-gpu/src/pipeline/host.rs:429` | `<16-hex shader hash>,...`; unset = off | For pipelines of the listed shaders: an info line when the host root signature is made (object, blob fingerprint, every static sampler's filter, register, space, visibility) and, in diagnostics builds, a `BIND` line in a captured frame's ops listing saying whether the root signature and pipeline were set or already set. | neither |
| `VICTORY_ROOT_SRV_DIRECT` | `crates/victory-gpu/src/pipeline/host.rs:355` | set (any value) = root descriptors | `VICTORY_ROOT_SRV_DIRECT=1` goes back to root descriptors. | neither |
| `VICTORY_RT_RESOLVE` | `crates/victory-gpu/src/pipeline/make.rs:70` | only read with `VICTORY_BVH_BUILDER=0`: `1` (any non-`0`) keeps the `CS_RaytracingResolve` programs; default: left out | With the builder off, ~1,000 resolve programs shade no hits while clogging the cold compiler queue. Left out, what they write is never written (see `VICTORY_ACTIVATE_RAYS`). With the builder on they always run. | neither |
| `VICTORY_SCREEN_WAIT_MS` | `crates/victory-gpu/src/pipeline/make.rs:209` | number; default `10_000` | As [`wait_budget`], for passes over the screen (`VICTORY_SCREEN_WAIT_MS`, 10 s; 0: like the others). | neither |
| `VICTORY_SHADER_SAVE` | `crates/victory-gpu/src/pipeline.rs:154` | path; unset = off | `VICTORY_SHADER_SAVE=<dir>`: each shader as the title gave it (`.cdxil`) and as translated (`.dxil`), by the hash crash reports name it with | neither |
| `VICTORY_SKIP_PIPELINES` | `crates/victory-gpu/src/pipeline.rs:518` | `<16-hex hash>,...`; anything else warned and ignored; diag build | Omit draws/dispatches whose pipeline contains a listed shader hash. | play.ps1=`none` |

### victory-gpu / pool (2)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_RIPE_BLOCKS` | `crates/victory-gpu/src/pool.rs:101` | number; default `12` | How many blocks of textures are made ahead, so that one has been there for a while when its first piece is taken (`VICTORY_RIPE_BLOCKS`, 12; 0: none, and new blocks are used at once, as before). | neither |
| `VICTORY_TEXTURE_POOL` | `crates/victory-gpu/src/pool.rs:84` | true unless `0` | `VICTORY_TEXTURE_POOL=0` makes every one an allocation of its own again.  | neither |

### victory-gpu / popwatch (3)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_POP_ALL` | `crates/victory-gpu/src/popwatch.rs:314` | true only if `1` | Pop watch: save the pictures of every frame to `VICTORY_POP_FOLDER`, not only frames with blips (popwatch.rs module doc). | neither |
| `VICTORY_POP_FOLDER` | `crates/victory-gpu/src/popwatch.rs:295` | `<folder>`; unset = off | The three pictures around a frame with blips (at most 200 times) are saved there, 640 wide (popwatch.rs module doc). | neither |
| `VICTORY_POP_WATCH` | `crates/victory-gpu/src/popwatch.rs:46` | text; unset = empty | `VICTORY_POP_WATCH=<from>-<to>[,<from>-<to>..]` (seconds since the start): every presented frame in those times is read back, made small on a thread of its own and compared with its neighbours, block by block. | neither |

### victory-gpu / queue (8)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_ASYNC_SUBMIT` | `crates/victory-gpu/src/queue/behind.rs:649`, `crates/victory-gpu/src/queue/behind.rs:654` | true only if `1`; diag build; `VICTORY_AB`-switchable | Whether submissions are prepared by a thread of ours (`VICTORY_ASYNC_SUBMIT=1`; needs flights with the present sent ahead, as the relay has them, and lists cut at their waits). | neither |
| `VICTORY_CUT_WAITS` | `crates/victory-gpu/src/queue.rs:113` | true unless `0` | `VICTORY_CUT_WAITS=0` holds whole lists, as before. | neither |
| `VICTORY_GPU_LOAD_MS` | `crates/victory-gpu/src/queue.rs:856` | u64; default `0` | What the title's GPU time is made to look like, in ticks (`VICTORY_GPU_LOAD_MS`; unset: as it is). | neither |
| `VICTORY_QUEUE_WAIT` | `crates/victory-gpu/src/queue.rs:75` | true unless `0` | `VICTORY_QUEUE_WAIT=0`: they do not, and every submission runs when it is made (as before). | neither |
| `VICTORY_SPLIT_WAITS` | `crates/victory-gpu/src/queue.rs:94` | true unless `0` | `VICTORY_SPLIT_WAITS=0` holds whole submissions, as before. | neither |
| `VICTORY_SUBMIT_DEPTH` | `crates/victory-gpu/src/queue/behind.rs:623` | default `64` | How many submissions may be in the line when a submit call returns (`VICTORY_SUBMIT_DEPTH`; 64: about two frames of the title's). | neither |
| `VICTORY_SUBMIT_FRAMES` | `crates/victory-gpu/src/queue/behind.rs:631` | default `1` | How many presents may be in the line when a present call returns (`VICTORY_SUBMIT_FRAMES`; 1: the frame before is sent, this one may not be; 0: both are). | neither |
| `VICTORY_WAIT_FORCE` | `crates/victory-gpu/src/queue.rs:123` | true unless `0` | `VICTORY_WAIT_FORCE=0` keeps it held: for finding out whether the title's waits reach across its presents; a wait nothing ever meets then stops the title for good. | neither |

### victory-gpu / replay (6)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_RECORD` | `crates/victory-gpu/src/replay/record.rs:61` | `<folder>`; unset/empty = off; diag build (replay) | Frame recorder for `victory replay` (docs/replay.md); with `_AT` (seconds or `f<frame>`), `_FRAMES`, `_ALWAYS=1`. | neither |
| `VICTORY_RECORD_ALWAYS` | `crates/victory-gpu/src/replay/record.rs:67` | true only if `1` | From `VICTORY_RECORD`, `VICTORY_RECORD_AT` (seconds, or `f<frame>`), `VICTORY_RECORD_FRAMES` and `VICTORY_RECORD_ALWAYS=1`. | neither |
| `VICTORY_RECORD_AT` | `crates/victory-gpu/src/replay/record.rs:62` | number | From `VICTORY_RECORD`, `VICTORY_RECORD_AT` (seconds, or `f<frame>`), `VICTORY_RECORD_FRAMES` and `VICTORY_RECORD_ALWAYS=1`. | neither |
| `VICTORY_RECORD_FRAMES` | `crates/victory-gpu/src/replay/record.rs:63` | number; default `1` | From `VICTORY_RECORD`, `VICTORY_RECORD_AT` (seconds, or `f<frame>`), `VICTORY_RECORD_FRAMES` and `VICTORY_RECORD_ALWAYS=1`. | neither |
| `VICTORY_REPLAY_GBV` | `crates/victory-gpu/src/replay/play.rs:3422` | true if set (any value) | Every shader access checked on the card: very slow. | neither |
| `VICTORY_REPLAY_PEEK` | `crates/victory-gpu/src/replay/play/diverge.rs:975` | true if set (any value) | module: | neither |

### victory-gpu / resource (9)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_ALIAS_COPY` | `crates/victory-gpu/src/resource.rs:1421`, `crates/victory-gpu/src/resource.rs:1427` | true unless `0` | `VICTORY_ALIAS_COPY=0`: none is found, as before; | neither |
| `VICTORY_CARD_ROOM_FOR_TARGETS_MB` | `crates/victory-gpu/src/resource.rs:1729` | u64 | `VICTORY_CARD_ROOM_FOR_TARGETS_MB` (3,072; 0: none, as before) less what such copies hold now. | neither |
| `VICTORY_DEPTH_TWINS` | `crates/victory-gpu/src/resource.rs:463`, `crates/victory-gpu/src/resource.rs:474`, `crates/victory-gpu/src/resource.rs:550` | true unless `0` | `VICTORY_DEPTH_TWINS=0` makes none (such views stay null), `VICTORY_DEPTH_TWINS=stencil` only those of formats with a stencil plane, `VICTORY_DEPTH_TWINS=nodepth` none for the depth planes of such formats. | neither |
| `VICTORY_HELD_LEVELS` | `crates/victory-gpu/src/resource.rs:1243` | true unless `0` | Whether a texture the title places in memory is read, kept and uploaded from the first level its views show on (`VICTORY_HELD_LEVELS=0`: whole, as before). | neither |
| `VICTORY_LAZY_TEXTURES` | `crates/victory-gpu/src/resource.rs:1487` | true unless `0` | `VICTORY_LAZY_TEXTURES=0` makes each when the title creates its texture, as before. | neither |
| `VICTORY_PLACED_FILLER` | `crates/victory-gpu/src/resource.rs:3039` | true only if `1` | `VICTORY_PLACED_FILLER=1`: kept in the key, as before. | neither |
| `VICTORY_TEXTURE_BUDGET_MB` | `crates/victory-gpu/src/resource.rs:1681` | u64 | `VICTORY_TEXTURE_BUDGET_MB` sets a limit on those copies' bytes instead). | neither |
| `VICTORY_TEXTURE_ROOM_PERCENT` | `crates/victory-gpu/src/resource.rs:1768` | u64 | The share of the card's budget, in percent, up to which host copies of textures picked by index are made ([`room`]): 90 unless `VICTORY_TEXTURE_ROOM_PERCENT` says another (50 to 97). | neither |
| `VICTORY_TEXTURE_SWEEP` | `crates/victory-gpu/src/resource.rs:2020`, `crates/victory-gpu/src/resource.rs:2070` | true unless `0` | `VICTORY_TEXTURE_SWEEP=0` lets nothing go. | neither |

### victory-gpu / safecopy (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_SAFE_COPY` | `crates/victory-gpu/src/safecopy.rs:325` | true unless `0` | Whether copies are made here and not by the kernel. | neither |

### victory-gpu / shader_cache (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_TRANSLATOR` | `crates/victory-gpu/src/shader_cache.rs:57` | build-time (`cargo:rustc-env`, victory-gpu/build.rs:44) | BLAKE3 of victory-shader sources, `shader_cache.rs`, `Cargo.lock`, `rust-toolchain.toml`: part of every shader-cache key. | neither |

### victory-gpu / shadow (24)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_CARD_ALL` | `crates/victory-gpu/src/shadow.rs:511` | true only if `1` | Whether every title buffer's copy has two sides, however small (`VICTORY_CARD_ALL=1`; off by default). | neither |
| `VICTORY_CARD_BUFFERS` | `crates/victory-gpu/src/shadow.rs:169` | true unless `0` | `VICTORY_CARD_BUFFERS=0` keeps every copy in the PC's memory, as before. | neither |
| `VICTORY_CARD_BUFFERS_MB` | `crates/victory-gpu/src/shadow.rs:292` | MiB; default derived from VRAM budget (`default_card_limits_mb`): max(512, budget/7) at ≤ 12 GiB (512 if unknown), 2048 below 20 GiB, budget/4 (at most 8192) above | Most bytes of title buffers given card-memory copies. Set, it also removes the small copies' share (first come, first served). | neither |
| `VICTORY_CARD_BUFFERS_MIN_KB` | `crates/victory-gpu/src/shadow.rs:300` | u64; unset | `VICTORY_CARD_BUFFERS_MIN_KB` (after fxceless c36971b2) says the least size in KiB, so that it can go under 1 MiB: 64 is every title buffer. Under the default cap of a ≤ 12 GiB budget, copies under the tier's least size (16 MiB) share at most 3/4 of the cap. | cmd=`64` |
| `VICTORY_CARD_BUFFERS_MIN_MB` | `crates/victory-gpu/src/shadow.rs:291` | MiB; default derived (`resolve_card_limits`): 16 at ≤ 12 GiB, 4 below 20 GiB, 1 above | Least size of a title buffer that gets a card-memory copy (see also `_MIN_KB`). | neither |
| `VICTORY_CARD_HASH_LEN` | `crates/victory-gpu/src/shadow.rs:4437` | true unless `0` | `VICTORY_CARD_HASH_LEN=0`: the whole buffer. | neither |
| `VICTORY_CARD_ROOM_FOR_BUFFERS` | `crates/victory-gpu/src/shadow.rs:415` | true unless `0` (default ON; the comment above still says off by default: stale) | Whether the texture queue leaves room on the card for card copies of title buffers not made yet. | neither |
| `VICTORY_CARD_WINDOWS_MB` | `crates/victory-gpu/src/shadow.rs:635` | u64; default `0` | The most bytes of windows outside buffers whose copies go to the card's memory (`VICTORY_CARD_WINDOWS_MB`; 0, the default, none). | neither |
| `VICTORY_EXACT_BACK` | `crates/victory-gpu/src/shadow.rs:2267` | true unless `0` | Whether what shaders wrote goes into title memory run by run ([`put_changed`]; on by default, `VICTORY_EXACT_BACK=0` writes the whole stretch from the first changed byte to the last, as before). | neither |
| `VICTORY_KEEP_SNAPSHOTS` | `crates/victory-gpu/src/shadow.rs:2713` | true only if `1` | Whether a copy's snapshot is kept from one submission to the next and only the pieces that changed in between are taken again (`VICTORY_KEEP_SNAPSHOTS=1`; off by default). | neither |
| `VICTORY_KEEP_SNAPSHOTS_MB` | `crates/victory-gpu/src/shadow.rs:2724` | u64; default `256` | `VICTORY_KEEP_SNAPSHOTS_MB`, 256 unless set (it was 768: run 80, with kept snapshots beside copies in the card's memory, ran the PC out of memory in 27 s; kept snapshots are the PC's memory, all of it resident). | neither |
| `VICTORY_LAND_ECHO` | `crates/victory-gpu/src/shadow.rs:2204` | default ON; `0` off | Whether a refresh looks if what counts as written is only what a landing put there, and then sends nothing of it up (`VICTORY_LAND_ECHO=0`: it copies and sends it, as before). | play.ps1=`0 (-Safe)` |
| `VICTORY_MEMORY_CARD_MB` | `crates/victory-gpu/src/shadow.rs:5384` | u64; default `4096` | At most so much on the card's own memory (4 GiB unless `VICTORY_MEMORY_CARD_MB` says otherwise), and while it has room; past that the side shaders see is in the PC's memory. | neither |
| `VICTORY_QUICK_FIND` | `crates/victory-gpu/src/shadow.rs:4971` | default ON; `0` off | Whether [`FOUND`] is kept (`VICTORY_QUICK_FIND=0`: not). | neither |
| `VICTORY_REFRESH_CHECK` | `crates/victory-gpu/src/shadow.rs:3978` | true only if `1` | Diagnostic (`VICTORY_REFRESH_CHECK=1`; off by default): after a refresh of `[from, to)` that trusted the write watch or the guards, the first 256 bytes of every 64 KiB block of it are read from title memory and compared with ... | neither |
| `VICTORY_SEND_CHANGED` | `crates/victory-gpu/src/shadow.rs:2247` | true if set and not `0`; diag build | Whether a refresh of a copy in the card's memory takes, and sends up, the dwords that title memory changed and no others (`VICTORY_SEND_CHANGED=1`; off: the 64 KiB pages written since it last took them, whole). | neither |
| `VICTORY_SNAPSHOT_POOL` | `crates/victory-gpu/src/shadow.rs:1472` | number; default `1024` | How many snapshot buffers are kept (`VICTORY_SNAPSHOT_POOL`; | neither |
| `VICTORY_UNDER_WIDE` | `crates/victory-gpu/src/shadow.rs:2351` | default ON; `0` off | Whether the pages of [`Shadow::merge_under_writes`] are compared on several threads (`VICTORY_UNDER_WIDE=0`: on the submitting one). | play.ps1=`0 (-Safe)` |
| `VICTORY_WIDE_BACK` | `crates/victory-gpu/src/shadow.rs:1638` | true unless `0` | `VICTORY_WIDE_BACK=0` turns it off. | play.ps1=`1`; cmd=`1` |
| `VICTORY_WIDE_SPIN` | `crates/victory-gpu/src/shadow.rs:1657` | number; default `0` | Waits awake until `there` says so, `VICTORY_WIDE_SPIN` microseconds at most (0, the default: not at all): asleep, a thread is woken in 50 us and more, 95 times a frame. | neither |
| `VICTORY_WINDOWS_BAR_MB` | `crates/victory-gpu/src/shadow.rs:673` | u64; default `0` | The most bytes of windows outside buffers that lie in the card's memory and are filled by the CPU through the bus (`VICTORY_WINDOWS_BAR_MB`; 0, the default, none): one buffer (`D3D12_HEAP_TYPE_GPU_UPLOAD`), no CPU's side, no ... | neither |
| `VICTORY_WINDOWS_OWN` | `crates/victory-gpu/src/shadow.rs:711` | true only if `1` | Whether windows outside buffers have two sides too, so that submissions in the air read the bytes they were prepared with (`VICTORY_WINDOWS_OWN=1`; off by default: it takes the windows' bytes again, 3.3 GB in capture replay2, ... | neither |
| `VICTORY_WINDOW_KB` | `crates/victory-gpu/src/shadow.rs:5240` | u64 | The grid the windows of asks with a length are cut on, in bytes: 4 MiB unless `VICTORY_WINDOW_KB` names a smaller power of two (64 at least). | neither |
| `VICTORY_WINDOW_TAILS` | `crates/victory-gpu/src/shadow.rs:5279` | true only if `1` | With this on, the window is the cell's tail from the ask's step on ([`TAIL_STEP`]); an ask lower in the cell later makes a longer tail, which every ask from then on takes, and the shorter one goes when nothing holds it any ... | neither |

### victory-gpu / texback (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_TEXTURE_BACK` | `crates/victory-gpu/src/texback.rs:34` | `1`/unset: default kind list; or `<w>x<h>f<fmt>,...` | For textures of the kinds in `VICTORY_TEXTURE_BACK` (comma-separated `<width>x<height>f<format>`; unset or `1`: the four water formats found by the upstream trace; | neither |

### victory-gpu / texture (11)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_COMMITTED_QUERY` | `crates/victory-gpu/src/texture.rs:1697` | true only if `1` | Whether the two questions above and below are put to `VirtualQuery`, as they were (`VICTORY_COMMITTED_QUERY=1`). | neither |
| `VICTORY_MAPPED_BY_PAGE` | `crates/victory-gpu/src/texture.rs:1792` | true only if `1` | module: | neither |
| `VICTORY_MAPPED_BY_VIEWS` | `crates/victory-gpu/src/texture.rs:1782` | true unless `0` | `VICTORY_MAPPED_BY_VIEWS=0`: the host is asked, as before. | neither |
| `VICTORY_TEXTURE_COPY` | `crates/victory-gpu/src/texture.rs:458` | true only if `1` | Whether a texture all of whose memory is mapped is read through the kernel's copy instead of in place (`VICTORY_TEXTURE_COPY=1`; off by default). | neither |
| `VICTORY_TEXTURE_FILL` | `crates/victory-gpu/src/texture.rs:394` | true unless `0` | Whether a refresh that does not wait ([`no_wait`]) and finds a page missing fills a staging buffer that was never filled from the pages that are there (`VICTORY_TEXTURE_FILL=0`: it was left as it was made). | neither |
| `VICTORY_TEXTURE_GUARD` | `crates/victory-gpu/src/texture.rs:1302` | true unless `0` | `VICTORY_TEXTURE_GUARD=0` hashes at every check, as before. | neither |
| `VICTORY_TEXTURE_HOT` | `crates/victory-gpu/src/texture.rs:133` | true unless `0` | `VICTORY_TEXTURE_HOT=0`: every eighth frame only, as before. | neither |
| `VICTORY_TEXTURE_NO_WAIT` | `crates/victory-gpu/src/texture.rs:376` | true only if `1` | Whether a texture refresh that finds the runtime remapping title memory goes on without waiting for it (`VICTORY_TEXTURE_NO_WAIT=1`; off by default). | play.ps1=`1`; cmd=`1` |
| `VICTORY_TEXTURE_ONCE` | `crates/victory-gpu/src/texture.rs:677` | true unless `0` | Whether a staging buffer made for one upload of a texture picked by index is filled without asking the write watch and the guards (`VICTORY_TEXTURE_ONCE=0`: with, as before). | neither |
| `VICTORY_TEXTURE_SAME` | `crates/victory-gpu/src/texture.rs:1620` | true unless `0` | (Of 787 repeated uploads of a frame that streams 398 were equal in every byte, and a texture uploaded again costs a staging buffer, a detiling and a copy more than this look.) `VICTORY_TEXTURE_SAME=0`: not asked. | neither |
| `VICTORY_UPLOAD_AHEAD` | `crates/victory-gpu/src/popwatch.rs:203`, `crates/victory-gpu/src/texture/ahead.rs:42`, `crates/victory-gpu/src/texture/ahead.rs:46` | true if set and not `0`; diag build; `VICTORY_AB`-switchable | Whether it is on (`VICTORY_UPLOAD_AHEAD`; off). | play.ps1=`1 (not -Safe)`; cmd=`1` |

### victory-gpu / tiles (2)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_LEVELS_MIN_KB` | `crates/victory-gpu/src/tiles.rs:120` | KiB, at least 64; default 1 MiB (`parse_floor`) | The bytes from which a texture is made this way: a megabyte, or `VICTORY_LEVELS_MIN_KB`. | neither |
| `VICTORY_LEVELS_ON_DEMAND` | `crates/victory-gpu/src/tiles.rs:81` | true only if `1` | Whether textures are made this way: the switch (`VICTORY_LEVELS_ON_DEMAND=1`; off by default), and a card that reads zeros from, and drops writes to, levels without memory (tiled resources tier 2). | cmd=`1` |

### victory-gpu / timeline (3)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_TIMELINE` | `crates/victory-gpu/src/timeline/record.rs:467` | `<file>`; unset/empty = off | Timeline recorder: per-thread spans and waits written to the file (timeline/record.rs). | neither |
| `VICTORY_TIMELINE_AT` | `crates/victory-gpu/src/timeline/record.rs:473` | seconds; default `60.0` | When the timeline recording starts. | neither |
| `VICTORY_TIMELINE_FRAMES` | `crates/victory-gpu/src/timeline/record.rs:474` | frames; default `3` | How many frames the timeline records. | neither |

### victory-gpu / titlewrite (4)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_COPY_BESIDE` | `crates/victory-gpu/src/titlewrite.rs:70`, `crates/victory-gpu/src/titlewrite.rs:103` | on only if `1`; `VICTORY_AB`-switchable | module: | neither |
| `VICTORY_SAME_RECORDS` | `crates/victory-gpu/src/titlewrite.rs:185` | default ON; `0` off | Bytes of the title's that we put into its memory for it (a descriptor it has copied): as a copy of its own, but what is there already is never written (`VICTORY_SAME_RECORDS=0`: always). # Safety `[from, from + n)` and `[to, ... | neither |
| `VICTORY_STORE_BESIDE` | `crates/victory-gpu/src/titlewrite.rs:78` | on only if `1` | module: | neither |
| `VICTORY_WRITE_SITES` | `crates/victory-gpu/src/titlewrite.rs:273`, `crates/victory-loader/src/write_sites.rs:288` | `<file>`; unset = off | With `VICTORY_WRITE_SITES=<file>`, a place that can be made to look first (`can_look`) and mostly left it is written into that file, for the next start (`victory_loader::write_sites`). | neither |

### victory-gpu / upload (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_UPLOAD_LOW` | `crates/victory-gpu/src/upload.rs:42` | true unless `0` | `VICTORY_UPLOAD_LOW=0`: from the process heap, as before. | neither |

### victory-gpu / watch (12)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_ASK_EARLY` | `crates/victory-gpu/src/watch.rs:403` | f64; default `2.0` | Whether a thread asks, between a submission's askings, about the memory they found written (`VICTORY_ASK_EARLY=<ms>` between its rounds; off unless given: in run cpu-2 the submissions' own askings were none the cheaper for its ... | neither |
| `VICTORY_ASK_ONCE` | `crates/victory-gpu/src/watch.rs:524` | true unless `0` | `VICTORY_ASK_ONCE=0` asks per range, as before. | neither |
| `VICTORY_GUARD_ASK` | `crates/victory-gpu/src/watch.rs:1646` | true only if `1` | `VICTORY_GUARD_ASK=1` asks every time, as before. | neither |
| `VICTORY_GUARD_HOLES` | `crates/victory-gpu/src/watch.rs:1762` | true unless `0` | `VICTORY_GUARD_HOLES=0` looks at the first address only, as before. | neither |
| `VICTORY_GUARD_REST` | `crates/victory-gpu/src/watch.rs:1348`, `crates/victory-gpu/src/watch.rs:1352` | default ON; `0` off | How long a block the title has just opened by a write is left open by those that take it (`VICTORY_GUARD_REST=<microseconds>`, 3,000 unless given; | neither |
| `VICTORY_KNOWN_HOLES` | `crates/victory-gpu/src/watch.rs:1070` | true unless `0` | Whether placeholders are known for what they are (`VICTORY_KNOWN_HOLES=0`: every hole is looked at at every asking, as before). | neither |
| `VICTORY_LIGHT_REFRESH` | `crates/victory-gpu/src/watch.rs:826` | true unless `0` | Whether the refreshes of host copies go the light way (`VICTORY_LIGHT_REFRESH=0`: as before): view memory nothing wrote since it was last asked about is known by its bits ([`PUBLISHED`]), a copy nothing of whose range changed ... | neither |
| `VICTORY_OPEN_ONE_VIEW` | `crates/victory-gpu/src/watch.rs:1326` | default ON; `0` off | Whether a write fault opens the view that was written and leaves the page's other views read-only, not clean any more (`VICTORY_OPEN_ONE_VIEW=0`: opens them all, as before). | neither |
| `VICTORY_PAGE_PARTS` | `crates/victory-gpu/src/watch.rs:54` | true unless `0` | Whether the parts of a page that the kernel names are kept, and a refresh copies those (`VICTORY_PAGE_PARTS=0`: the 64 KiB page, as before). | neither |
| `VICTORY_PAGE_WRITES` | `crates/victory-gpu/src/watch.rs:2116` | one of `0`, `1`, `slots`; else default arm | What `VICTORY_PAGE_WRITES` asks to be done with a write the guards do not see once its bytes are there ([`title_memory_written`]), as bits: 1, the guarded blocks under it are opened and count as written (host copies of buffers ... | neither |
| `VICTORY_TAKE_PARTS` | `crates/victory-gpu/src/watch.rs:787` | default ON; `0` off | Of a page taken before, the parts written since, where they are known (`VICTORY_TAKE_PARTS=0`: the page). | neither |
| `VICTORY_VIEW_GUARD` | `crates/victory-gpu/src/watch.rs:1809` | true unless `0` | Whether all the views of a guarded page are guarded together (`VICTORY_VIEW_GUARD`). | neither |

### victory-gpu / watched (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_WATCHED_HEAP` | `crates/victory-gpu/src/watched.rs` `on` | true unless `0` | Whether big blocks (64 KiB or more) of host memory for the title (`XMemAllocDefault`, device heaps) are write-watched reservations (`VICTORY_WATCHED_HEAP=0`: they come from the process heap, as before 2026-11-27, and host copies of buffers there are copied whole at every refresh). | neither |

### victory-gpu / writers (6)

The writers watch (`docs/crates/victory-gpu-memory.md` §3.4a). Read once, by
`writers_from_env`, which only diagnostics builds call
(`victory_hle::watch_writers`, from `apps/victory/src/run.rs` `diagnostics`);
a release build never reads them.

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_WRITERS` | `crates/victory-gpu/src/writers.rs:398` | `<hex address>+<hex length>,...` (title addresses, `0x` optional, each at most 64 KiB); unset = off; diag build | Keeps the 4 KiB pages of each range read-only during the window and names every write into them: instruction as `module+rva`, offset, size, values, thread, frame, stack; a table every 10 s while something is new, at the window's end and at exit; the range's first 256 words when its watch starts and ends. | neither |
| `VICTORY_WRITERS_OF` | `crates/victory-gpu/src/writers.rs:418` | `<16-hex shader hash>:s<slot>+<dword offset>,...`; unset = off; diag build | For draws whose pipeline has that shader, the title memory of the table view at that binding (a constant view or a placed buffer view, as the ops listing names it) is watched as `VICTORY_WRITERS` would, up to 4 distinct addresses per binding. | neither |
| `VICTORY_WRITERS_FROM` | `crates/victory-gpu/src/writers.rs:467` | seconds (f64) after the title starts; default 0; diag build | When the watch's window opens: before it nothing is read-only. | neither |
| `VICTORY_WRITERS_FOR` | `crates/victory-gpu/src/writers.rs:468` | seconds (f64); default: to the end; diag build | How long the window stays open; at its end the table and the words are told and the pages are let go. | neither |
| `VICTORY_WRITERS_DETAIL` | `crates/victory-gpu/src/writers.rs:469` | number; default 64; diag build | Writes of each (instruction, offset) noted with a stack; later ones are counted, with their values. | neither |
| `VICTORY_WRITERS_STACK` | `crates/victory-gpu/src/writers.rs:470` | number; default 12, at most 32; diag build | Frames of each stack noted. | neither |

### victory-hle (3)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_EFFICIENCY_MODE` | `crates/victory-hle/src/runtime.rs:251` | true only if `1` | `VICTORY_EFFICIENCY_MODE=1` leaves the system its choice. | neither |
| `VICTORY_IMAGE_WATCH` | `crates/victory-hle/src/image_watch.rs:51` | list of words (see `image_watch::words`) | Logs changes of words in a mapped title image (image_watch.rs). | neither |
| `VICTORY_PROCESS_PRIORITY` | `crates/victory-hle/src/runtime.rs:284` | `normal` or `0` keep start priority; default raise to ABOVE_NORMAL | `VICTORY_PROCESS_PRIORITY=normal`: as it was started. | neither |

### victory-hle / modules/audio (3)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_AUDIO_CAPTURE` | `crates/victory-hle/src/modules/audio/capture.rs:69`, `crates/victory-hle/src/modules/audio/capture.rs:85` | true if set (any value) | Starts the recorder if `VICTORY_AUDIO_CAPTURE` names a file. | neither |
| `VICTORY_AUDIO_CAPTURE_OF` | `crates/victory-hle/src/modules/audio/capture.rs:49` | set by `victory` for its recorder child | Names the process the audio-capture child records (set in the child's environment). | neither |
| `VICTORY_XMA_DUMP` | `crates/victory-hle/src/modules/audio.rs:667` | `<dir>`; unset = off; diag build | `VICTORY_XMA_DUMP=<dir>`: saves the context words (`<n>-<index>.ctx`) and input buffer 0 (`.xma`) of the first 32 voices enabled, to test the decoder on a title's streams outside the title. | neither |

### victory-hle / modules/dstorage (6)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_DSTORAGE_HALF` | `crates/victory-hle/src/modules/dstorage.rs:1305` | true only if `1` | Whether a queue submits itself once half of it is enqueued (`VICTORY_DSTORAGE_HALF=1`), as DirectStorage for the PC does and as ours did. | neither |
| `VICTORY_DSTORAGE_IO` | `crates/victory-hle/src/modules/dstorage.rs:301` | one of `one`, `handles`, `overlapped`; else default arm | How files are read (`VICTORY_DSTORAGE_IO`). | neither |
| `VICTORY_DSTORAGE_LOG` | `crates/victory-hle/src/modules/dstorage.rs:715`, `crates/victory-hle/src/modules/dstorage.rs:735` | path; unset = off | The streaming log (`VICTORY_DSTORAGE_LOG=<file>`): a line for every request (`R`: queue, number, file, offset, source bytes, destination bytes, options, when enqueued, submitted, started, done, result), every status entry ... | neither |
| `VICTORY_DSTORAGE_RETRIES` | `crates/victory-hle/src/modules/dstorage.rs:1667` | number; default `12` | A page the title has just made, or a view the host refuses for a moment (commit or address space short while a load maps many at once), can come right a little later: tried again before the request fails ... | neither |
| `VICTORY_DSTORAGE_STAGING_MB` | `crates/victory-hle/src/modules/dstorage.rs:236` | u64; default `256` | The title asks for 32 MiB and stops if this fails. | neither |
| `VICTORY_DSTORAGE_TRACE` | `crates/victory-hle/src/modules/dstorage.rs:1057` | number; default `0` | The streaming trace (`VICTORY_DSTORAGE_TRACE`): 0 or unset: off; 1: a line for every batch, and for every request whose page list, or whose pages' mapping, changed when it should not have, or whose bytes another running ... | neither |

### victory-hle / modules/gameinput (2)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_KEYS` | `crates/victory-hle/src/modules/gameinput.rs:1251` | `<s>:<key>,...` or `<from>-<to>:<key>` | `VICTORY_KEYS=<seconds>:<key>,...` holds each key for 0.2 s that long after input starts (`<from>-<to>:<key>`: all that time), with or without focus. | neither |
| `VICTORY_ONE_PAD` | `crates/victory-hle/src/modules/gameinput.rs:163` | true unless `0` | `VICTORY_ONE_PAD=0` shows each host pad as a device of its own, and the stand-in only while there is none). | neither |

### victory-hle / modules/kernel (1)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_TITLE_PRIORITIES` | `crates/victory-hle/src/modules/kernel.rs:431` | true only if `0` | `VICTORY_TITLE_PRIORITIES=0`: as the title sets them. | play.ps1=`0` |

### victory-hle / modules/lifecycle (2)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_RELAUNCHED_FROM` | `crates/victory-hle/src/modules/lifecycle.rs:325` | set by `victory` for a relaunched child | Inherited handle of the process a relaunched `victory` replaces; the child waits for it (`lifecycle::wait_for_previous`). | neither |
| `VICTORY_WINDOW_MAX` | `crates/victory-hle/src/modules/lifecycle.rs:182` | `WIDTHxHEIGHT`; unset = no limit; bad value warned | `VICTORY_WINDOW_MAX=WIDTHxHEIGHT` applies a smaller limit without changing the title's render targets. | neither |

### victory-hle / modules/xmem (5)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_MAP_LEAN` | `crates/victory-hle/src/modules/xmem.rs:1052` | true unless `0` | Whether a mapping change asks the host only for what it needs: no split of a placeholder that is already the view's own, no commit of section pages that are committed (`VICTORY_MAP_LEAN=0`: both every time, as before). | neither |
| `VICTORY_MAP_THREADS` | `crates/victory-hle/src/modules/xmem.rs:1305`, `crates/victory-hle/src/modules/xmem.rs:1322` | on if set and not `0`; diag build | How many threads map a group of pages side by side (`VICTORY_MAP_THREADS`; 8 unless set; 1: the caller alone, a page after the other; | play.ps1=`1`; cmd=`1` |
| `VICTORY_SECTION_WINDOW` | `crates/victory-hle/src/modules/xmem.rs:412` | true unless `0` | `VICTORY_SECTION_WINDOW=0`: a view for each write, as before. | neither |
| `VICTORY_STREAM_NOTICE` | `crates/victory-hle/src/modules/xmem.rs:601` | true unless `0` | See [`pages_written`]. | neither |
| `VICTORY_STREAM_PRIORITY` | `crates/victory-hle/src/modules/xmem.rs:1066` | true unless `0` | Called where that thread maps and submits: lifts a thread that is below normal to above it (`VICTORY_STREAM_PRIORITY=0`: left as the title set it). | neither |

### victory-loader (5)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_CLZERO_ASK` | `crates/victory-loader/src/console_insn.rs:28` | true unless `1` | `VICTORY_CLZERO_ASK=1` asks every time, as before. | neither |
| `VICTORY_PATCH_CLZERO` | `crates/victory-loader/src/console_insn.rs:76` | true unless `0` | `VICTORY_PATCH_CLZERO=0` emulates each time, as before. | neither |
| `VICTORY_PATCH_WAITS` | `crates/victory-loader/src/console_insn.rs:57` | true unless `0` | `VICTORY_PATCH_WAITS=0` emulates each time, as before. | neither |
| `VICTORY_PROFILE` | `crates/victory-loader/src/profile.rs:64` | schedule list; unset = off | Sampling profiler of the title's threads at the listed times (loader `profile_on_schedule`, called from diag `run`). | neither |
| `VICTORY_STACKS` | `crates/victory-loader/src/stacks.rs:27` | seconds list (e.g. `20,60`); unset = off | Reads `VICTORY_STACKS` and, if set, starts a thread that prints every thread's stack at each listed time (seconds from now). | neither |

### victory-shader (22)

| name | where read | values & default | effect (from the read-site comment) | launcher |
|---|---|---|---|---|
| `VICTORY_BVH_BOUNDS` | `crates/victory-gpu/src/shader_cache.rs:77`, `crates/victory-shader/src/emit/mod.rs:201` | true unless `0` | Whether the structure builder's accesses through its root views stay near them, and a size in a structure's header that cannot be one counts as none (`VICTORY_BVH_BOUNDS=0`: neither). | neither |
| `VICTORY_BVH_FENCES` | `crates/victory-gpu/src/shader_cache.rs:76`, `crates/victory-shader/src/emit/mod.rs:192` | true unless `0` | Whether buffer atomics of the ray-tracing structure builder stand between two fences (`VICTORY_BVH_FENCES=0`: they do not). | neither |
| `VICTORY_DESC_WHY` | `crates/victory-shader/src/emit/lower.rs:6469` | true if set (any value) | Translator debug print: `DESC-WHY` lines on stderr for descriptor tracing decisions (lower.rs:6469). | neither |
| `VICTORY_DXIL` | `crates/victory-shader/src/emit/validator.rs:122` | path to `dxil.dll`; unset = newest Windows SDK x64 copy, else DLL search path | Where `dxil.dll` is looked for. | neither |
| `VICTORY_F16_NEAREST` | `crates/victory-gpu/src/shader_cache.rs:70`, `crates/victory-shader/src/emit/lower.rs:1108` | only `1` | f16 conversion rounding form; keyed into the shader cache. | neither |
| `VICTORY_FIRSTLANE_OWN` | `crates/victory-gpu/src/shader_cache.rs:72`, `crates/victory-shader/src/emit/lower.rs:8841` | true only if `1` | `VICTORY_FIRSTLANE_OWN=1` takes the host wave's own first lane, exact when the value is the same in every lane of the console wave (the ray-tracing builder's scans read a sum of scalars this way, inside a loop whose exit is ... | neither |
| `VICTORY_FIRST_HALF` | `crates/victory-gpu/src/shader_cache.rs:73`, `crates/victory-shader/src/emit/lower.rs:1673` | true unless `0` | A first half whose result is also read as data (an f16 second half keeps the other half of its dst, PS_DeferredTextured): evaluated as the pair it starts. | neither |
| `VICTORY_GROUP_SYNC` | `crates/victory-shader/src/emit/mod.rs:180` | one of `0`, `1`; else default arm | (The console's barrier is the wave's, whatever lanes are live, and a wave that ended is not waited for.) `VICTORY_GROUP_SYNC`: - unset or `2`: every barrier waits, as before. - `1`: the title's `s_barrier` waits only in a ... | neither |
| `VICTORY_HELPER_OWN` | `crates/victory-gpu/src/shader_cache.rs:71`, `crates/victory-shader/src/emit/mod.rs:157` | default ON; `0` off | Whether a pixel shader's helper lane reads its own value where the console reads the first lane's (`lower.rs` `first_lane`); on unless `VICTORY_HELPER_OWN=0`. | neither |
| `VICTORY_MESH_OUTPUT_STRICT` | `crates/victory-gpu/src/shader_cache.rs:75`, `crates/victory-shader/src/emit/lower.rs:4630` | true if set (any value) | Experiment (`VICTORY_MESH_OUTPUT_STRICT=1`): they are refused. | neither |
| `VICTORY_RDNA_FLOAT` | `crates/victory-shader/src/emit/mod.rs:243` | parsed by `parse_set` | `VICTORY_RDNA_FLOAT`: a set of [`rdna`]'s bits (decimal or `0x` hex); all of them when it is unset, `0` none. | neither |
| `VICTORY_SHADER_LOOP_LIMIT` | `crates/victory-shader/src/emit/mod.rs:145` | decimal or `0x` hex, >= 1; default 2^20; diag build only | With the `diagnostics` feature, `VICTORY_SHADER_LOOP_LIMIT` (decimal or `0x` hex, at least 1) sets it instead. | neither |
| `VICTORY_SHADER_OPT` | `crates/victory-shader/src/emit/mod.rs:223` | pass bitmask; unset = all but the last (63); `0` none; `127` all | `VICTORY_SHADER_OPT`: - unset: every pass but the last one below (63). - `0`: none (the module as the lowering made it). - a number (decimal or `0x` hex): those passes; 1 dead code, 2 constants and identities, 4 one ... | neither |
| `VICTORY_SHADER_SOURCE_HASH` | `crates/victory-shader/src/emit/mod.rs:285` | build-time (victory-shader/build.rs:62) | FNV hash of victory-shader/src: part of `translation_fingerprint`. | neither |
| `VICTORY_SHADER_SWITCHES` | `crates/victory-shader/src/emit/mod.rs:288` | build-time (victory-shader/build.rs:64) | Every `"VICTORY_*"` literal in victory-shader/src; at run time the current value is folded into `translation_fingerprint`, which has no callers, so this does **not** reach the shader-cache key. | neither |
| `VICTORY_STRICT_JOINS` | `crates/victory-gpu/src/shader_cache.rs:74`, `crates/victory-shader/src/emit/lower.rs:12622` | true if set (any value) | Left unmarked, a value without a DXIL form coming into it became zero without a word (the looked-up tint of the material resolve programs, whose size computation had no value, was zero in every lane that way). shortcut: only ... | neither |
| `VICTORY_WARP` | `crates/victory-shader/src/emit/host.rs:68` | only `1` | Shader-test/translate `Host`: create the D3D12 device on WARP. | neither |
| `VICTORY_WAVE_DEEP_SKIPS` | `crates/victory-gpu/src/shader_cache.rs:67`, `crates/victory-shader/src/emit/mod.rs:569` | default ON; `0` off | (`VICTORY_WAVE_DEEP_SKIPS=0` and `VICTORY_WAVE_WIDE=0` turn the two off.) | neither |
| `VICTORY_WAVE_LOCAL` | `crates/victory-gpu/src/shader_cache.rs:69`, `crates/victory-shader/src/emit/mod.rs:470`, `crates/victory-shader/src/emit/mod.rs:476` | default ON; `0` off; `idiom` = strict | Each host wave as a wave of its own (`lower::with_local_waves`), for programs still stopped where not every lane runs. | neither |
| `VICTORY_WAVE_OWN_BITS` | `crates/victory-gpu/src/shader_cache.rs:68`, `crates/victory-shader/src/emit/lower.rs:1121` | default ON; `0` off | `VICTORY_WAVE_OWN_BITS=0`: a wave OR where not every lane runs is refused as before ([`super::own_bits`]). | neither |
| `VICTORY_WAVE_WHY` | `crates/victory-shader/src/emit/lower.rs:1502`, `crates/victory-shader/src/emit/lower.rs:1706`, `crates/victory-shader/src/emit/mod.rs:458`, `crates/victory-shader/src/emit/mod.rs:651` | true if set (any value) | `VICTORY_WAVE_WHY=1`: why a program keeps 64-lane waves (one line per reason) | neither |
| `VICTORY_WAVE_WIDE` | `crates/victory-gpu/src/shader_cache.rs:66`, `crates/victory-shader/src/emit/mod.rs:542`, `crates/victory-shader/src/emit/mod.rs:649` | see site (explicit policy over defaults) | Wave64 program form switch (barrier form for 64-lane programs); keyed into the shader cache. | neither |

## Test-only and child-process names

Read only inside tests, `#[ignore]` tools or benchmarks (run with
`cargo test … -- --ignored`), or set by a test for a child process it spawns.
Tests that need real files take paths from these and skip when unset
(docs/asset-policy.md, enforcement point 4).

| name | first site | purpose |
|---|---|---|
| `VICTORY_A`, `VICTORY_B`, `VICTORY_NONE_SUCH` | `crates/victory-gpu/src/ab.rs:123` | placeholder names in `VICTORY_AB` parser tests |
| `VICTORY_BCPACK_TABLES`, `VICTORY_BCPACK_CASES` | `crates/victory-formats/src/bcpack.rs:1576` | external table file / directory of `<name>.bc<n>` + `<name>.out` cases for the BCPack decoder test |
| `VICTORY_XMA_REFERENCE` | `crates/victory-formats/src/xma.rs:1788` | dir with `mono/stereo.xmadata` + `.ref` reference decodes (made outside the repo) |
| `VICTORY_XMA_STREAMS` | `crates/victory-formats/src/xma.rs:1743` | dir of saved `*.xma` + `*.ctx` inputs (from `VICTORY_XMA_DUMP`) to walk and decode |
| `VICTORY_BENCH_CS`, `VICTORY_BENCH_N`, `VICTORY_BENCH_S`, `VICTORY_BENCH_THREADS`, `VICTORY_BENCH_WARP`, `VICTORY_BLOCK_WAY` | `crates/victory-gpu/src/indirect.rs:974` ff. | ignored card measurements (another shader source, runs, seconds, threads, WARP; `BLOCK_WAY`/`BENCH_S` are passed to child processes via `.env`, `indirect.rs:1785`) |
| `VICTORY_BENCH_FILE` | `crates/victory-gpu/src/texture.rs:2225` | texture-upload benchmark: also time an unbuffered read of this file |
| `VICTORY_BENCH_HOLES` | `crates/victory-gpu/src/host.rs:10455` | `0`: views without holes in a benchmark |
| `VICTORY_TEST_ROUNDS` | `crates/victory-gpu/src/host.rs:10135` | rounds of a safe-copy stress test (default 120) |
| `VICTORY_TILES_TEST_CARD` | `crates/victory-gpu/src/tiles.rs:800` | `1`: tiles test on the default adapter instead of WARP |
| `VICTORY_TEST_D3D_DEBUG` | `crates/victory-gpu/tests/replay.rs:1598` | debug layer in the replay integration test |
| `VICTORY_CONSOLE_DXC`, `VICTORY_PC_DXC`, `VICTORY_DIFF_ONLY` | `crates/victory-shader/src/emit/diff_tests.rs:656` | differential translator tests against a console and a PC `dxc` (`DIFF_ONLY` = one case) |
| `VICTORY_PROBE` | `crates/victory-shader/src/emit/probe_tests.rs:15` | `#[ignore]` tool: run each `.dxil` in a folder on the card |
| `VICTORY_REF_VS`, `VICTORY_REF_PS` | `crates/victory-shader/src/emit/mod.rs:1690` | files substituted for the test's pixel / vertex shader before validation |
| `VICTORY_EXIT_CHILD` | `crates/victory-loader/src/threads.rs:407` | marks the child process of the thread-exit test |
| `VICTORY_TRAP_LOG` | `crates/victory-loader/src/thunks.rs:160` | crash-log path for the child that calls a trapping unimplemented import |
| `VICTORY_HOST_LOCK_CHILD` | `crates/victory-hle/src/host.rs:172` | marks a host-lock test child |
| `VICTORY_SAVE_CHILD`, `VICTORY_SAVE_OP`, `VICTORY_SAVE_STOP` | `crates/victory-hle/src/modules/xgamesave.rs:998` | crash-safety test of saves: child process, operation, step at which it stops |

## Known oddities (verified in code)

* `VICTORY_HOST_WAVE` (`apps/victory/src/main.rs:208`) and `VICTORY_HOST_MAX_WAVE`
  (`apps/victory/src/translate.rs:250`) both call `set_host_max_wave`; the first
  applies to every `translate` invocation, the second only to the non-`--warm` path.
  `--help` for `translate --warm` names `VICTORY_HOST_WAVE`.
* `VICTORY_CARD_ROOM_FOR_BUFFERS` reads "true unless `0`" (default ON,
  `crates/victory-gpu/src/shadow.rs:415`), while the comment at `shadow.rs:386` and
  docs/README-fxceless.md say `=1`, off by default (the later comment at `shadow.rs:405` matches the code).
* `VICTORY_D3D12_DRED`: `play.ps1` sets `light`, which only a diagnostics build
  reads (`crates/victory-gpu/src/host.rs:1678`); `Play-VIctory.cmd` runs a
  release build and does not set it.
* `VICTORY_PIPELINE_BOOK=1` is needed for `victory warm` to know any pipeline
  (`crates/victory-gpu/src/pipeline/known.rs:41`), and neither launcher sets it.
* Output folders given by `VICTORY_CAPTURE`, `VICTORY_SHADER_SAVE`,
  `VICTORY_RECORD`, `VICTORY_FAIL_DUMP`, `VICTORY_DUMP_MATCH`,
  `VICTORY_XMA_DUMP`, `VICTORY_MEMORY_DUMP`, `VICTORY_DSTORAGE_LOG`,
  `VICTORY_SHADER_CENSUS`, `VICTORY_POP_FOLDER` … are **not** passed through
  `ensure_outside_repo`; only `VICTORY_EMIT_DUMP` is
  (`apps/victory/src/translate.rs:245`). Point them outside the checkout yourself.

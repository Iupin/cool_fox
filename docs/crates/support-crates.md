# Support crates: victory-core, victory-formats, victory-pipeline, victory-game, xtask

The small crates around the big three. For the loader, runtime, GPU back end and
shader translator see [victory-loader.md](victory-loader.md),
[victory-hle.md](victory-hle.md), [victory-gpu.md](victory-gpu.md),
[victory-gpu-memory.md](victory-gpu-memory.md),
[victory-shader.md](victory-shader.md), [victory-pe.md](victory-pe.md).
CLI: [victory-cli.md](victory-cli.md). Tooling and policy mechanics:
[../reference/repo-tooling.md](../reference/repo-tooling.md).

| crate | lines | depends on (internal) | used by |
|---|---:|---|---|
| `victory-core` | 456 | – | `apps/victory`, `victory-pipeline`, `victory-loader` (`ensure_outside_repo` in `windows.rs:56`) — the only three dependents |
| `victory-formats` | 5,368 + 78 KB table | – (no dependencies at all) | `victory-gpu` (`texture::Layout`), `victory-hle` (`texture` in `modules/xg.rs`, `bcpack` in `modules/dstorage.rs`, `xma` in `modules/audio.rs`) |
| `victory-pipeline` | 259 | `victory-core` | `victory-gpu::shader_cache` only |
| `victory-game` | 243 | – (`memchr`, `zlib-rs`) | `apps/victory` only (`identify` in `run.rs:59`, `sgd5` in `shader.rs`) |
| `xtask` | 356 | – (`clap`, `walkdir`) | `cargo xtask …`, the pre-commit hook, CI tests |

---

## victory-core

Shared plumbing: where the user's files live, the runtime half of the asset
policy, one logging setup. `crates/victory-core/src/lib.rs` re-exports
`Config`, `PolicyError`, `ensure_outside_repo`.

### Module map

| file | contents |
|---|---|
| `src/lib.rs` | module list and re-exports |
| `src/config.rs` | `Config` (`game_dir`, `cache_dir`, `data_dir`, `test_titles`, `source`), `Config::load`, `require_game_dir`, `cache_subdir`, `data_subdir`; private TOML structs `FileConfig { paths: FilePaths }` |
| `src/policy.rs` | `ROOT_MARKER = ".victory-root"`, `ensure_outside_repo`, `PolicyError { InsideRepo, Io }`, path resolution for not-yet-existing paths |
| `src/logging.rs` | `init(default_level)`: `tracing_subscriber` registry, `EnvFilter` from `VICTORY_LOG` → `RUST_LOG` → `default_level`, level cap `TRACE` with feature `diagnostics` else `INFO`, output to stderr with targets |

### Key APIs

| item | behaviour |
|---|---|
| `Config::load() -> Result<Config>` | file = `VICTORY_CONFIG` (non-empty) else `./victory.toml` relative to the **current directory**; missing default file is fine, missing explicit file is an error; TOML with `deny_unknown_fields`; then `resolve` applies env over file over default (`config.rs:76-101`) |
| `Config::cache_subdir(sub)` / `data_subdir(sub)` | `root.join(sub)` → `ensure_outside_repo` → `create_dir_all`; the only sanctioned way to get a writable derived-data folder |
| `Config::require_game_dir()` | error text pointing at `VICTORY_GAME_DIR` / `victory.example.toml` (currently no caller in the workspace) |
| default dirs | `default_dir(name)`: `%LOCALAPPDATA%` → `$XDG_CACHE_HOME` → `$HOME/.cache`, then `victory/<name>` (`config.rs:138`) |
| `ensure_outside_repo(path)` | resolves the longest existing prefix with `canonicalize`, re-appends the rest, normalises `..`; refuses if under the source tree this binary was compiled from (`CARGO_MANIFEST_DIR/../..`, only if it still holds `.victory-root`) **or** if any ancestor contains `.victory-root` (other clones) (`policy.rs:41-63`) |
| `logging::init("info")` | called once by `apps/victory/src/main.rs:171`; `try_init` so a second call is harmless |

Feature `diagnostics` (empty) only raises the logging cap.

### Tests

`config.rs` (8): env beats file, empty env ignored (game dir, test titles,
config path), `VICTORY_TEST_TITLES` splitting, `victory.example.toml` parses,
cache/data inside the repo refused. `policy.rs` (5): inside this repo refused,
`..` escapes back in refused, marker in another tree refused, unrelated temp
dir accepted, `..` in a missing tail resolves lexically.

### Recipe: add a config key

1. Add the field to `FilePaths` (or a new `#[serde(default)]` table on
   `FileConfig`; unknown keys are rejected, so old binaries will refuse a file
   that uses it).
2. Add the public field to `Config` and resolve it in `Config::resolve` with an
   env override (`env_path("VICTORY_<NAME>")` treats empty as unset).
3. If it is a path that receives derived data, expose it only through a
   `subdir`-style helper that calls `ensure_outside_repo`.
4. Document it in `victory.example.toml` (the test `example_config_parses`
   parses that file), print it in `show_config` (`apps/victory/src/main.rs:236`),
   and add it to [../reference/env-switches.md](../reference/env-switches.md).

---

## victory-formats

Pure platform-level formats: bytes in, bytes out, no I/O, no dependencies.
(The crate doc at `src/lib.rs:1-6` still lists BCPack and audio as "Planned";
both are implemented. Opus is decoded in `victory-hle` with `opus-pure`, and
zlib with `zlib-rs` in `victory-hle`/`-gpu`/`-game`, not here.)

### Module map

| file | lines | contents |
|---|---:|---|
| `src/lib.rs` | 10 | `pub mod bcpack; pub mod texture; pub mod xma;` |
| `src/texture.rs` | 1,513 | GFX10.3 (RDNA2) texture layouts after AMD addrlib (MIT notice in the file): `Swizzle` modes, `Desc`/`Layout` per plane (mip placement, mip tail, block dims, `offset`, `detile`/`tile`), `format(dxgi)`, `TextureDesc`/`Texture`/`Plane` (depth + separate stencil plane) |
| `src/bcpack.rs` | 1,608 | BCPack decoder (BC1, BC3-BC7; BC2 left out): 12/16 interleaved rANS lanes, 128 static models, back references and per-format dictionaries; `Tables::{builtin, parse}`, `decode` |
| `src/bcpack-tables.bin` | 77,982 B | the format's constant tables; the single asset-policy exception ([../asset-policy.md](../asset-policy.md)); `include_bytes!` at `bcpack.rs:124` |
| `src/xma.rs` | 1,827 | XMA2 (WMA Pro frames, 512 samples, 2048-byte packets) after FFmpeg `wmaprodec.c`: `packet`, `Packet`, `read_frame`, `Frame`, `Decoder` |
| `src/xma/tables.rs` | 410 | WMA Pro Huffman and level tables (from FFmpeg `wmaprodata.h`) |
| `examples/texture_layout_bench.rs` | – | `cargo run --release -p victory-formats --example texture_layout_bench`: `Texture::new` timing |

### Key APIs

| item | signature / behaviour |
|---|---|
| `texture::Swizzle(u32)` | console swizzle mode; consts `LINEAR`, `SW_256B_S/D`, `SW_4KB_S/D(_X)`, `SW_4KB_R_X`, `SW_64KB_S/D(_T/_X)`, `SW_64KB_Z_X`, `SW_64KB_R_X`; `block_log2()`, `xor()` |
| `texture::Layout::new(&Desc) -> Result<Layout, Error>` | one plane; `Error::{Invalid, Unsupported, TooSmall}` |
| `Layout::offset(mip, x, y, z)` | byte address of an element |
| `Layout::detile(tiled, mip, slice, linear, row_pitch, depth_pitch)` / `tile(...)` | copy one subresource between tiled memory and linear rows |
| `Layout::bytes_from(first_mip)`, `subresource_depth`, `addressable` | sizes for partial uploads |
| `texture::format(dxgi) -> Option<Format{bytes, block, stencil}>` | `None` for 96-bit, 1-bit, 4:2:2 and video formats (marked `shortcut:`) |
| `texture::Texture::new(&TextureDesc)` | all planes, placed one after another; `mips: 0` = full chain; no HTILE/CMASK/FMASK/DCC planes (`shortcut:`, so sizes can be smaller than the console's) |
| `bcpack::Tables::builtin()` | parse the shipped table file (`TABLES_SIZE` checked) |
| `bcpack::decode(bc, src, dst, &tables)` | `dst.len()` gives the block count; trailing input ignored; truncated input is `Error` |
| `xma::packet(data, index)`, `xma::read_frame(data, start)` | packet header / frame walk |
| `xma::Decoder::new(channels, rate)`, `decode(data, bits, &mut Vec<i16>)`, `flush`, `reset` | PCM out |

### Tests

`texture.rs` 18 (addresses are a bijection and copies round-trip, hand-checked
addresses, matches addrlib, mip tail first, depth/stencil two planes, pitch
overflow refused, …). `bcpack.rs` 12 (shipped tables parse, BC4/BC7 blocks,
lane schedule, malformed input/noise never panics; 2 `#[ignore]`: throughput
benchmark, and a reference test needing `VICTORY_BCPACK_TABLES` /
`VICTORY_BCPACK_CASES`). `xma.rs` 16 (IMDCT matches direct sum, packet/frame
walks, noise decodes; 3 `#[ignore]`: benchmark, `VICTORY_XMA_STREAMS`,
`VICTORY_XMA_REFERENCE`). All tests use synthetic data; real-data tests take
paths from env vars and are ignored by default.

---

## victory-pipeline

Offline conversion framework: a `Converter` trait and a content-addressed
on-disk cache. Today its only client is `victory-gpu::shader_cache`
(translated shaders, `impl Converter for Translate<F>` at `crates/victory-gpu/src/shader_cache.rs:132`); no other product type implements `Converter`.

### Module map

| file | contents |
|---|---|
| `src/lib.rs` | crate doc; `pub trait Converter: Send + Sync { fn id(&self) -> &'static str; fn version(&self) -> u32; fn convert(&self, &[u8]) -> anyhow::Result<Vec<u8>>; }`; re-exports `Cache`, `CacheKey` |
| `src/cache.rs` | `CacheKey` (BLAKE3 of `id`, `0`, `version` LE, input), `Cache { root }`, `open`, `path_for`, `get_or_convert`, staging-file helper |

### Behaviour

| item | behaviour |
|---|---|
| `Cache::open(root)` | `ensure_outside_repo(root)` then `create_dir_all` |
| `Cache::path_for(key)` | `root/<first 2 hex>/<remaining 62 hex>` |
| `Cache::get_or_convert(conv, input) -> Result<PathBuf>` | hit if the file exists (no content check); else convert, write to `.tmp-<pid>-<n>` in the same folder (`create_new`, up to 128 names), `rename` over the final path; the staging file is removed on failure |

Tests (7): convert once then hit, version bump changes the key, failed publish
removes the staging file, staging retries collisions without touching
existing files, retries bounded at 128, other I/O errors not retried, cache
inside the repo refused.

### Recipe: add a converter

1. Implement `Converter` for a unit struct: a stable dotted `id()` (e.g.
   `"texture.detile"`), `version()` starting at 1, and a deterministic
   `convert`. The key covers only id, version and input bytes: **bump
   `version()` whenever output for the same input changes**, or fold any
   setting that affects output into the input or the id (as
   `victory-gpu::shader_cache` does with the translator fingerprint).
2. Open one `Cache` under a `Config::cache_subdir("<name>")` folder and call
   `get_or_convert`; the returned path is the output file.
3. Test with a temp dir (`tempfile`) and synthetic input only.

---

## victory-game

Title-specific code. Today: the build-identity table (empty) and the engine's
shader file format. No hooks or per-build offsets live here yet (the crate doc
and docs/architecture.md describe that as its purpose).

### Module map

| file | contents |
|---|---|
| `src/lib.rs` | `KnownBuild { label, blake3 }`, `KNOWN_BUILDS: &[KnownBuild] = &[]`, `identify(blake3_hex) -> Option<&KnownBuild>` (case-insensitive) |
| `src/sgd5.rs` | SGD5 shader files: `MAGIC = b"SGD5"`, `Record { offset, name, kind, .. }`, `Record::unpack()` (raw deflate via `zlib-rs`, size-checked, 64 MiB cap), `is_sgd5`, `records` (found by the `0x000DDCE7` marker, header tables not decoded) |

Tests (3, `sgd5.rs`): reads synthetic records, corrupt stream does not unpack,
empty / mismatched / truncated streams handled.

### Recipe: add a title build identity

1. `victory inspect <exe>` prints `blake3 <hex>` of the original file (the same
   hash `run` passes to `identify`: `prepared.original_hash`, `run.rs:58`).
2. Add `KnownBuild { label: "<our label>", blake3: "<hex>" }` to
   `KNOWN_BUILDS`. Only the hash and our own label are allowed
   (docs/asset-policy.md "Allowed": build identities, per-build offsets under
   our names, byte-pattern signatures).
3. `victory run <exe>` then prints `build <label> (<hash>)`. Nothing else
   consults `KNOWN_BUILDS` today.

---

## xtask

Repository maintenance, run as `cargo xtask <cmd>` (alias in
`.cargo/config.toml`: `run --quiet --package xtask --`).

| file | contents |
|---|---|
| `src/main.rs` | clap `Cmd::{Policy { --staged }, InstallHooks}`; `workspace_root()` = parent of xtask's manifest dir |
| `src/policy.rs` | the static asset-policy check: `run(root, staged)`, `list_files`, pure `check(path, content) -> Vec<String>`, encoded-run detectors, `EXCEPTIONS` |

| command | does |
|---|---|
| `cargo xtask policy` | checks tracked **and** untracked-not-ignored files (`git ls-files -z --cached --others --exclude-standard`; without git, walks the tree minus `target`/`.git`); prints `asset policy: N files ok` or `POLICY: path: problem` lines and fails |
| `cargo xtask policy --staged` | checks `git diff --cached --name-only --diff-filter=ACMR`, reading the **index** content (`git show :path`) |
| `cargo xtask install-hooks` | `git config core.hooksPath .githooks` |

Rules of `check` (details in [repo-tooling.md](../reference/repo-tooling.md#asset-policy-mechanics)):
extension/name allowlist, 640 KiB size cap, forbidden leading magic, no NUL
bytes, no alphanumeric/`+/=` run over 512 chars, no `\x`/`0x` literal run over
512 hex digits.

Tests (6): `repository_passes_policy` (runs the full check on the working tree
— this is how CI enforces the policy), accepts source, rejects game-like files,
rejects smuggled hex text, `\x` byte strings, `xxd -i` arrays. Note: the
"too big" case in `rejects_game_like_files` uses 600 KiB of `a`, which is
under the 640 KiB cap; it still fails `check` only because it is a 614,400-char
alphanumeric run, so it no longer tests the size limit.

### Recipe: allow a new file type or an exception

Add the extension to `ALLOWED_EXTENSIONS` (or the exact name to
`ALLOWED_NAMES`) in `xtask/src/policy.rs`. An exception for one path goes in
`EXCEPTIONS` with a reason; docs/asset-policy.md says nothing else of the
BCPack kind is accepted, and `.gitignore` needs a matching `!` line if the
extension is ignored there.

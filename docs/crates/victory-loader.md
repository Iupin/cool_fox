# victory-loader

Runs a title's original x86-64 PE image natively inside our process. The
machine code needs no translation (console CPU is Zen 2); the loader makes the
**Windows loader** map the image as a real module (so debuggers, unwinding,
profilers and crash dumps see it) without letting it run anything or resolve
anything, then binds imports itself and starts the title.

## Dependencies and features

| Kind | What |
|------|------|
| deps | `victory-core` (`ensure_outside_repo`), `victory-pe`, `anyhow`, `thiserror`, `tracing`, `blake3`; `windows-sys` on Windows |
| dev-deps | `tempfile` |
| feature `diagnostics` | compiles `stacks.rs`, `profile.rs`, `crash::save_on_crash`; enabled by `victory-hle/diagnostics` |
| platform | only `prepare`, `resolve`, `publish` and `crash` are portable-declared; everything else is `cfg(windows)` (`lib.rs:26-45`). `crash` is declared without a `cfg` but imports `windows-sys` (a Windows-only dependency), so a non-Windows build would likely not compile (inference, not tried). |

Depended on by `victory-hle` (resolver trait, crash log, `disable_thread_calls`,
`locate`) and `apps/victory` (`run.rs`, the only driver).

## Module map

| File | Role |
|------|------|
| `lib.rs` | Crate doc (the 5-step plan), module gating, re-exports, `DLL_PROCESS_ATTACH/DETACH`. |
| `prepare.rs` | Pure byte patching: original image → `PreparedImage` (DLL flag, no entry, empty import dir, detached TLS callbacks). Portable, tested on the test binary. |
| `resolve.rs` | `ImportResolver` trait, `Resolution::{Found, Missing}`, `Provenance::{Hle, Host, Title}`. |
| `publish.rs` | `write_if_changed`: race-free publish of the prepared copy into the shared cache (per-process staging file + atomic rename). |
| `windows.rs` | `load` (write copy, `LoadLibraryExW`, TLS redirect), `load_title_dlls`/`load_title_dll` (dependency-first mapping), `WithTitleDlls` resolver, `LoadedImage::{bind_imports, attach, detach, notify, run_tls_callbacks, spawn_main, unload, export*}`. |
| `tls.rs` | Static-TLS fix for an **exe** mapped as a DLL: rewrites every `mov r64, gs:[0x58]` into a call to a per-register thunk adding `index*8`, sets `_tls_index` to 0. |
| `thunks.rs` | 32-byte stubs for unresolved imports; `UnimplementedPolicy::{Trap, ReturnZero}`; global call counter. |
| `threads.rs` | Our own `.CRT$XLAV` TLS callback that forwards `DLL_THREAD_ATTACH/DETACH` to attached title images; `DisableThreadLibraryCalls`; orderly process-exit detach with a 5 s watchdog. |
| `crash.rs` | Vectored handler + unhandled filter: emulates AMD-only instructions, reports faults as `module+rva` with registers, unwound stack, memory state, C++ throw type; `crashes.log`; panic hook. |
| `console_insn.rs` | Zen 2-only instruction emulation (clzero/clzero loops, clwb, monitorx/mwaitx, rdpru, SSE4a) from the vectored handler, with first-fault code patching. Header says ported from XCL (GPL-2.0-only). |
| `console_insn_decode.rs` | Decoder for those instructions; included as `console_insn::decode` via `#[path]` (`console_insn.rs:8`), not declared in `lib.rs`. |
| `write_sites.rs` | Optional load-time rewrite of listed write instructions (`or byte [m], imm`, moves, 16/32-byte vector stores) into "read first, write only if changed" stubs (`VICTORY_WRITE_SITES`). |
| `stacks.rs` | diagnostics: `VICTORY_STACKS` scheduled stack dumps of every thread; thread enumeration and per-thread unwind helpers. |
| `profile.rs` | diagnostics: `VICTORY_PROFILE` sampling profiler of the busiest threads. |
| `tests/load_probe.rs` | End-to-end: builds `tests/fixtures/hle-probe`, prepares, maps, binds, calls into it. |
| `tests/title_dlls.rs` | A title DLL that fails to prepare is named in the error. |

## Public API (used by `apps/victory` and `victory-hle`)

```text
prepare(&[u8]) -> Result<PreparedImage, PrepareError>              prepare.rs:44
PreparedImage { info /* of ORIGINAL bytes */, bytes /* patched */, original_hash: blake3 }
  .file_name() -> "victory-image-<first 16 hex of blake3>.dll"     prepare.rs:29
load(&PreparedImage, cache_dir) -> LoadedImage                     windows.rs:55
load_title_dlls(&ImageInfo, title_dir, cache_dir, wanted) -> Vec<(name, LoadedImage)>   :101
load_title_dll(name, title_dir, cache_dir, wanted)                 :117
WithTitleDlls { inner: &dyn ImportResolver, dlls }                 :163
LoadedImage::bind_imports(&dyn ImportResolver, UnimplementedPolicy) -> BindReport   :222
LoadedImage::{base, info, export, export_symbol, attach, detach, run_tls_callbacks,
              spawn_main(Arc<Self>, dlls) -> JoinHandle<Result<u32>>, unload}
BindReport { hle, host, title, missing: Vec<MissingImport{dll, symbol}> }
ImportResolver::resolve(&self, dll /* as written */, &ImportSymbol) -> Resolution
UnimplementedPolicy::{Trap (default), ReturnZero}; unimplemented_call_count()
report_crashes(); log_crashes_to(path); log_crash(text); report_unhandled(info);
note_title_output(line); locate(addr) -> "module+0xrva"
disable_thread_calls(base) -> Option<bool>
write_sites::{apply, apply_asked}
diagnostics: dump_stacks, dump_stacks_on_schedule, profile_on_schedule(phase), save_on_crash
```

## What "prepared image" means and where it lives

`prepare` (`prepare.rs:44`) copies the original bytes and patches, without
moving anything (same length, same sections):

| Patch | Why |
|-------|-----|
| `IMAGE_FILE_DLL` set in file characteristics | `LoadLibraryExW` maps it as a DLL (relocated, unwind tables registered, static TLS allocated). |
| `AddressOfEntryPoint = 0` | Windows calls nothing on load. |
| Import directory → its own null terminator (RVA = start + count*20, size 20); bound-import dir zeroed | Windows resolves nothing; **the empty table is kept on purpose** because Windows 10 only assigns static TLS to a loaded DLL that has an import table (`prepare.rs:67`). IAT directory untouched. |
| TLS `AddressOfCallBacks = 0`, and the DIR64 base relocation for that field turned into `IMAGE_REL_BASED_ABSOLUTE` | OS still allocates the TLS block, but callbacks run only when we call them after binding. Without killing the reloc, a rebase (Wine always rebases DYNAMIC_BASE) would turn 0 into `delta` and the OS would read garbage callbacks (`prepare.rs:97`). |
| `FORCE_INTEGRITY` cleared, checksum zeroed | The copy is unsigned. |

`PreparedImage.info` is parsed from the **original**: real entry point,
imports, TLS callbacks. Cache location: `load` writes the bytes to
`<cache_dir>/images/victory-image-<hash16>.dll` (`run.rs:64` passes
`cfg.cache_subdir("images")`), via `publish::write_if_changed`: skipped if the
file already has identical bytes; otherwise staged to
`.<name>.<pid>.<seq>.tmp` and renamed. `victory_core::ensure_outside_repo`
refuses a cache dir inside the checkout (asset policy). Title DLLs get the same
treatment under the same folder. Crash reports and `locate` therefore name
title code as `victory-image-<hash16>.dll+0x<rva>`; RVAs equal the original
exe's.

## Sequence: `victory run <exe> --execute` to the entry point

Driver: `apps/victory/src/run.rs:39` (`run`). Loader calls in **bold**.

```text
check_cpu_features (AVX2; VICTORY_SKIP_CPU_CHECK=1 bypasses)       run.rs:51
lifecycle::wait_for_previous()      (relaunch handshake, HLE)
Config::load; read exe bytes
**prepare(original)**  -> blake3 -> victory_game::identify (build label)
d3d12x::use_shader_cache(cache/shaders)   (HLE -> victory-gpu)
**load(&prepared, cache/images)**                                    windows.rs:55
   write_if_changed -> LoadLibraryExW(path, 0)   OS maps + relocates + TLS slot
   if original is an exe: **tls::redirect(base, info)**              tls.rs:32
   threads::mapped(image)   (registered, not yet attached)
drop prepared/original bytes (they were ~2x image size of RAM)
HLE state: set_main_image, set_image_path, Title::from_manifest(MicrosoftGame.config),
   mount G: = exe folder, T: = <data>/<TITLEID>/temp, set_title, console::set
**load_title_dlls(info, title_dir, cache/images, wanted)**           windows.rs:101
   wanted(dll) = !HostPolicy.allows(dll) && no HLE module serves(dll)   run.rs:286
   recursive over imports + delay imports, dependencies first, each prepared+loaded
absent title DLLs (wanted but no file) -> bail under Trap policy, warn under zero
resolver = WithTitleDlls { inner: HleResolver(HostPolicy::default | none if --strict), dlls }
**dll.bind_imports(...)** for each title DLL, then **image.bind_imports(...)**  windows.rs:222
   every IAT slot of imports AND delay imports: resolver -> address, else stub
   stubs: one ThunkBlock (RW, sealed RX), Box::leak'ed ImportSite per import
   write_slots: VirtualProtect RW per 8-byte slot, write, restore
**write_sites::apply_asked(image_name, base, size)**  (no-op unless VICTORY_WRITE_SITES)
(without --execute: print report, return)
**report_crashes()**  vectored handler (appended last) + SetUnhandledExceptionFilter
**log_crashes_to(<data>/<TITLEID>/crashes.log)**  + panic hook
runtime::use_console_timers (1 ms timer, no power throttling, ABOVE_NORMAL class)
diagnostics (feature): stacks, profile, audio capture, VICTORY_WATCH/POKE/CRASH_SAVE
runtime::set_title_dll_loader(LateDlls)   for LoadLibrary of unimported title DLLs
**Arc::new(image).spawn_main(dlls)**                                  windows.rs:386
   new thread "title-main", stack = max(SizeOfStackReserve, 1 MiB)
   for each dll in order: attach()  = TLS callbacks(PROCESS_ATTACH) + DllMain;
                          FALSE -> error "attaching <name>"; threads::attached
   exe: run_tls_callbacks(PROCESS_ATTACH); threads::attached(base)
   entry(base + entry_rva)(NULL)  -> CRT start -> title main
main.join(); print "title exited with <code>"
```

Relocations are applied by the Windows loader (the image is mapped wherever
it fits; `has_relocations == false` only adds a hint to the
`LoadLibraryExW failed` error). Delay-load slots are bound eagerly so the
title's own delay-load helper never runs (`windows.rs:231`).

### Late title DLLs

`LoadLibrary*` from title code reaches `victory-hle::modules::kernel`, whose
`find_module` calls `runtime::load_title_dll`, i.e. `LateDlls::load`
(`run.rs:328`): under a re-entrant loader-lock lookalike (`LoaderLock`,
`run.rs:405`), `load_title_dll` maps the DLL and its title dependencies,
binds them against `WithTitleDlls{ WithTitleDlls{HLE, already mapped}, new }`,
publishes them, then attaches them in order on the calling thread. If a
`DllMain` fails: detach what was attached (reverse), forget and unmap all of
this load, return the error (as `LoadLibrary` would).

## Memory layout decisions

| Thing | Where | Why |
|-------|-------|-----|
| Title image, title DLLs | Wherever `LoadLibraryExW` puts them (ASLR, relocated by OS) | Real modules: `RtlLookupFunctionEntry` and unwinding work. |
| Unimplemented-import stubs | `VirtualAlloc` anywhere, RW then RX, leaked (`thunks.rs:92`) | Stub uses `mov rax, imm64; jmp rax`, no reach requirement. |
| TLS thunks (16 x 32 B) | One page within +-1 GiB of the image, searched in 64 KiB steps both sides (`tls.rs:143`); leaked | Patched sites are `call rel32`. Fails if the image is >= 1 GiB. |
| clzero / write-site stubs | 64 KiB RWX within `jmp rel32` reach (`console_insn::near`, `console_insn.rs:117`) | Sites become `jmp rel32` to stub and back. |
| Import site records | `Box::leak` per unresolved import | Stub holds a `&'static ImportSite`. |

## Key mechanisms

### Import binding and the stub

Each unresolved import gets 32 bytes (`thunks.rs:113`):
`mov rcx, &ImportSite; mov rax, unimplemented_import; jmp rax` plus `int3`
padding. The jump keeps the title's return address, so the handler returns
straight to the caller. `unimplemented_import` (`thunks.rs:53`): increments the
global and per-site counters; `ReturnZero` warns once per site and returns 0
in RAX (it does not touch other outputs); `Trap` logs, writes
`VIctory: unimplemented import <dll>!<symbol> called` to the crash log
(`abort` skips the handlers that would), `DebugBreak` if a debugger is
attached, then `std::process::abort()`.

### TLS redirection (`tls.rs`)

An exe assumes TLS index 0 and reads `gs:[0x58]` (TEB TLS array) then `[r]`
directly. After `LoadLibraryExW`, `redirect` reads the index the OS wrote via
the directory's `AddressOfIndex` (offset 16); if 0, nothing to do. Otherwise
every executable section is scanned **by byte pattern, not decode**
(`65 48|4C 8B modrm(mod=00,rm=100) 25 58 00 00 00`, reg != rsp) and each
9-byte load becomes `call thunk[reg]; nop4`. The thunk does
`mov r, gs:[0x58]; lea r, [r + index*8]; ret` (flags preserved). Finally
`_tls_index` is set to 0 so CRT code that adds it is consistent. The
`shortcut:` comment at `tls.rs:85` records that this was checked against a full
disassembly for one title; a new title could produce a false match. DLLs are
not redirected (they read `_tls_index` properly).

### Thread notices (`threads.rs`)

The prepared copies have no entry point/callbacks, so Windows never tells them
about threads. Our exe carries a TLS callback in `.CRT$XLAV` (sorts before
`XLB`, `threads.rs:237`); for every thread of the process it forwards
`DLL_THREAD_ATTACH` (load order) / `DETACH` (reverse) to every **attached**
image: TLS callbacks, then `DllMain` unless `DisableThreadLibraryCalls`
(`quiet`). `disable_thread_calls` refuses images with static TLS, as Windows
does. For threads started by Rust std, our own static TLS block is saved
before and restored after (`OwnTls`), because std aborts if its per-thread
state was touched before its own setup (title callbacks reach our logging,
which asks for the thread id).

Exit: `attached()` registers `detach_all` with the host CRT's `atexit` once
(`threads.rs:59`), so it runs after the title's exit functions. It sets
`EXITING`, spawns a 5 s watchdog (`DETACH_DEADLINE`) that `TerminateProcess`es
without heap or locks, terminates every other thread except ntdll thread-pool
workers (`end_other_threads`), then sends `DLL_PROCESS_DETACH` (reserved =
1, "process ending") to DLLs in reverse attach order, then the exe's TLS
callbacks.

### Crash reporter (`crash.rs`)

`report_crashes` (`crash.rs:112`) installs `AddVectoredExceptionHandler(0, handler)`
(appended **last**, so handlers registered with `first=1`, e.g. write guards
elsewhere, run before it; inference from the API, not checked against
victory-gpu) and `SetUnhandledExceptionFilter`. The title later replacing the
filter is intercepted by `victory-hle::kernel::SetUnhandledExceptionFilter`,
which keeps ours in front and chains to the title's.

`handler` (`crash.rs:469`), in order:
1. `EXCEPTION_ILLEGAL_INSTRUCTION` → `console_insn::emulate` (may patch the
   site on first fault); non-zero result returned.
2. `emulate_amd_only` (`crash.rs:145`): older fallback for `clzero`,
   `monitorx`/`mwaitx`, `rdpru` (returns TSC for MPERF/APERF), clwb and clwb
   loops, SSE4a `extrq`/`insertq` → `CONTINUE_EXECUTION`.
3. Codes in `FATAL` (AV, illegal/priv instruction, int div0, stack overflow,
   breakpoint) are printed as `first-chance exception` (stderr only).
4. `CONTINUE_SEARCH`.

Unhandled exceptions (`report_unhandled`, `crash.rs:496`) are printed **and**
appended to the log. Report format (`report`, `crash.rs:562`):

```text
VIctory: unhandled exception 0xc0000005 at victory-image-<h>.dll+0x1234 (write 0x10: nothing mapped there)
  rax .. rbp
  r8 .. r15
  #0  victory-image-<h>.dll+0x1234
  #1  ...                       (<= 40 frames, RtlVirtualUnwind; leaf/no-unwind code: [rsp])
  title said: <last 8 OutputDebugString lines>
```

Details per code: AV gives kind and `memory_at` (free / reserved-not-committed
/ guard page / protection, with module); MSVC C++ throws (`0xE06D7363`) give
the decorated type name and an integer value or `std::exception::what()`;
illegal/privileged instruction gives up to 15 instruction bytes (never past
the page). Log entries are prefixed `unix time <s>, process <pid>`. Panics of
ours and `log_crash` texts (host GPU failure, commit-limit messages from
`victory-hle::xmem`, trapped imports) go to the same file.
`save_on_crash` (diagnostics) dumps named memory ranges on a report.

## Environment switches read in this crate

| Name | Values / default | Effect | Where |
|------|------------------|--------|-------|
| `VICTORY_CLZERO_ASK` | `1` / unset | `1`: query `VirtualQuery` for every emulated instruction instead of remembering writable regions per thread. | `console_insn.rs:28` |
| `VICTORY_PATCH_WAITS` | `0` / on | `0`: emulate `monitorx`/`mwaitx` every time instead of patching them to no-op/`pause` on first fault. | `console_insn.rs:57` |
| `VICTORY_PATCH_CLZERO` | `0` / on | `0`: emulate `clzero` loops every time instead of patching to a plain-store stub. | `console_insn.rs:76` |
| `VICTORY_WRITE_SITES` | file path / unset (off) | Lines `<image file name> <hex offset>`; each site rewritten at load (`apply`, `write_sites.rs:226`). The name must match the mapped file (`victory-image-<h>.dll` for the exe, per `run.rs:71`). | `write_sites.rs:288` |
| `VICTORY_STACKS` | `<s>[,<s>...]` / unset | diagnostics: every thread's stack as `module+rva` at those seconds. | `stacks.rs:27` |
| `VICTORY_PROFILE` | `<from>,<to>,<ms>,<file>[;...]` / unset | diagnostics: samples the 28 busiest threads every `<ms>`, writes counted stacks. | `profile.rs:64` |
| `VICTORY_TRAP_LOG`, `VICTORY_EXIT_CHILD` | test-only | child-process markers for `thunks` and `threads` tests. | `thunks.rs:160`, `threads.rs:407` |

Read by the driver, not this crate, but part of the same flow
(`apps/victory/src/run.rs`): `VICTORY_SKIP_CPU_CHECK=1` (`:8`),
`VICTORY_CRASH_SAVE=<rva>:<len>,...` (`:212`), `VICTORY_WATCH` (`:232`),
`VICTORY_POKE` (`:263`), the last three diagnostics-only.

## Invariants and unsafe hot spots

- **Order is the contract**: bind before any TLS callback/`DllMain`/entry;
  attach DLLs dependencies-first before the exe; `tls::redirect` and
  `write_sites::apply` only while no title code runs (they rewrite code
  without suspending threads).
- `LoadedImage` is `Clone` without a reference count; `unload` exactly one
  copy. `threads::IMAGES` keeps `Arc` clones.
- All stubs, thunks and `ImportSite`s are intentionally leaked.
- `write_slots` trusts `iat_slot_rva`; `victory-pe` guarantees it is in-image.
- `unwind_into` copies `CONTEXT` into a 16-byte-aligned wrapper (`Aligned16`)
  because `RtlVirtualUnwind` uses aligned stores and windows-sys's `CONTEXT`
  is not declared aligned. It never allocates beyond `MAX_FRAMES` capacity
  (another thread may be suspended holding the heap).
- `title_output()` uses `try_lock` so a crash on a thread holding `SAID`
  does not deadlock the reporter.
- Emulation in the vectored handler runs for **every** illegal-instruction
  exception in the process, ours included.

## Gotchas / surprises

- Two emulators overlap: `console_insn::emulate` and `crash::emulate_amd_only`
  both handle clzero/clwb/mwaitx/rdpru/SSE4a. The first wins; the second is a
  fallback. Changing one without the other can hide bugs.
- `console_insn.rs` states it was ported from XCL (GPL-2.0-only), and
  `console_insn_decode.rs` is that decoder "unchanged". The root README says
  "no code of it is here". This is a licensing question for the maintainers,
  not a code question.
- The TLS patch is a byte match (see above).
- `bind_imports`'s `tracing::info!` omits the `title` count; the driver
  prints all four.
- The crash reporter is installed only with `--execute`, after binding:
  faults during prepare/load/bind are ordinary Rust errors.
- `load` writes into the cache even without `--execute` (it must map).

## How to

- **Diagnose an unimplemented-import trap**: the console and `crashes.log`
  show `VIctory: unimplemented import <dll>!<symbol> called` (with the title's
  last debug lines). `victory run <exe>` without `--execute` lists every
  `missing <dll>!<symbol>` at bind time; `victory census <exe> --missing`
  does it without mapping. Implement it in `victory-hle` (see that doc), or
  pass `--unimplemented zero` to push through (returns 0, logs once).
- **Read a crash report**: `victory-image-<h>.dll+0xRVA` is an RVA of the
  original exe (open it in a disassembler at preferred base + RVA). Our own
  frames appear as `victory.exe+rva`. AV text tells wild pointer vs failed
  commit vs guard page vs write-to-code. Windows error 1455/1450 on commit
  means the paging file is too small (see README-fxceless).
- **Add a patch to the prepared image**: add the offset to
  `victory_pe::layout`, patch in `prepare`, extend `patches_own_test_binary`.
  Changing prepared bytes changes nothing in the file name (hash is of the
  original), but `write_if_changed` republishes.
- **Find where a thread waits**: diagnostics build + `VICTORY_STACKS=20,60`.

## Tests

`cargo test -p victory-loader` (Windows; many tests are `cfg(windows)`).
Unit tests: `crash.rs` 19 (unwind vs system walk, report text, emulation),
`prepare.rs` 4 (including `windows_assigns_static_tls_to_a_prepared_dll`),
`console_insn.rs` 3, `write_sites.rs` 4, `threads.rs` 2 (exit in a child
process), `thunks.rs` 2 (trap in a child process, checks `crashes.log`),
`publish.rs` 2, `tls.rs` 1 (patches and executes real loads for rax/r8/r12),
`stacks.rs` 2, `profile.rs` 1, `console_insn_decode.rs` 2.

Integration: `tests/load_probe.rs` builds `tests/fixtures/hle-probe` with a
nested `cargo build --manifest-path tests/fixtures/hle-probe/Cargo.toml
--target-dir $CARGO_TARGET_TMPDIR/hle-probe` (needs the MSVC toolchain and
`delayimp.lib`). The probe is a cdylib (not a workspace member) that imports
`xgameruntime!XGameRuntimeInitialize`, a missing DLL, a "title DLL"
(`victory-probe-title.dll`, the test copies System32 `version.dll` under that
name), a delay-loaded DLL (`/DELAYLOAD`, `build.rs`), and `kernel32`; it has
a `.CRT$XLB` TLS callback and a `DllMain` counting thread starts and detaches.
The test checks HLE/host/title/missing counts (3/>0/1/1), calls each probe
export, checks thread notices reach both TLS callback and `DllMain` for std and
`CreateThread` threads, `disable_thread_calls` refusal, and detach. Because the
probe is a DLL, `tls::redirect` (exe-only) is **not** exercised by it.
`tests/title_dlls.rs` checks the error names a broken DLL. No game data needed.

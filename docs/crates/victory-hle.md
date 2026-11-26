# victory-hle

Our implementation of the Xbox system API surface a title imports ("high-level
emulation"): console-only DLLs (`xgameruntime.dll`, `xmem.dll`, `d3d12_xs.dll`,
`XCRDAPI.dll`, `gameinput.dll`, audio coprocessors...), overrides of shared
Windows APIs whose console meaning differs (paths, memory, module lookup,
network), and the policy for which host DLLs titles may use directly. GPU work
is handed to `victory-gpu`; codecs come from `victory-formats` and `opus-pure`.

Read first: [architecture.md](../architecture.md) ("Import resolution"),
[victory-loader.md](victory-loader.md) (how imports get bound and the title
starts).

## Dependencies and features

| Kind | What |
|------|------|
| deps | `victory-gpu` (D3D12.X device, title-memory tracking, console model, timeline), `victory-loader` (resolver trait, crash log, thread notices), `victory-pe` (`ImportSymbol`), `victory-formats` (XMA, BCPack, texture layouts), `opus-pure`, `zlib-rs`, `tracing`, `windows-sys` |
| optional | `windows`, `windows-core`: only for `diagnostics` (`modules/audio/capture.rs`, WASAPI loopback recorder) |
| feature `diagnostics` | enables `victory-gpu/diagnostics`, `victory-loader/diagnostics`, the audio recorder, `VICTORY_KEYS`, `VICTORY_XMA_DUMP`, per-call-site logging in `QueryApiImpl`, timeline wrapping of host imports, re-exports `frame_phase`, `replay` |
| platform | `lib.rs`, registry, `runtime`, most COM-area modules compile anywhere; `apu, dstorage, files, lifecycle, media, net, reads, xmem` are `cfg(windows)` (`modules/mod.rs:11-41`), and `modules::all` only registers them on Windows. |

## Public API used by other crates

| Item | Used by | Notes |
|------|---------|-------|
| `HleResolver { host: HostPolicy }`, `::new`, `::classify` | `apps/victory` (run, census) | implements `victory_loader::ImportResolver` (`lib.rs:51`) |
| `HostPolicy::{default, none, allows}` | run (`--strict` = `none()`), census | `host.rs` |
| `registry() -> &[HleModule]`, `HleModule::serves`, `HleExport`, `hle_export!` | run.rs `title_dll` filter | `registry.rs` |
| `runtime::{set_main_image, set_image_path, forget_image, image_named, set_title_args, set_title_dll_loader, Title, set_title, use_console_timers}` | run.rs, main.rs | process-wide state, `runtime.rs` |
| `modules::files::{mount, mount_at}` | run.rs mounts `G:` and `T:` | `files.rs:49`, `:67` |
| `modules::lifecycle::wait_for_previous` | run.rs | relaunch handshake |
| `modules::d3d12x::{use_shader_cache, precompile, warm_shader}` | run, translate, warm | `d3d12x.rs` |
| `modules::audio::capture::{start, child}` | main.rs, run.rs diagnostics | recorder child process |
| re-exports `console` (from `victory-gpu`), `known_pipelines`, `frame_phase`, `replay` | app | |
| `watch_writers()` (diagnostics) | run.rs diagnostics | starts the GPU side's writers watch (`VICTORY_WRITERS*`, `docs/crates/victory-gpu-memory.md` §3.4a) with the loader's `locate` and `unwind_here`; the CRT `exit`, `TerminateProcess` of the title's own process (`kernel.rs`) and the suspend's end (`lifecycle.rs` `end`) call `victory_gpu::writers_at_exit` for its last table |

## Module map

| File | Role |
|------|------|
| `lib.rs` | Crate doc, `HleResolver` (registry first, then host policy), re-exports, resolver tests. |
| `registry.rs` | `HleModule { name, dlls (lower-case patterns, trailing `*` = prefix), exports }`, `HleExport { name, address }`, `hle_export!`, lazily built `REGISTRY`, `lookup`. |
| `host.rs` | `HostPolicy` allow-list, `resolve` (System32-only `LoadLibraryExA`, `GetProcAddress`, then `victory_gpu::titlewrite::import` wrapping), `function(dll, name)` for our wrappers. |
| `runtime.rs` | Main image base, image paths by base, title args, late title-DLL loader hook, `Title` (manifest parser for `MicrosoftGame.config`), console timers and process priority. |
| `image_watch.rs` | diagnostics-style switch `VICTORY_IMAGE_WATCH`: logs named words of the title image once a second. |
| `modules/mod.rs` | Module list; `all()` builds the registry in a fixed order. |
| `modules/kernel.rs` | Module/library APIs (pseudo handles, title DLLs, `GetProcAddress` overrides), debug output, CRT exit paths, unhandled filter chaining, argv, thread priorities. |
| `modules/files.rs` | Virtual drives (`mount`, `mount_at`, `translate`) and every path-taking kernel32/CRT/msvcp import; the kernel reads into title memory (`ReadFile`, `fread`, `_read`) and `setvbuf`; `host!` macro. |
| `modules/reads.rs` | What the title reads, said aloud: names of handles/streams/descriptors (`GetFinalPathNameByHandleW`), rate-limited warnings (`tell`), how a destination is guarded (`guarded`), per-file lines for the first minute (`note`, `note_named`). |
| `modules/xmem.rs` | `xmem.dll` and `VirtualFree`: console virtual memory, allocation hooks, physical pages as pagefile sections, aliasing for the GPU, working-set stats. |
| `modules/d3d12x.rs` | `d3d12_xs.dll`: hands device creation, virtual-memory config and root signatures to `victory-gpu`; shader/pipeline cache dirs; host GPU failure → crash log + exit. |
| `modules/xg.rs` | `xg_xs.dll` texture-layout computer from `victory_formats::texture`. |
| `modules/dstorage.rs` | `XCRDAPI.dll` DirectStorage: factory, files, queues, status arrays, worker pool with weighted round robin, zlib/BCPack, page destinations. |
| `modules/gameinput.rs` | `gameinput.dll` (ordinal `#1` = `GameInputCreate`): XInput pads, keyboard, stand-in pad, readings, callbacks, rumble. |
| `modules/audio.rs` | `AcpHal.dll`: APU heap, SHAPE contexts, coprocessor queues, XMA decoding thread. |
| `modules/audio/capture.rs` | diagnostics: `VICTORY_AUDIO_CAPTURE` process-loopback WAV recorder in a child process. |
| `modules/apu.rs` | `xapu.dll`: Opus decode commands (`opus-pure`). |
| `modules/dsp.rs` | `chad.dll`: DSP streams (reverb); commands complete, output silence. |
| `modules/media.rs` | `mfplat.dll!MFCreateSourceResolver` proxy: local files only, virtual-drive paths rewritten. |
| `modules/net.rs` | `ws2_32.dll`/`winhttp.dll` guard: loopback only. |
| `modules/rpcrt4.rs` | `rpcrt4.dll` GUID calls (`UuidCreate*`, `UuidFromString*`, `UuidToString*`, `RpcStringFree*`). |
| `modules/lifecycle.rs` | App-state/constrained notifications, `CreateWindowEx*` hook (window close = suspend + exit, `VICTORY_WINDOW_MAX`), relaunch. |
| `modules/xgameruntime.rs` | `xgameruntime.dll` exports; `QueryApiImpl`; shared COM-object plumbing (`Guid`, `Object`, `Methods`, `missing`, `put`, `put_string`). |
| `modules/xasync.rs` | XTaskQueue + XAsync (task-queue provider object). |
| `modules/xuser.rs` | One offline signed-in user. |
| `modules/xsystem.rs` | Console identity/analytics; registry override for the device-type value (`advapi32`). |
| `modules/xgame.rs` | Title id, feature checks, invites, events (offline). |
| `modules/storage.rs` | Persistent local storage as a virtual drive. |
| `modules/xgamesave.rs` | Game saves on host disk, atomic versioned containers. |
| `modules/xnetworking.rs` | Offline network status. |
| `modules/xpackage.rs` | The title's own package only. |
| `modules/xstore.rs` | Offline store: own licence active, nothing else. |
| `modules/xgameui.rs` | Profile card (closes), text entry (host dialog). |
| `modules/speech.rs` | Speech synthesis: valid empty WAVE, silent. |
| `modules/display.rs` | HDR, display-timeout deferrals, URI launch, screenshots (area objects). |
| `tests/no_dll_planting.rs` | Host pass-through never loads a same-named DLL from the current folder. |

## How an import is declared and registered

```text
modules/<area>.rs:
  pub extern "system" fn XMemAlloc(size: usize, attributes: u64) -> *mut c_void { .. }
  pub(crate) fn module() -> HleModule {
      HleModule { name: "xmem",
                  dlls: &["xmem.dll", "kernel32.dll", "api-ms-win-core-memory-*"],
                  exports: vec![ hle_export!(XMemAlloc),                    // name = Rust ident
                                 HleExport { name: "VirtualFree",           // name != ident
                                             address: title_virtual_free as *const () as usize } ] } }
modules/mod.rs::all()  -> vec![kernel, xgameruntime, gameinput, d3d12x, audio, dsp, xg,
                               (win) apu, media, dstorage, files, lifecycle, net,
                               rpcrt4, (win) xmem, xsystem("device type")]
registry.rs: static REGISTRY = LazyLock::new(modules::all)
lookup(dll, name): first module (in that order) that serves(dll) and has export `name`
```

- `hle_export!(f)` (`registry.rs:39`) = `HleExport { name: stringify!(f), address: f as usize }`.
  The Rust name must equal the imported name, hence `#![allow(non_snake_case)]`
  in most modules.
- Ordinal imports look up the string `"#<n>"` (`ImportSymbol`'s `Display`,
  `lib.rs:53`), e.g. gameinput's `name: "#1"`.
- **Data imports** work too: the address is the data's address
  (`d3d12x.rs`: `IID_ID3D12Device` → `&device::IID`).
- Several modules may serve the same DLL (`kernel`, `files`, `xmem` all serve
  `kernel32.dll`); there are currently no duplicate names across modules
  (checked by grep; the unit test `export_names_are_unique_per_dll_pattern`
  only checks within a module). On a clash, registry order decides.
- xgameruntime *areas* (`xasync`, `xuser`, ...) are **not** in `all()`: they
  are COM objects reached through `QueryApiImpl`, registered in
  `xgameruntime::AREAS`.

**Calling convention**: functions are `extern "system"` (Win64 ABI on
x86-64), CRT entry points `extern "C"` (identical on x64). Arguments are raw
pointers the title trusts; modules allow `clippy::not_unsafe_ptr_arg_deref`
and dereference after null checks. Do not panic in an export: a panic cannot
unwind through an `extern "system"`/`"C"` boundary, so Rust aborts the
process (current Rust semantics; the loader's panic hook still writes it to
`crashes.log` first, once `log_crashes_to` ran).

## Resolution and host pass-through

```text
HleResolver::resolve(dll, sym)                                     lib.rs:51
 1. registry::lookup(dll, sym.to_string())        -> Found(Hle)
 2. HostPolicy::resolve(dll, sym)                 -> Found(Host)
      allows(dll)?  (lower-case, '*' = prefix)
      imp::module: LoadLibraryExA(dll, LOAD_LIBRARY_SEARCH_SYSTEM32), cached by name,
                   loaded outside the cache lock (loader-lock deadlock, see test)
      GetProcAddress (name or ordinal)
      victory_gpu::titlewrite::import(name, addr)  memcpy/memmove/memset -> our wrappers
      diagnostics: victory_gpu::timeline::import(name, addr) wraps waits
 3. Missing   -> loader's WithTitleDlls tries title DLL exports, then a stub
```

Default allow-list (`host.rs:17`): `api-ms-win-core-*`, `api-ms-win-eventing-*`,
`api-ms-win-security-*`, `api-ms-win-crt-*`, `kernel32`, `kernelbase`, `ntdll`,
`ucrtbase`, `vcruntime140*`, `msvcp140*`, `ws2_32`, `bcrypt`, `crypt32`,
`ncrypt`, `winhttp`, `sspicli`, `cabinet`, `avrt`, `ext-ms-win-rtcore-ntuser-*`,
`api-ms-win-shcore-*`, `msvcp_win`, `ole32`, `advapi32`, `iphlpapi`, `dbghelp`,
`xaudio2_9`, `mfplat`, `mfreadwrite`. `--strict` uses `HostPolicy::none()`.

Run-time lookups go through `kernel.rs`:

- `LoadLibrary{A,W,ExA,ExW}` → `find_module` (`kernel.rs:302`): basename,
  `.dll` appended if no extension; for code loads first the late title-DLL
  loader (maps and attaches a title DLL on first use; failure →
  `ERROR_MOD_NOT_FOUND`, never the host); then an already-mapped title image
  by file name; then, if the name is **not** in `HostPolicy::default()`, an HLE
  module serving it → **pseudo handle** = address of the `HleModule`; host
  DLLs → the host; anything else is refused with `ERROR_MOD_NOT_FOUND` and
  logged once. Data/resource loads (`runs_code`) of unmapped title DLLs go to
  the host.
- `GetProcAddress` (`kernel.rs:153`): NULL module = title image; a pseudo
  handle searches **that module's** exports by name (ordinals return NULL with
  a warning); a host module first asks `host_override` (`kernel.rs:186`):
  the same registry lookup import binding would do, by the module's file
  name, so by-name lookups cannot bypass the network guard or path
  rewriting. Title images go to the host's `GetProcAddress` (real exports).
- `GetModuleHandle*(NULL)` → title image; `GetModuleFileName*` → the
  original file path recorded by `set_image_path`, not the cache copy.
  `FreeLibrary` on pseudo handles and title images is a no-op success.

## COM-like console interfaces

Three patterns exist; pick by whether the object needs state.

| Pattern | Where | Shape |
|---------|-------|-------|
| Static area object | `xgameruntime.rs` (`Object`, `Methods<N>`), all `QueryApiImpl` areas | `#[repr(C)] Object { vtable: *const *const (), area: &'static str }`; `static VTABLE: Methods<N> = Methods([QueryInterface, AddRef, Release, slot 0x18, ...])`; `add_ref` returns 1 for both AddRef and Release (static, no counting). |
| Stateful instance | `dstorage.rs:72` `Instance<T> { base: Object, refs: AtomicU32, state: T }` | `Box::into_raw`; `release::<T>` frees at 0; `this` cast back to `Instance<T>`. |
| Own tables | `gameinput.rs` (`Table<N>`, `Object`, `Device`, `Reading` refs) | Separate vtables per interface; readings are reference counted. |

`QueryApiImpl(provider, iid, out)` (`xgameruntime.rs`) searches
`AREAS = [xasync, xuser, xsystem, xgame, storage, xnetworking, xpackage,
display, xgamesave, xstore, xgameui, speech]` for an exact (provider, iid)
pair. Unknown pairs log `runtime API area not implemented` once per pair and
return `NOT_SUPPORTED` (`0x80070032`). An area's interface id changes when its
method table changes, so each area lists the ids whose slot layout we know
(e.g. `IID` and `IID_2026`). Unfilled slots use `missing::<SLOT>`, which logs
`runtime method not implemented area=.. slot=0x..` and returns `E_NOTIMPL`
(wrong for slots returning `bool`/values; a known hazard noted in code).
Helpers: `put` (null-checked out param, `E_INVALIDARG`), `put_string` (runtime
string-getter contract, `E_NOT_SUFFICIENT_BUFFER`), `text`, `token`.

Title-side flow: a stub linked into the title `LoadLibraryExA("xgameruntime.dll")`
(pseudo handle) → `GetProcAddress` for `InitializeApiImpl*`, `QueryApiImpl`,
`XErrorReport` → every runtime call queries an object, calls one slot,
releases.

## Module inventory and completeness

Legend: **impl** = implemented for what titles were seen to call; **partial**;
**offline** = deliberately answers as an offline retail console;
**stub** = accepts calls, does nothing useful; **guard** = filters host calls.

| Module | DLL / API area | Exports or areas | State |
|--------|----------------|------------------|-------|
| kernel | kernel32, kernelbase, `api-ms-win-core-{libraryloader,processthreads,debug,errorhandling}-*`, `api-ms-win-crt-runtime-*` | 22: `GetModuleHandle*`, `GetModuleFileName*`, `LoadLibrary*`, `GetProcAddress`, `FreeLibrary`, `DisableThreadLibraryCalls`, `OutputDebugString*`, `exit`, `abort`, `terminate`, `_invalid_parameter_noinfo_noreturn`, `TerminateProcess`, `SetUnhandledExceptionFilter`, `SetThreadPriority`, `__p___argc/argv` | impl (overrides; rest passes to host) |
| files | kernel32/kernelbase, `api-ms-win-core-{file,processenvironment}-*`, ucrtbase, `api-ms-win-crt-{stdio,filesystem,time}-*`, msvcp140 | 33 path functions (`CreateFile{W,A,2}`, `Delete*`, `*Directory*`, `*FileAttributes*`, `GetDiskFreeSpace*`, `FindFirstFile*`, `MoveFileExW`, `SetCurrentDirectoryA`, `fopen*`, `_wfopen*`, `_open/_wopen`, `_wstat64`, `_stat64i32`, `remove`, `_wchmod`, `_wutime64`, `std::_Fiopen` x2) + `ReadFile`, `fread`, `_read`, `setvbuf` | impl |
| xmem | xmem.dll, kernel32/kernelbase, `api-ms-win-core-memory-*` | 14: `XMem{Alloc,Free}{,Default}`, `XMemVirtualAlloc/Query`, `XMem{Allocate,Free,Map}PhysicalPages`, `...Scatter`, `XMemGetWorkingSetStatistics`, `XMemTransferMemory`, `XMemIsToolsMemAvailable` (0), `VirtualFree` | impl, with `shortcut:`s |
| d3d12x | d3d12_xs.dll | 6: `D3D12XboxCreateDevice`, `D3D12ConfigureVirtualMemoryX`, `D3D12Serialize{,Versioned}RootSignature`, `D3D12CreateRootSignatureDeserializer`, data `IID_ID3D12Device` | forwards to victory-gpu |
| xg | xg_xs.dll | `XGCreateTextureComputer`, `XGSuggestSwizzleMode`; computer slots 0x08/0x10/0x40/0x48/0x50 | partial (other slots log + fail) |
| dstorage | XCRDAPI.dll | `DStorageGetFactory`; factory/file/queue/status slots the title calls | impl (unknown slots log offset) |
| gameinput | gameinput.dll | `#1` (GameInputCreate) | impl (pads, keyboard; aggregate devices/dispatcher return errors) |
| audio | AcpHal.dll | 9: `AcpHal{Create,Allocate,Release}ShapeContexts`, `Apu{Alloc,Free,CreateHeap,IsVirtualAddressValid,MapApuAddress,MapVirtualAddress}` | partial: XMA decoded; other SHAPE units not emulated (those voices silent) |
| apu | xapu.dll | 5: `XApu{Connect,Disconnect,EnqueueCommand,DequeueResult,GetResultCount}` | impl (Opus, decoded at enqueue) |
| dsp | chad.dll | 5: `XDsp{ConnectWithMaximumStreamLimit,Disconnect,Activate,Deactivate,SubmitCommand}` | stub (completes, outputs silence; reverb dry) |
| media | mfplat.dll | `MFCreateSourceResolver` | guard (local files only; rest of MF is host) |
| net | ws2_32.dll, winhttp.dll | 33 connect/send/bind/lookup/WinHttp functions | guard (loopback only) |
| rpcrt4 | rpcrt4.dll | 8 GUID calls | impl; DLL not passed through |
| lifecycle | `api-ms-win-core-psm-appnotify-*`, `ext-ms-win-rtcore-ntuser-window-*` | `{Register,Unregister}App{State,Constrained}ChangeNotification`, `CreateWindowEx{W,A}` | impl |
| xsystem ("device type") | advapi32, `api-ms-win-core-registry-*` | `RegOpenKey{W,ExW}`, `RegQueryValueExW`, `RegCloseKey` (one OEM key answered with the chosen console model; others to host) | impl |
| xgameruntime | xgameruntime.dll | `XGameRuntime{Initialize,Uninitialize}`, `InitializeApiImpl{,Ex,Ex2}`, `UninitializeApiImpl`, `QueryApiImpl`, `XErrorReport` | impl |
| xasync (area) | task-queue provider | XTaskQueue*, XAsync* (31-slot table) | impl |
| xuser (area) | user provider | one signed-in user, all privileges | offline |
| xsystem (area) | system info, analytics | fixed identity, console model | offline |
| xgame (area) | title id, features, invites, events | invites never arrive, events dropped | offline |
| storage (area) | persistent local storage | path = virtual drive root, space/quota | impl |
| xgamesave (area) | XGameSave | local containers, atomic updates, no cloud | impl |
| xnetworking (area) | XNetworking | stack "up", reaches nothing | offline |
| xpackage (area) | XPackage | own package complete, no DLC | offline |
| xstore (area) | XStore | own licence active; service calls fail with store network error | offline |
| xgameui (area) | system dialogs | profile card closes; text entry host dialog, `E_ABORT` on cancel | partial |
| speech (area) | speech synthesis | empty WAVE | stub |
| display (area) | HDR, shell (URIs, deferrals), capture | see module | partial (unverified per slot) |

## Threading and async model

- **Title threads call us directly** on their own stacks. Most state is
  `static` behind `Mutex`/`RwLock`/`OnceLock`/`LazyLock`; poisoned locks are
  recovered (`unwrap_or_else(|e| e.into_inner())` everywhere).
- **XTaskQueue** (`xasync.rs`): a queue has a work port and a completion port,
  each in mode `Manual` (title calls `XTaskQueueDispatch`), `ThreadPool`,
  `Serialized`, or `Immediate` (runs on the submitting thread). Queue handles
  are `Arc::into_raw(Queue)`; duplicate/close add/drop a reference. A null
  handle = the process queue, created lazily as a thread-pool queue
  (`PROCESS_QUEUE`), clearable by the title. The thread pool (`Pool`) spawns a
  new worker whenever more tasks wait than idle workers and keeps workers
  forever, so a callback that blocks on another never starves (no upper
  bound on threads). Terminate cancels queued/delayed tasks (callbacks see
  `canceled`), refuses new work, then calls the title's callback.
- **XAsync**: in-flight calls are tracked by block address (`CALLS`), and the
  block's system area holds status and result size so they stay readable
  after our state is gone. Providers get `OP_BEGIN/DO_WORK/GET_RESULT/CANCEL/CLEANUP`.
  Completions are bound to the `Call`, not the block, so a late completion
  cannot end a newer call on the same block. Helpers for area authors:
  `finished(block, identity, hr)`, `finished_with(block, identity, Ok(bytes))`
  (synchronous results: the call completes inside its begin, so on a
  thread-pool completion port the title's routine can run before the
  title's `*Async` call has returned; logged, see "Diagnose"), `pending_with` (result computed on our thread, e.g.
  dialogs), `result::<T>` to fetch, `bytes(value)`.
- **Our own threads**: dstorage workers (one per logical CPU, named
  `dstorage N`), XMA decoder (`victory XMA decoder`, parks 2 ms), gameinput
  poll thread, lifecycle suspend + deadline threads, xgameui dialog thread,
  image watch, xmem mapping helpers for `map_aliases` (`side_by_side()`:
`VICTORY_MAP_THREADS`, default 8). These are std
  threads, so they also receive title TLS/`DllMain` thread notices via the
  loader (see loader doc, `OwnTls`).
- `kernel::SetThreadPriority` lifts the title's `IDLE..=LOWEST` threads to
  `BELOW_NORMAL`; `runtime::use_console_timers` sets 1 ms timer resolution,
  opts out of power throttling, and raises the process to
  `ABOVE_NORMAL_PRIORITY_CLASS`.

## File path rewriting (`files.rs`)

```text
DRIVES: Vec<(letter, host root UTF-16)>
mount(root)        -> highest free letter D..Z not used by host or another mount
mount_at('G', exe folder), mount_at('T', <data>/<TITLEID>/temp)   (run.rs:100-102)
storage area       -> mount(<data>/<TITLEID>/storage), path "X:\"
translate(path):  "X:\..", "X:/..", "X:", "X:name", "\\?\X:\.."
   letter mounted? -> root + tail (adds '\' for "X:" / "X:name")
   root+tail >= 248 chars -> "\\?\" form, '/'->'\', '.' and '..' resolved here
                             ('..' stops at root), UNC roots -> "\\?\UNC\"
   not mounted -> None: path reaches the host unchanged
narrow (A) paths: converted with CP_ACP and sent to the host's *wide* twin
```

Rules that matter: a rewritten narrow path never goes through our own wide
wrapper (it would be rewritten twice if its letter is mounted too); fixed
letters (`G:`, `T:`) hide a host drive of the same letter from the title;
`mount` never hides a host drive. The original host functions are reached
through `host!("kernel32.dll", "CreateFileW": fn-type)` (`files.rs:26`), a
`OnceLock`-cached `host::function` lookup that panics if the host lacks it.
`dstorage::open_file` and `media`'s resolver go through `files` too.
`ReadFile`, `fread` and `_read` are bound here for a different reason: they
open guarded title memory first (`victory_gpu::kernel_writing`), because
kernel writes into a read-only-guarded page fail (`ERROR_NOACCESS` /
`ERROR_INVALID_USER_BUFFER`) instead of faulting into the guard handler.

### Kernel reads into title memory

Every way the host kernel writes title memory, and what guards it
(2026-11-26 review of the title's and its DLLs' imports):

| Path | How | Safe? |
|------|-----|-------|
| `ReadFile` (title, Bink, libHttpClient) | ours: destination opened for the call; a read refused for its buffer (`ERROR_NOACCESS`, `ERROR_INVALID_USER_BUFFER`) on a disk file is tried again up to 8 times (about 0.25 s), from its OVERLAPPED offset or with the file pointer put back; a read left pending (`ERROR_IO_PENDING`) keeps its destination open for 2 s more (`KernelWrite::keep_for`), so no refresh takes it for clean before the bytes land | yes |
| `fread` | ours: destination opened for the call (the CRT reads large requests straight into it); failures warned, not retried (the stream position is the CRT's) | yes |
| `_read` (libHttpClient) | ours, as `fread` | yes |
| stream refills (`fgetc`, small `fread`, `std::basic_filebuf` through `_get_stream_buffer_pointers`) | into the CRT's own buffer; `setvbuf` with a title buffer is given none, so the CRT makes its own | yes |
| `std::basic_istream::read` (host msvcp140) | calls the title's own `basic_filebuf::xsgetn`, which calls the title's `fread` import (ours) | yes |
| kernelbase out parameters (`GetFileSizeEx`, `GetFileAttributesExW`, `FindFirstFileW`, `QueryPerformance*`, the read count of `ReadFile`) | the kernel writes a local of kernelbase, copied out in user mode, where a guard faults into the handler | yes |
| an overlapped read's OVERLAPPED status words | written by the kernel at completion; the title's OVERLAPPED is not a GPU buffer | not guarded, not seen to matter |
| DirectStorage, memory destination | `kernel_writing` for the request, failed reads retried 8 times, then `kernel_will_write` | yes |
| DirectStorage, page destination | our own section view (`write_physical`/`page_window`), then `pages_written` | yes (user mode) |
| XGameSave reads (`read_blobs`, `read_result`), xasync results | copied in user mode | yes |

Said aloud for the next run (`reads.rs`): every failed or unexplained short
read is a warning, rate-limited (first 16, then every power of two):
`ReadFile: a read of the title's failed`, `ReadFile: the host refused the
title's buffer; opened again and tried again`, `ReadFile: fewer bytes than
asked, short of the file's end`, `fread: a read of the title's failed`,
`_read: a read of the title's failed`, with the host path, destination,
sizes, error and `guarded="R of B blocks read-only by the GPU side's guard,
A also shown and guarded at another view"`. For the first minute,
`title file: first read` once per file (path, api, size, first request),
then `title files read in the first minute` and one `title file: read in the
first minute` per file (reads, bytes asked and got, failures), sorted by
path, for diffing a good and a bad launch.

## Save data and title storage

`Title::from_manifest` (`runtime.rs:103`) reads `Game/TitleId` (hex),
`Game/StoreId`, `Game/PersistentLocalStorage/SizeMB` from
`MicrosoftGame.config` beside the exe (small hand-written XML walker:
comments, CDATA, quoted `>` handled). Missing manifest → id 0, no quota.

| Data | Host location (`<data>` = `VICTORY_DATA_DIR`, default `%LOCALAPPDATA%\victory\data`) |
|------|------|
| Persistent local storage | `<data>/<TITLEID:08X>/storage` (virtual drive) |
| Game saves | `<data>/<TITLEID>/saves/<XUID:016X>/<config or "default">/<container>~<hash16>/v<N>/` (`Title::saves`, `xgamesave.rs:406`) |
| Temp (`T:`) | `<data>/<TITLEID>/temp` |
| Crash log | `<data>/<TITLEID>/crashes.log` |

XGameSave updates are all-or-nothing: write `v<N>.new/` (unchanged blobs
hard-linked), flush, rename to `v<N>`; highest committed version wins;
leftovers removed on sight; delete renames away first. Container and blob
names are stored in a `manifest` file, never used as file names directly (blob
`i` is file `i`); folder = sanitised name + hash of the exact name, so
case-only differences get distinct folders. Crash safety is tested by killing
a child at each step (`VICTORY_SAVE_CHILD/OP/STOP`).

## Memory (`xmem.rs`)

Unified memory on the console; here CPU memory, which `victory-gpu` mirrors
to the GPU. Memory types and bandwidth classes are ignored; address
constraints are honoured.

| Console call | Host implementation |
|--------------|---------------------|
| `XMemVirtualAlloc(base, size, kind, flags, protect)` | `VirtualAlloc2` with `MEM_ADDRESS_REQUIREMENTS` from `address_range(flags)` (`xmem.rs:78`): 32-bit flag → below 4 GiB; else from 4 GiB up to 2^(47-field) or 2^40 with no field (console keeps title memory, and ray-tracing structures, below 2^40). 2 MiB page kind → 2 MiB alignment. Console protection bits above 0x1ff (GPU, uncached, write-combined) dropped. Reservations at our chosen address get `MEM_WRITE_WATCH` (GPU side copies only written pages). A non-committing 64 KiB-page reservation becomes a **placeholder** (`MEM_RESERVE_PLACEHOLDER`) so physical pages can be mapped into it later. Success → `victory_gpu::title_memory_allocated`. |
| `XMemVirtualQuery` | `VirtualQueryEx`, then flags = GRAPHICS for any committed page, tag 0 (`shortcut:`) |
| `XMemAlloc/XMemFree` | the title's `__XMemAllocationHooks__` export (two fn pointers, looked up once on the main image) if present, else `...Default` |
| `XMemAllocDefault/FreeDefault` | 32-bit attribute → `XMemVirtualAlloc` low memory, released with `VirtualFree`; else Rust global allocator with a 16-byte header (size, align) below the pointer; alignment from attribute bits 24..28. Host heap lies above 2^40 (`shortcut:`). |
| `XMemAllocatePhysicalPages` | one pagefile-backed section per call (`SEC_RESERVE`, committed lazily; `SEC_COMMIT` under Wine), pages are 64 KiB slices, numbered in a global table |
| `XMemMapPhysicalPages(addr, n, pages)` | unmap whatever is there, then one view per run of consecutive slices of a section, replacing placeholders; null pages = unmap. Wrapped in `victory_gpu::title_memory_remapping()` so in-place GPU reads never see the gap. Tracks `Physical.views` (addr → slice) and `Shown` (slice → addresses) for aliasing. |
| `XMemMapPhysicalPagesScatter` | one page per address; page 0 or null = unmap |
| GPU page-mapping copies | `map_alias`/`map_aliases` (installed by `d3d12x::create_device` via `victory_gpu::resource::use_page_mapper`) map the pages shown at `from` also at `to`, optionally from several threads |
| `VirtualFree` (kernel32 import) | `title_virtual_free`: tells `victory_gpu::title_memory_freed` first (unless only splitting/joining placeholders) |
| `XMemTransferMemory` | title→system transfers counted against the console model's standard memory; tools memory refused |
| `XMemGetWorkingSetStatistics` | reports title limit/use for the chosen console |

dstorage writes into physical pages through `write_physical`/`page_window`
(a kept whole-section view, `Section::window`) and then calls
`pages_written`, which notifies the GPU side for every address showing those
pages. Commit failures log through `commit_refused` and, at the commit limit,
`victory_loader::log_crash` with a paging-file hint.

## DirectStorage (`dstorage.rs`)

```text
DStorageGetFactory(FACTORY iid) -> static factory object
factory: OpenFile (via files::CreateFileW) -> Instance<File{Files(Io)}>
         CreateQueue(desc) -> Instance<Queue{id}>, CreateStatusArray,
         SetStagingBufferSize (floor VICTORY_DSTORAGE_STAGING_MB, default 256 MiB)
queue:   EnqueueRequest -> Command::Read buffered;  EnqueueStatus;  Submit -> lanes
Scheduler (one global): workers = available_parallelism, named "dstorage N"
   queues served by weighted round robin over priority (cost 100/10/1 per byte),
   started in queue order, compressed bytes in flight <= staging buffer
execute(request): read source (file at offset or memory) -> zlib inflate (zlib-rs,
   thread-local) and/or BCPack (victory_formats::bcpack, built-in tables)
   -> destination: memory address (opened with kernel_writing) or physical
   pages (xmem::write_physical, retried VICTORY_DSTORAGE_RETRIES times)
status entry: written once every earlier request on its queue finished,
   carrying the first failure since the previous status entry
```

Requests start at `Submit`, not at enqueue, because the title fills in page
lists in its own submit. Errors map to HRESULTs (`END_OF_FILE`,
`INVALID_DATA`, `E_NOTIMPL` when BCPack tables fail to load).

## Input (`gameinput.rs`)

`GameInputCreate` (ordinal 1) returns the hub's object; a `Hub` started once
(`HUB`, with a `Source` trait so tests can feed fake devices) polls XInput
slots 0-3 and the keyboard, creates a new reading per change, and runs
device callbacks on connect/disconnect. Default (`VICTORY_ONE_PAD` unset): one
always-present pad merged from the keyboard stand-in (`FALLBACK_BUTTONS`,
layout in README-fxceless) and every host pad. Rumble via `XInputSetState`.
No input device is associated with the user (`xuser`).

## Audio path

```text
title / fmod ---> AcpHal.dll (audio.rs)
   ApuCreateHeap/ApuAlloc: host memory with 32-bit device addresses
   AcpHalAllocateShapeContexts: context arrays in that heap
   coprocessor command/message queues: every command completes and is acked at once
   ENABLE_XMA(S) -> "victory XMA decoder" thread decodes enabled contexts
                    with victory_formats::xma into their output rings
   SHAPE SRC/EQ/filter/mix: not emulated -> silent voices
   mixed output -> host WASAPI (title reaches it through COM, xaudio2_9 passes through)
xapu.dll (apu.rs): Opus packets -> float PCM with opus_pure at enqueue
chad.dll (dsp.rs): reverb streams -> silence
```

## Hand-off to victory-gpu

`D3D12XboxCreateDevice` (`d3d12x.rs:14`) first installs
`XMemAlloc/XMemFree` as resource allocator and `xmem::map_alias(es)` as page
mappers, registers `host_gpu_failed` (crash log + `TerminateProcess(0x887A0005)`),
then calls `victory_gpu::device::create`. Everything returned (device, queues,
command lists, heaps) is a `victory-gpu` object. Shader translations go to
`<cache>/shaders`, compiled pipelines to `<cache>/pipelines`
(`use_shader_cache`). Memory events (`title_memory_*`, `kernel_writing`,
`kernel_will_write`) flow from xmem/files/dstorage to victory-gpu.

## Environment switches read in this crate

On/off switches use `is_none_or(|v| v != "0")` (on unless `0`) or
`is_some_and(|v| v == "1")` (off unless `1`), as listed.

| Name | Values / default | Effect | Where |
|------|------------------|--------|-------|
| `VICTORY_IMAGE_WATCH` | `name=<hexoff>[:1\|2\|4\|8],...` / off | log those image words once a second when they change | `image_watch.rs:51` |
| `VICTORY_EFFICIENCY_MODE` | `1` / off | `1`: let Windows throttle execution speed (efficiency mode) | `runtime.rs:251` |
| `VICTORY_PROCESS_PRIORITY` | `normal` or `0` / above-normal | keep the start priority class | `runtime.rs:284` |
| `VICTORY_TITLE_PRIORITIES` | `0` / remap | `0`: title thread priorities as set (no idle→below-normal lift) | `modules/kernel.rs:431` |
| `VICTORY_WINDOW_MAX` | `WxH` / unset | bound the title's top-level window (keeps aspect) | `modules/lifecycle.rs:182` |
| `VICTORY_RELAUNCHED_FROM` | inherited handle (internal) | relaunched process waits up to 30 s for the old one | `modules/lifecycle.rs:325`, `:363` |
| `VICTORY_ONE_PAD` | `0` / on | `0`: each host pad its own device; stand-in only with no pad | `modules/gameinput.rs:163` |
| `VICTORY_KEYS` | `<s>[-<s>]:<key>,...` / off (diagnostics) | scripted key presses after input starts | `modules/gameinput.rs:1251` |
| `VICTORY_SECTION_WINDOW` | `0` / on | `0`: map a view per physical-page write instead of one kept section view | `modules/xmem.rs:412` |
| `VICTORY_STREAM_NOTICE` | `0` / on | `0`: don't notify the GPU side of page writes (then `VICTORY_PAGE_WRITES`, read in victory-gpu, decides) | `modules/xmem.rs:601` |
| `VICTORY_MAP_LEAN` | `0` / on | `0`: old per-page mapping call sequence | `modules/xmem.rs:1052` |
| `VICTORY_STREAM_PRIORITY` | `0` / on | `0`: don't lift a below-normal streaming thread when it maps/submits | `modules/xmem.rs:1066` |
| `VICTORY_MAP_THREADS` | `auto` / `N` (1-32) / unset=8 | threads for `map_aliases`; `auto` = 16 when busy else 1; diagnostics build also accepts on/off toggling via `victory_gpu::switched` | `modules/xmem.rs:1305`, `:1322` |
| `VICTORY_DSTORAGE_STAGING_MB` | MiB / 256 | floor for the title's staging buffer size | `modules/dstorage.rs:236` |
| `VICTORY_DSTORAGE_IO` | `one`/`handles`/`overlapped`/`direct` / `handles` | how files are read (one handle, handle per worker, overlapped, unbuffered) | `modules/dstorage.rs:301` |
| `VICTORY_WALLS_TRACE` | `0`/`1`/`2` / 0 | log failed (1) or all (2) read requests with files | `modules/dstorage.rs:659` |
| `VICTORY_DSTORAGE_LOG` | file path / off | tab-separated stream log, flushed each second, with xmem map costs | `modules/dstorage.rs:715`, `:735` |
| `VICTORY_DSTORAGE_TRACE` | `0`/`1`/`2` / 0 | batch / suspicious-request (1) or every request (2) trace | `modules/dstorage.rs:1057` |
| `VICTORY_DSTORAGE_HALF` | `1` / off | restore old behaviour of starting requests before submit (known-bad, for comparison) | `modules/dstorage.rs:1305` |
| `VICTORY_DSTORAGE_RETRIES` | count / 12 (~1.5 s) | retries of a failed page-destination write | `modules/dstorage.rs:1667` |
| `VICTORY_XMA_DUMP` | dir / off (diagnostics) | save first 32 XMA voices' contexts and input | `modules/audio.rs:667` |
| `VICTORY_AUDIO_CAPTURE` | `.wav` path / off (diagnostics) | record the process's audio in a child recorder | `modules/audio/capture.rs:69`, `:85` |
| `VICTORY_AUDIO_CAPTURE_OF` | pid (internal) | marks the recorder child | `modules/audio/capture.rs:49` |
| `VICTORY_BENCH_FILE` | path, ignored bench test only | | `modules/dstorage.rs:2137` |
| `VICTORY_SAVE_CHILD/OP/STOP`, `VICTORY_HOST_LOCK_CHILD` | test-only child markers | | `xgamesave.rs:998-1080`, `host.rs:172` |

## Invariants

- Registry is built once (`LazyLock`); module order in `modules::all` is
  lookup priority. HLE always beats the host.
- `find_module` uses `HostPolicy::default()`, not the run's policy, so with
  `--strict` a by-name `LoadLibrary` of an allow-listed DLL still reaches the
  host (by design, comment at `kernel.rs:327`).
- Host DLLs load from System32 only (`LOAD_LIBRARY_SEARCH_SYSTEM32`;
  `tests/no_dll_planting.rs`); never under the module-cache lock.
- Title images stay loaded: `FreeLibrary` on them is a no-op.
- In `XMemMapPhysicalPages` the page list is copied **before** any lock (a
  fault on title memory may wait for the mapping lock). Lock order:
  `PHYSICAL` before `SHOWN`.
- Area objects are `static`; never free them; `Release` returns 1.
- Paths given to the host are NUL-terminated `Vec<u16>` owned until the call
  returns (`Wide` keeps the buffer alive).

## Unsafe hot spots

`xmem.rs` (VirtualAlloc2 placeholders, section views, page tables shared with
victory-gpu), `files.rs` (`host!` transmutes to declared signatures; a wrong
signature is UB), `kernel.rs` (transmuting the title's filter, argv leaked
pointers), `dstorage.rs` (`Instance<T>` casts, writes to title addresses from
workers), `xasync.rs` (raw `Arc` handles, provider callbacks into title code),
`audio.rs` (decoder thread writes into title rings), `gameinput.rs` (reading
refcounts).

## Gotchas and stale bits

- `modules/mod.rs:7-9` "Planned areas: `file`, `thread`, `xaudio2`, `xstore`
  stubs" is stale: `files` and `xstore` exist, thread notices live in the
  loader, XAudio2 passes through to the host.
- `xmem.rs` comments cite "Decision 0001" / "decision 0007"; no decisions
  directory exists in `docs/`.
- `missing::<SLOT>` returns an HRESULT even for slots whose real return type
  is `bool` or a value.
- `xasync::Pool` grows without bound.
- Many comments carry measurements and `shortcut:` markers specific to one
  title; read them as rationale, not specification.

## How to

**Implement a missing import** (`dll!Func` from a trap or `victory census --missing`):
1. Find or create the module serving `dll` (patterns in each `module()`).
   Shared Windows DLLs: add only if console behaviour differs; otherwise
   add the DLL to `HostPolicy::default()`.
2. Write `pub extern "system" fn Func(..) -> ..` with the exact name and
   Win64 signature; null-check pointers; return documented HRESULTs; log
   first use with `tracing`.
3. Add `hle_export!(Func)` (or `HleExport { name: "#n" | "Name", address }`).
4. To call the host's original from an override: `files::host!("dll", "Name": extern "system" fn(..) -> ..)`.
5. Test: `HleResolver::new(HostPolicy::none()).classify(dll, &name)` is
   `Some(Provenance::Hle)` (see `lib.rs` tests).

**Add a runtime area** (QueryApiImpl logs `runtime API area not implemented provider=.. iid=..`):
create `modules/<area>.rs` with `PROVIDER`/`IID` GUIDs, a
`static VTABLE: Methods<N>` (slots 0-2 = `missing::<0>`, `add_ref`, `add_ref`;
methods from 0x18), `pub static OBJECT`, `pub const INTERFACES`, add it to
`modules/mod.rs` and to `AREAS` in `xgameruntime.rs`. Async methods complete
through `xasync::finished_with`/`pending_with`. A logged
`runtime method not implemented area=X slot=0xNN` names the slot to fill.

**Add a new console DLL**: new `modules/<x>.rs` with `module()`, push it in
`modules::all()` (behind `cfg(windows)` if it uses Win32). Make sure the DLL
is not in `HostPolicy::default()` or pseudo-handle loading won't happen. The
loader then never looks for it in the title folder (`run.rs:286`).

**Pass a host DLL through**: add its lower-case name or `prefix*` to
`HostPolicy::default()` (`host.rs:17`). Check it cannot open a network path
(`ws2_32`/`winhttp` are guarded by `net`; `rpcrt4` deliberately is not
passed). Overriding single functions of it still works (HLE wins).

**Diagnose**: `VICTORY_LOG=victory_hle=debug,info`; `title` target logs the
title's `OutputDebugString`. At info: the title's file reads of the first
minute and every failed one (`reads.rs`, above), `game save: provider
opened`, `game save: containers reported`, `game save: blob read` (or a
warning when a read fails or its buffer is too small), `dstorage: a request
completed with less data than its destination holds` (warning), and
`async: a completion routine runs before the call that began it returned`
(`xasync::early_completion`: our areas complete in their begin; the line
names the area's call site and whether it ran inline). Unknown DLL names refused by `LoadLibrary` log
`a host DLL outside the host policy: refused`; pseudo-module misses log
`GetProcAddress: not implemented`.

## Tests

`cargo test -p victory-hle` (most need Windows; some spawn child processes or
touch the real filesystem via `tempfile`). Approximate unit test counts:
`dstorage` 19 (one `#[ignore]` benchmark needing `VICTORY_BENCH_FILE`),
`xmem` 17 (one ignored benchmark), `xasync` 13, `xgamesave` 11 (crash at every
step in child processes), `lifecycle` 10, `audio` 8, `gameinput` 8 (fake
`Source`), `files` 11, `kernel` 6, `reads` 3, `lib.rs` 5, `media` 5, `rpcrt4` 5, `xg` 4,
`xsystem` 4, others 1-3. Integration: `tests/no_dll_planting.rs`. The
`tests/fixtures/hle-probe` stand-in title is used by **victory-loader**'s
`tests/load_probe.rs` with a fake resolver, not by this crate's tests. No game
data is needed.

# victory-gpu: the command and draw path

`crates/victory-gpu` turns the title's D3D12.X calls into host D3D12 work. This
page covers the **command/draw half**: the object model, lists, queues,
submission and its threads, pipelines, descriptors, draws, indirect execution
and present. The **memory/resource half** (host copies of title memory, write
watching, resources and textures, uploads, residency, the relay's landing,
frame record/replay) is in [victory-gpu-memory.md](victory-gpu-memory.md).
Where the two halves meet, this page links there and does not go into depth.

Everything here was checked against the source as of commit `f4b48ad`.
Anything marked *(inferred)* was not proven from the code. References are
`path:line` relative to `crates/victory-gpu/src/`, or symbol names.

---

## 1. Purpose and where it sits

```
title code (native x86-64)
  │  d3d12_xs!D3D12XboxCreateDevice etc.   (victory-hle/src/modules/d3d12x.rs)
  ▼
victory-gpu  ── console-layout COM objects (device, lists, queues, heaps, ...)
  │             recorded onto host ID3D12GraphicsCommandLists
  │             shaders translated by victory-shader at first draw
  ▼
host D3D12 + DXGI swap chain in a window of ours (or the title's)
```

On the console, CPU and GPU share memory, and a GPU address is a CPU address.
Here they do not share memory, so the crate:

- answers every console graphics call with an object that has the console's
  memory layout;
- records commands onto host lists;
- keeps host copies of the title memory that shaders read, and brings what
  shaders wrote back (the memory half);
- sends the work to one host direct queue;
- presents by copying the title's back buffer into a DXGI swap chain.

**Windows only for real work.** Most modules are `#[cfg(windows)]` (`lib.rs:18-95`).
On other targets the object layer compiles, and every draw-state method is an
alias of `ignored_state` (`list.rs:1836-1862`).

### Cargo

| | |
|---|---|
| Feature `diagnostics` | `Cargo.toml:27`. Turns on frame capture, the host debug layer, DRED, the timeline, `VICTORY_AB` A/B switching, the frame recorder and replay (`replay`), copy statistics and the fault book. It is enabled by `victory-hle/diagnostics`, and that by the app's `diagnostics` feature. Release builds without it compile out every `#[cfg(feature = "diagnostics")]` block. |
| Dependencies | `blake3`, `zlib-rs`, `tracing`, `anyhow`, `victory-formats`, `victory-pipeline` (content-addressed cache), `victory-shader` (translator). On Windows, `windows` and `windows-sys`. Dev: `tempfile`. |
| `build.rs` | Hashes `victory-shader/src/**`, `victory-shader/Cargo.toml`, `shader_cache.rs`, `Cargo.lock` and `rust-toolchain.toml` into `env!("VICTORY_TRANSLATOR")`. Line endings are ignored. The hash keys the shader cache (`shader_cache.rs:57`), so a translator change invalidates cached translations. A rebuild of anything else does not. |

### Public entry points used by other crates

| Item | Used by | What |
|---|---|---|
| `device::create`, `device::configure_virtual_memory`, `device::IID` | `victory-hle/src/modules/d3d12x.rs` (`D3D12XboxCreateDevice`, `D3D12ConfigureVirtualMemoryX`, data import `IID_ID3D12Device`) | The device, a single static (`device.rs:115`) |
| `root::serialize`, `serialize_versioned`, `create_deserializer` | same | Root-signature (de)serialisation. Forwards to the host's `d3d12.dll` after stripping console flags (bits 24+, `root.rs:28`) |
| `object::{Object, HRESULT, Iid}` | d3d12x | Types for the exports |
| `resource::use_allocator`, `use_page_mapper`, `use_many_page_mapper` | d3d12x `create_device` | Hooks into `xmem` (memory half) |
| `on_failure(fn(&str) -> !)` | d3d12x | What a fatal host GPU failure does. Default: `abort`. The HLE layer logs a crash and calls `TerminateProcess` (`host.rs:788-822`) |
| `shader_cache::{use_shader_cache, precompile}`, `pipeline::{use_pipeline_cache, save_pipeline_cache}`, `pipeline::known` | d3d12x, `lifecycle.rs`, re-exported by `victory-hle` as `known_pipelines` | Shader cache, pipeline library, `victory warm` |
| `console::{Console, set, current}` | `xsystem.rs`, `xmem.rs` | Series X/S choice |
| `title_memory_*`, `kernel_writing`, `kernel_will_write` | `xmem.rs`, `dstorage.rs`, `files.rs` | Memory half (`watch`, `inplace`) |
| `timeline::{span, import}`, `titlewrite::import` | `victory-hle/src/host.rs` | Import wrappers |
| `switched`, `frame_phase`, `replay` (diagnostics) | `victory-hle` | A/B switches, profiler phase, recorder |

---

## 2. Vocabulary (this fork's plain-English terms)

| Term | Meaning in the code |
|---|---|
| **title list / host list** | `list::List` is the title-visible object with the console layout. `host::HostList` (`host.rs:1155`) is an `ID3D12CommandAllocator` + `ID3D12GraphicsCommandList` plus our bookkeeping. |
| **step** | `host::Step` (`host.rs:2218`): CPU work deferred to submission time, in recorded order. Kinds: `Copy`, `Fill`, `Refresh`, `Run`, `Host`, `GpuWrite`, `Texture`, `Plane`, `Placed`. Kept in `HostList::deferred` and run by `HostList::run_deferred`. |
| **refresh** | Bringing a range of a buffer's host copy up to date from title memory (memory half). |
| **prelude** | A small host list run just before a host list. It holds the texture state transitions and twin copies that only become known at submission, because lists are recorded on many threads (`HostList::prelude`). Made lazily (`VICTORY_LAZY_PRELUDE`). |
| **cut / part / gate** | A title list that records a *wait for a memory value* is cut there. The host list so far is closed into a `Part`, and recording goes on in a fresh host list (`list::cut`, `list.rs:315`). `gate` holds the waits that stand before the current host list. |
| **flight** | A submission sent to the card and not waited for (`host::Flight`, `Late::Run`). The flights in the air form the `LINE` (`host.rs:2697`). |
| **late / settle** | `host::late()` (`host.rs:2373`): submissions become flights. *Settling* (`Host::settle`) waits for the flights, copies back what their shaders wrote, then does their bottom-of-pipe writes and fence signals in order. |
| **relay** | A settle done continuously by a thread of its own, `victory-relay` (`relay_worker`, `host.rs:3408`). The submitting thread only waits on it (`relay_wait`). **On by default** (`shadow::relay`, `shadow.rs:1100`). Details are in the memory doc. |
| **land / landing** | The relay putting what a flight's shaders wrote into title memory (`land.rs`, memory doc). In `host.rs`, `land()` (`host.rs:3248`) only recycles the host lists of landed flights. |
| **the line (of submissions)** | `queue::behind::LANE`: with `VICTORY_ASYNC_SUBMIT`, the title's submits, signals, waits and presents are queued here in call order. One thread, `victory-submit`, serves the queue. Not the same as `host::LINE`, which holds the flights. |
| **thin** | `VICTORY_THIN=1`: list methods only write their arguments into a word stream. Threads of ours later replay the stream through the real methods (`list/thin.rs`). |
| **join parts** | `VICTORY_JOIN_PARTS=N`: the submit thread prepares up to N consecutive parts/submissions of one queue as one host submission (`Work::take_along`, `queue/behind.rs:465`; `host::joins`, `host.rs:3088`). |
| **card steps** | `VICTORY_CARD_STEPS=1|2`: steps that would read title memory a flight may still write run on the GPU, in a list of ours queued ahead of the submission, instead of waiting for the flight (`host/card_steps.rs:77`). |
| **in place** | `VICTORY_COPIES_IN_PLACE` (on): the title's buffer copies are done on the card at their place in the list, not on the CPU at submission (`host/place.rs`). |
| **aside** | Work done by a thread of ours (`victory-aside`) while the title records, e.g. the heap buffer-view pass (`host/aside.rs`). |
| **retire / bury** | Host objects the title released while a submission may still use them are kept in `RETIRED` until enough submissions have finished (`host::retire`, `bury`, `host.rs:462-523`). |
| **dynamic slots** | Host descriptor slots past the title's heap size. They are used for tables made fresh per draw (`descriptor::host::DYNAMIC` = 2^18, `descriptor/host.rs:1483`). |
| **twin** | A colour copy of a depth texture that shaders also write, or a raw-view duplicate of a slot (`HostList::twin_use`, `descriptor/host.rs` `raw_twins`). |

---

## 3. Module map (every file)

"Half" C = command/draw (this page), M = memory ([victory-gpu-memory.md](victory-gpu-memory.md)),
D = diagnostics only (`cfg(feature="diagnostics")`).

| File | Lines | Half | Role |
|---|---:|---|---|
| `lib.rs` | 115 | C | Module list, re-exports, `switched`/`frame_phase` |
| `object.rs` | 517 | C | COM-like object header, tables, `missing_table`, `hand_out`/`release_boxed`, private data |
| `device.rs` | 567 | C | The static device, its 160-slot table, feature queries |
| `dxgi.rs` | 336 | C | Static DXGI device/adapter/factory/output; `VICTORY_DISPLAY_MODE` |
| `console.rs` | 42 | C | Series X/S choice |
| `fence.rs` | 261 | C | Fences: a value and waiting events; `fence::set` moves one |
| `frame.rs` | 247 | C | Frame pacing clock (`SetFrameIntervalX`, waits, tokens) |
| `queue.rs` | 1295 | C | Command queues: execute, signal/wait, held work, PresentX, page mappings |
| `queue/behind.rs` | 1020 | C | `VICTORY_ASYNC_SUBMIT` line (`Lane<Job>`) and the `victory-submit` thread |
| `list.rs` | 2352 | C | Allocators, lists, the 160-slot list table, cuts at waits, detach/recycle, copies, barriers, queries, waits/writes |
| `list/thin.rs` | 1094 | C | `VICTORY_THIN`: word streams and the `victory-list-N` translator threads |
| `list/clear.rs`, `list/clear/tests.rs` | 483+403 | C/M | `ClearUnorderedAccessViewUint/Float` (slots 0x1a8/0x1b0): host clears of textures, depth twins and buffer host copies, the cleared range declared written (§9) |
| `draw.rs` | 5061 | C | `draw::State`: per-list draw state, `bind`, draws/dispatches/mesh, tables, indirect windows/batches; ops listings (D): textures with depth and levels, a view's title record, words of constants (`VICTORY_CAPTURE_CONSTANTS`) |
| `primitive.rs` | 349 | C | Rect lists expanded to triangles on the CPU |
| `indirect.rs` | 3818 | C | GPU clamp of `ExecuteIndirect` records, budgets |
| `indirect/batch.rs` | 287 | C | Clamps of many executions in one pass |
| `indirect/bench.rs` | 491 | C | Ignored benchmark of clamping strategies |
| `signature.rs` | 282 | C | Command signatures (record layout) |
| `root.rs` | 547 | C | Root-signature serialisation and title root-signature objects |
| `query.rs` | 148 | C | Query heaps (description only) |
| `sampler.rs` | 211 | C | Console sampler descriptions to host samplers |
| `descriptor.rs` | 1721 | C | Descriptor heaps, handles = addresses, view table (`VIEWS`), heap sync |
| `descriptor/host.rs` | 5628 | C/M | Host shader-visible heaps mirroring title heaps, dynamic slots, texture slot queues, `show_after` |
| `pipeline.rs` | 919 | C | Pipeline objects, shader containers by hash, stream parsing, BVH-builder gating; the "shader translated" line names a fingerprint of the DXIL and the static samplers' console words and host filters |
| `pipeline/host.rs` | 1597 | C | `HostPipeline::new`: our root signature and the host PSO from translated shaders; `VICTORY_ROOT_LOG`, `VICTORY_LINEAR_SAMPLERS` |
| `pipeline/make.rs` | 614 | C | Pipeline worker threads, per-frame wait budgets, priorities |
| `pipeline/library.rs` | 877 | C | `ID3D12PipelineLibrary` on disk, early/used/slow lists |
| `pipeline/known.rs` | 456 | C | `VICTORY_PIPELINE_BOOK`, `victory warm` |
| `shader_cache.rs` | 413 | C | Translated shaders on disk, keyed by translator hash + settings |
| `host.rs` | 10258 | C/M | Host device/queue/fence, `HostList`, `Host::submit`, late/relay line, present/swap chain, window, retire/sweep |
| `host/aside.rs` | 92 | C | Heap buffer-view pass on `victory-aside` |
| `host/awake.rs` | 235 | C | `VICTORY_CARD_AWAKE`: keeps GPU clocks up |
| `host/card_steps.rs` | 1278 | C/M | `VICTORY_CARD_STEPS` |
| `host/indirects.rs` | 134 | D | What indirect executions ran, for device-removed reports |
| `host/panel.rs` | 442 | D | On-screen numbers (`VICTORY_PANEL`, F3) |
| `host/place.rs` | 307 | C/M | Buffer copies done on the card at their place |
| `host/send.rs` | 306 | M | `VICTORY_SEND_OWN` sends |
| `host/spans.rs` | 214 | D | GPU timestamps around submissions |
| `host/stamps.rs` | 171 | D | Timestamps for the timeline |
| `host/torn.rs` | 301 | M | Torn fetch checks |
| `host/traffic.rs` | 67 | D | Bus traffic per host copy |
| `host/upcheck.rs` | 1234 | M/D | Checks of sends to card copies |
| `compose.rs` | 315 | C | Two-plane present compositor (Series S) |
| `ambient.rs` | 499 | C | Stand-in for ray-traced indirect light (`VICTORY_AMBIENT*`) |
| `cores.rs` | 98 | C | Fast-core affinity for our threads (`VICTORY_FAST_CORES`) |
| `tdr.rs` | 129 | C | Logs Windows TDR registry settings |
| `switch.rs` | 63 | C | `switch::on` (set and not `0`) and the `toggled!` macro |
| `ab.rs` | 118 | D | `VICTORY_AB` switches that flip while running |
| `capture.rs` | 1299 | D | `VICTORY_CAPTURE` PNGs, op listings, target dumps (3D textures as slice strips, 1D float tables as numbers; float targets' NaN, +Inf, -Inf and huge texels counted with their boxes, and the first target in dump order that has any) |
| `copystat.rs` | 1348 | D | Copy/phase statistics, `VICTORY_COPY_MODE`, `VICTORY_MEMORY_MAP` |
| `popwatch.rs` | 422 | D | `VICTORY_POP_WATCH` |
| `faultbook.rs` | 105 | D | `VICTORY_FAULT_BOOK` |
| `book.rs` | 699 | D | Who owns which card memory (`VICTORY_BOOK`) |
| `timeline.rs` + `timeline/{hooks,record,report}.rs` | 51+304+621+914 | C/D | `span()` (no-op without diagnostics), Chrome trace (`VICTORY_TIMELINE`) |
| `shadow.rs` | 5981 | M | Host copies of title buffers |
| `watch.rs` | 4604 | M | Write watching / guards |
| `writers.rs` | 2215 | M | The writers watch: which code writes a range of title memory, and what (`VICTORY_WRITERS*`, read in diagnostics builds; [memory doc §3.4a](victory-gpu-memory.md)) |
| `resource.rs` | 4390 | M | Heaps, resources, host textures (`HostTexture`, `host_transition`) |
| `texture.rs`, `texture/ahead.rs` | 2205+445 | M | Texture detiling and uploads, copy queue |
| `tiles.rs` | 1516 | M | Levels on demand |
| `texback.rs` | 249 | M | Textures read back to title memory |
| `upload.rs` | 162 | M | `allocate_upload` memory below 2^40 |
| `pool.rs` | 690 | M | GPU memory pools |
| `residency.rs` | 177 | M | Residency budgets |
| `memory.rs` | 1952 | M | Title memory by raw address for shaders (arenas, the directory, pages for faults with their limits: `FaultPages`) |
| `picked.rs` | 360 | M | `VICTORY_PICKED_WRITES` |
| `inplace.rs` | 1387 | M | Title memory opened as host heaps |
| `safecopy.rs` | 699 | M | Fault-tolerant copies |
| `titlewrite.rs`, `titlewrite/decode.rs` | 425+697 | M | Stores done in the title's place; `store_extent` gives the writers watch a store's size |
| `land.rs`, `land/tests.rs` | 655+457 | M | Landing threads |
| `replay/**` | ~12k | M/D | Frame record and replay (`docs/replay.md`) |
| `build.rs` | 42 | C | Translator fingerprint |
| `tests/replay.rs`, `tests/replay_big.rs` | 1806+387 | M/D | Replay integration tests |

---

## 4. How D3D12.X objects are represented

### Object header and tables (`object.rs`)

Every object starts with `object::Object` (`object.rs:43`), which is `repr(C)`:

```
+0x00 functions: *const *const ()   // method table
+0x08 bits: u32                     // device index + bookkeeping, always 0
+0x0c refs: AtomicU32
```

Tables are `Table<N>([*const (); N])` statics of `extern "system"` function
pointers. They are built in `const` context:

1. `missing_table::<KIND, N>()` (`object.rs:351`) fills every slot with a
   `missing::<KIND, SLOT>` stub. A stub logs `method not implemented`
   **once**, with the object kind, the slot's byte offset and the caller's
   `+offset` in its module (`object.rs:294`). It returns `E_NOTIMPL`. Slot 1
   is `add_ref` and slot 2 is `release`.
2. `d3d12_object(..)` fills the ID3D12Object slots: private data
   (`0x18`/`0x20`) and `SetName` (`0x30`, logs only).
3. Each module overwrites the slots it implements.

Tables are deliberately longer than the known interfaces (device and list:
160, queue: 36). A method that a newer title adds then logs instead of
jumping past the end of the table.

**Ownership.** Only the device (`device::DEVICE`) and the four DXGI objects
(`dxgi.rs:79-82`) are statics. Everything else is a `Box<T>` whose first
field is an `Object`. `hand_out` (`object.rs:116`) leaks the box to the title.
`release_boxed::<T>` (`object.rs:134`) frees it when the count goes 1→0, and
clears the object's private data first. `QueryInterface` is answered by
`object::answer`/`is` against each kind's IID list. A null `out` pointer
returns `S_FALSE` ("would it succeed").

### Console layouts

The title's inlined code reads fields at fixed offsets, so our structs are
`repr(C)` with compile-time `offset_of` asserts:

- **Device** (`device.rs:104`, 0x58 bytes). `out_of_line[0] = 1`, at byte
  0x18, makes the title call through the table instead of inlining
  (test `creation_hands_out_the_device`).
- **List** (`list.rs:101`). `put` at 0x18: the put pointer and four limits,
  **all zero**, so every inline "write a packet" check fails and every command
  becomes a table call (`list.rs:1-10`). Next come console scratch areas
  (`reserved`, root headers, `pipeline_shadow` ending at 0x560, and `newer`,
  1 KiB of padding because newer titles write past 0x560). Our fields follow
  them: `uploads`, `host: Option<HostList>`, `state: Mutex<draw::State>`,
  `earlier: Vec<Part>`, `gate: Vec<ValueWait>`, `thin: thin::Front`.
- **Queue** (`queue.rs:36`): `kind` at 0x10, the description, and `held: Mutex<Vec<Held>>`.
- **Pipeline** (`pipeline.rs:410`): a 32-byte `packet` at 0x10, always zero.

### Two layout generations ("newer")

`device::create` sets `NEWER` when the title creates the device as
`ID3D12Device11` (`device.rs:153`). Lists and queues are then given
`NEWER_*_FUNCTIONS`:

- Lists: slots ≥ 59 (`ExecuteIndirect` and later) move by +2
  (`list.rs:2335`). `NEWER_METHODS` adds absolute slots 142, 146
  (enhanced barriers) and 148 (`allocate_upload`).
- Queues: slots ≥ 14 (`Signal` and later) move by +2 (`queue.rs:1246`).

Both shifts were inferred from the title's call shapes, as the comments say.

### Handles and descriptors

- A descriptor handle is a plain address: CPU and GPU handles are the same
  (`descriptor.rs:1-7`). The increment is 32 bytes for views, 16 for samplers
  and 128 for RTV/DSV.
- A view slot holds a tagged record (`VIEW_TAG` "vVIE", `descriptor.rs:867`).
  The two words are either an index plus generation into the global `VIEWS`
  table (resource views, samplers), or an inline encoding
  (`KIND_SHIFT` bits: `CONSTANT`, constant-buffer address+size; `PLACED`,
  address + shape number). Inline encoding avoids an unbounded table
  (`VICTORY_VIEW_INLINE`, `descriptor.rs:895`).
- RTV/DSV slots hold an `RtvRecord` (`descriptor.rs:685`), tagged "vRTV" or
  "vDSV". It is turned into a host view from a 1024-entry ring
  (`RTV_RING`, `host.rs:37`) when a command uses it.
- Shader-visible view and sampler heaps get a `descriptor::host::HostHeap`
  lazily on first use (`Heap::host`, `descriptor.rs:83`; made late to save
  >100 MB of commit). It keeps the title heap's order: host slot `i` is made
  from title slot `i`, because translated shaders index heaps directly. Title
  slots are mirrored at `Create*View`/`Copy*` (`descriptor::mirror`). Raw byte
  copies into placed heaps are found by write-watch, in `sync_heaps`
  (`descriptor.rs:297`), on `victory-heap-sync` beside each submission.

### Adding a D3D12.X method: see recipe §11.1.

---

## 5. Key types, ownership and lifetime

| Type | Where | Owned by / lifetime |
|---|---|---|
| `host::Host` | `host.rs:39` | `static HOST: OnceLock<Option<Host>>` (`host.rs:824`), made by the first `host()` call. `None` if there is no D3D12 device: then nothing executes, and tests skip. Holds the device, **one direct queue**, our fence with `submitted: Mutex<u64>` (**the submission lock**), an event, RTV/DSV rings, `presenter: Mutex<Option<Presenter>>`, the adapter and fallback heaps. |
| `host::HostList` | `host.rs:1155` | Inside a title `List` (`List::host`). Taken from it at submission as `list::Detached` (with the list's uploads and `draw::State`), then reset and pooled in `list::IDLE` (≤512 lists / 256 MiB, `list.rs:241`) by `list::recycle` after it ran. |
| `HostList::deferred: Mutex<Deferred>` | `host.rs:2250` | The steps, upload buffers, kept host objects (`kept: Vec<IUnknown>`: pipelines, root signatures and heaps the list names, so the title may release them), bottom-of-pipe `writes`, read-`backs`. |
| `draw::State` | `draw.rs:407` | Per title list, behind a mutex. Holds what the title set (pipeline, heaps, roots, vertex/index views, targets, viewports...) and what the host list has (`host_pipeline`, `host_roots`, `host_heaps`), so redundant host calls are skipped. Owns the **dynamic descriptor slots** of its tables (`dynamic`), which are released in `Drop`/`tables_done`. `State::replay` re-applies direct state onto a fresh host list after a cut. |
| `pipeline::Pipeline` | `pipeline.rs:410` | Boxed title object. `host: Arc<make::Pending>`, so a worker can finish the host PSO after the title released the pipeline. |
| `make::Pending` | `pipeline/make.rs:33` | Shaders + `GraphicsState` + `made: OnceLock<Option<HostPipeline>>`. |
| `pipeline::Shader` | `pipeline.rs:59` | `Arc`, shared by full BLAKE3 of the container in `SHADERS`. Translation in `OnceLock`. Kept for the run. |
| `HostPipeline` | `pipeline/host.rs:379` | PSO + our root signature + per-stage binding map (`Stage`: user-data root parameter, root CBV/SRV/UAV parameter maps), vertex input slots, `rects`, `mesh`, `compute`. |
| `descriptor::Heap` | `descriptor.rs:49` | Boxed. Its memory is ours or the title's (placed heaps). `host: OnceLock<Option<HostHeap>>`. Registered in `SHADER_HEAPS` (RwLock) while alive. |
| `queue::Queue.held` | `queue.rs:43` | `Held::{Wait, Value, Run(Vec<Detached>, order), Signal}`, in order. Queues holding something are listed in `WAITING`. |
| `list::thin::Recording` | `list/thin.rs:181` | `Arc`, shared by the title list's `Front` and the line. Owns word chunks and the **back list** (a hidden title-layout `List` of ours, pooled in `BACKS`). |
| `host::Flight` / `Line` | `host.rs:2645`, `2667` | `static LINE: Mutex<Line>`: flights in the air, with their `Detached` lists, the ranges they may write and bottom-of-pipe writes. Landed flights' lists are recycled after `bury_lag` submissions (`land()`). |
| `Presenter` | `host.rs:8643` | Swap chain (2 buffers, FLIP_DISCARD), a copy list, `ahead` lists for presents in flight, the window, the frame meter and the compositor. |

---

## 6. Threading model

| Thread | Spawned at | Does |
|---|---|---|
| Title threads (≈5 recording, 1 render/submit) | title | Call list methods (record or, with thin, write words), `ExecuteCommandLists`, `Signal`/`Wait`, `PresentX`, `CreateGraphicsPipelineState` etc. |
| `victory-list-0..7` | `list/thin.rs:323` (`VICTORY_THIN_THREADS`, default 8, 1..32) | Pop `Recording`s from `READY` (Mutex+`WORK` Condvar) and replay their streams through the normal list methods onto the back list. They are `no_drain_here` threads (they never wait for the line). |
| `victory-submit` | `queue/behind.rs:687` | `Lane::serve`. Takes the oldest entry that is first in its queue and free to run, joins parts (`take_along`), calls `Host::execute_late/execute_parts`, does signals and presents. Above-normal priority, kept on fast cores. A panic here is fatal (`host::fail`). |
| `victory-late-wait` | `host.rs:3326` | Without the relay: waits for the card and settles flights. With the relay: on a `poke`, recycles landed lists and calls `queue::release_held`. |
| `victory-relay` (+ `land.rs` workers) | `host.rs:3336` | Lands flights in order (memory doc). |
| `victory-pipelines-N` (+2 screen-only) | `pipeline/make.rs:503` | Translate shaders and create PSOs. Count: `VICTORY_PIPELINE_WORKERS` or ⅔ of the logical CPUs (2..16; ¾, 2..8 with limited RAM). Lowest priority. |
| `victory-heap-sync` | `descriptor.rs:218` | `sync_heaps` rounds per submission, plus early rounds every `VICTORY_HEAP_SYNC_EARLY` ms. |
| `victory-aside` | `host/aside.rs:67` | Heap buffer-view pass while the title records. |
| `victory-window` | `host.rs:9772` | Our window and its message pump, if the title made none. |
| `victory-card-clock`, `victory-card-awake`, `victory-caption`, `victory-library-trim`, `victory-known`, `victory-wide-N`, `uploads ahead N`, `victory-ask-early`, diagnostics threads | various | Statistics, clock keeping, the pipeline book, wide refreshes and uploads (memory half). |

**Main locks and channels** (approximate order of nesting, *(inferred)*):
`Lane.state` → `Host.submitted` (the submission lock: one submission from
preparation to copy-back, `host.rs:5482`) → `LINE` / `QUEUE_ORDER` → per-list
`deferred` / `state` mutexes. Other locks: `queue::WAITING` and each
`Queue.held`; `list::VALUE_WAITS`, `IDLE`; `descriptor::SHADER_HEAPS`
(RwLock), `VIEWS` (RwLock), and per-slot `kept` mutexes in `HostHeap`;
`pipeline::SHADERS`; `make::QUEUE` + `AVAILABLE`, `DONE` condvars. The relay
thread never takes the submission lock (`relay_wait` doc, `host.rs:3347-3350`).

**The rule that keeps it correct:** the title is told work is done (fence
values, bottom-of-pipe writes, query results) only when the card has really
run it and what it wrote has landed: later than the call, never earlier
(`queue/behind.rs:18-21`, `host.rs:2362-2368`).

---

## 7. Life of a frame

Default build with the launcher settings (`Play-VIctory.cmd`, `scripts/play.ps1`):
`VICTORY_THIN=1`, `VICTORY_JOIN_PARTS=8`, `VICTORY_RELAY=2`, `VICTORY_RELAY_COPY=1`,
`VICTORY_CARD_STEPS=2`.

```
TITLE RECORDING THREAD                 victory-list-N               (host list + steps)
──────────────────────                 ──────────────               ───────────────────
list->SetPipelineState(..) ─► THIN_FUNCTIONS slot ─► Front::put(op|len, args)
list->Draw*(..)                      (pointed-at data copied: put_with)
   every 1024 words: hand_over ─► READY ─► Recording::run ─► translate(stream)
list->Close()  ─► CLOSE op                    └─► list.rs methods on the BACK list
                                                   └─► draw::State::draw → prepare_for → bind
                                                        ├ host PSO: pipeline::host_pipeline
                                                        │   (make::ready: queue + wait budget)
                                                        ├ root sig, heaps, user-data constants,
                                                        │ tables → dynamic slots, root views
                                                        ├ vertex/index: shadow::find_any + refresh
                                                        │   (records a Step::Refresh)
                                                        └ ID3D12GraphicsCommandList::DrawInstanced
TITLE RENDER THREAD
───────────────────
queue->ExecuteCommandLists ─► queue::execute ─► behind::submit
      thin::submitted(lists) closes them ─► LANE.push(Job::Lists(recordings))
      (returns once ≤ VICTORY_SUBMIT_DEPTH (64) runs are queued)

victory-submit (Lane::serve/step)
─────────────────────────────────
Job::Lists met when every Recording.done ─► unfold ─► jobs_of(back lists):
   list::detach (lists before the first cut list) → Job::Run
   list::detach_parts (rest)                      → Job::Value(wait).. Job::Run ..
take_along: later Run/Value/Wait/Signal of the same queue joined (≤ JOIN_PARTS, ≤ JOIN_LISTS=48)
Job::Run/Joined.go ─► Host::execute_parts ─► card_steps::cut_for may split ─► send_late
   └─► Host::submit(runs, parts, late=true)               (host.rs:5474)
        1 submission lock
        2 card_steps(runs): our list in front, if steps read what flights write
        3 else late_blocked → relay_wait / settle
        4 descriptor::begin_sync_heaps (victory-heap-sync)
        5 sweep (when nothing flies, or every 1.5 s)
        6 watch::ask_once, stale_under_flights
        7 Batch::refresh_front / refresh_ahead (wide refresh threads)
        8 per list: run_deferred (steps) + prelude + list
        9 batch.flush, shadow::take_snapshots
       10 picked::declare, declared_writes, card_before (sends up + compare list)
       11 drop heap sync guard (wait), descriptor::make_waiting
       12 queue.ExecuteCommandLists(per part) ; queue.Signal(fence, ++submitted)
   ◄─ Sent { back ranges, writes, card plan } ─► LINE.push(Late::Run(Flight)), Late::Write..
   relay_wait if ≥ VICTORY_LATE_WAIT_LISTS (256) lists in the air

victory-relay (memory doc)
──────────────────────────
waits fence ─► fetch/compare/merge shader writes into title memory
             ─► read-backs, bottom-of-pipe Written::write, fence::set of title fences
             ─► LINE_LANDED notify ─► title threads polling fences/values see them

TITLE RENDER THREAD: queue->PresentX(planes)
─────────────────────────────────────────────
queue::present ─► host copy of plane 0 (+ overlay at the 72-byte RetailPlane stride)
             ─► behind::present(Job::Present) ; waits until ≤ VICTORY_SUBMIT_FRAMES (1) presents queued
victory-submit: Job::Present.go ─► show_planes ─► Host::present_planes  (host.rs:8986)
   copy (or compose two planes) into the swap-chain back buffer on an `ahead` list
   ─► execute_own (the copy flies like any submission) ─► IDXGISwapChain3::Present(0, 0)
   ─► Meter (caption FPS, frame-rate line)
```

Without `VICTORY_THIN`/`VICTORY_ASYNC_SUBMIT`, the same steps run on the
title's threads. Methods record directly (`LIST_FUNCTIONS`), and `queue::execute`
dispatches like this (`queue.rs:568-645`):

1. `submit_cut` (`cuts()`, default on): lists before the first cut list go at
   once (`execute_late` when `late()`). The rest becomes `Held` parts.
2. Else `submit_split`.
3. Else whole-submission holding.
4. Else, as before these existed: `Host::execute` and wait.

`release_held` runs what queues hold once fences or values are met. At a
present, `release_held(forces())` runs everything still held: a wait is not
kept across frames.

With `VICTORY_RELAY=0` (and `VICTORY_LATE_WAIT` unset) `late()` is false. Every
submission is then synchronous: `Host::submit(late=false)` waits on an event,
copies back, and does the bottom-of-pipe writes inline (`host.rs:5753-5901`).

**Frame pacing** is separate (`frame.rs`). A clock ticks at the title's
interval (default 1/60 s). Origin waits hand out frame tokens. The host
`Present(0, 0)` has no vsync.

---

## 8. Pipelines

- **Creation** (`pipeline.rs:598-739`): `CreateGraphicsPipelineState`
  (plain description: shaders only, **no host PSO ever**, refused with
  "graphics state not kept", `pipeline/host.rs:650`), `CreateComputePipelineState`,
  `CreateComputePipelineStateX` (extensions ignored), and
  `CreatePipelineState` (stream). The stream parser `subobject(kind)`
  (`pipeline.rs:665`) gives each subobject's size. The console numbers
  amplification/mesh as 23/24, and an unknown type stops parsing with a
  warning. `GraphicsState::read` (`pipeline/host.rs:170`) keeps blend,
  rasterizer, depth (incl. depth bounds via type 21), input layout, topology,
  RT formats and samples.
- **Device slots 0x1c8 and 0x218** are unknown pipeline creators. They log
  their descriptions and fail (`device.rs:349-400`), and the title goes without
  those pipelines.
- **Host PSO** (`HostPipeline::new`, `pipeline/host.rs:417`): accepts
  compute alone, or vertex|mesh (+ pixel). It refuses domain, hull and
  geometry stages and mesh pipelines on hosts without mesh shaders. It builds
  **our own root signature** from the translated shaders' bindings (one
  root-constant block of user data per stage, root views, static samplers,
  directly indexed heaps; `victory_shader::emit` spaces). It loads or stores
  the PSO through the `ID3D12PipelineLibrary` (`library::pipeline`), named by
  `library::name(kind, root blob, code, g.describe())`.
- **When it is made**: `make::early` at creation if an earlier run drew with it
  (`library::wanted_early`) or it is a known scene input. Otherwise at first
  draw, by `make::ready` (`pipeline/make.rs:229`): it queues as `REQUESTED`
  and waits within a **per-frame budget shared by all threads**
  (`VICTORY_PIPELINE_WAIT_MS`, 500; screen passes `VICTORY_SCREEN_WAIT_MS`,
  10 000). Past the budget the **draw is dropped** (counted in
  `draw::DROPPED`). Diagnostics builds name each pipeline the first time a
  draw or dispatch of it is dropped while it is being made (`dropped: its
  host pipeline is being made (first drop of this pipeline)`); a pipeline
  left out on purpose (`make.rs` `left_out`) is counted under its own
  reason, not as being made.
- **Worker queues**: screen (2 dedicated workers), requested, early, slow
  (≤¼ of the workers at once).
- **The BVH builder** pipelines (`pipeline.rs:244-381`) run unless
  `VICTORY_BVH_BUILDER=0` (`-NoRayTracing`; off by default until
  2026-11-26, when launches without it were found posterised two times in
  three). With it off the ray-resolve and ray-activation programs are left
  out too (`pipeline/make.rs` `left_out`).

---

## 9. Draws, dispatches, indirect

- `draw::State::prepare_for` (`draw.rs:1459`): gets the host pipeline (or
  drops). `bind` (`draw.rs:1554`) does early textures for bindless, heaps,
  root signature + PSO (skipped if unchanged), user data, root views (bounded
  one-view tables by default, `VICTORY_ROOT_SRV_DIRECT` switches to root
  descriptors), descriptor tables into dynamic slots (`State::tables`,
  `draw.rs:3628`) and heap texture passes. Then vertex and index views from
  host copies (`shadow::find_any` + `refresh`), and target/depth states via
  `list::ensure_state` and `HostList::twin_use`.
- Rectangle lists (console topology 17) become two triangles each, on the CPU,
  at submission (`State::rectangles`, `draw.rs:3998`, `primitive.rs`), unless
  the pipeline has the rect geometry stage (`HostPipeline::rects`).
- Texture states: lists assume start/end states per host texture
  (`HostList::states`). Real transitions are made at submission in the
  prelude (`HostList::transition`, `settle`).
- Bottom-of-pipe writes (`WriteValue32/64`), query resolves and timestamps are
  `Deferred::writes`, done when the list has run (`list.rs:2165-2254`;
  `queue::Written::write` also calls `WakeByAddressAll`). Queries never run on
  the host. Results are plausible constants: occlusion 2^20, binary 1,
  timestamps from our clock (`list.rs:1486-1531`).
- Waits for memory values (`WaitValue32/64`) cut the list (`list::cut`).
  `ValueWait::met` reads the value through the kernel (`host::read_into`) and
  counts unmapped memory as met (`list.rs:1988-2018`).
- **ExecuteIndirect** (`list.rs:1885`): only signatures with one argument
  (draw, indexed, dispatch, mesh, console type 105 run as a dispatch).
  Signatures that change root arguments are dropped and logged once. Records
  are copied and **clamped on the GPU** into a device-local scratch buffer
  by an HLSL clamp compiled with `D3DCompile` (`indirect.rs:1-34`), against a
  per-list budget (2^27). Batches (`VICTORY_INDIRECT_BATCH`, default 1) and
  windows (`VICTORY_INDIRECT_WINDOW`) amortise the clamp passes
  (`draw.rs:297-378`). Indirect mesh dispatches through `DispatchMeshX` with
  an argument address are **dropped** (`list.rs:1803-1820`).
- **Unordered access view clears** (`ClearUnorderedAccessViewUint` 0x1a8,
  `...Float` 0x1b0, `list/clear.rs`; added 2026-11-26, before then the
  `missing` stub answered `E_NOTIMPL` and the title's upscaler's clears were
  lost). Arguments as on PC `(gpu handle, cpu handle, resource, values[4],
  rect count, rects)`; slots and order are *(inferred)* from the neighbouring
  clears, and a diagnostics build logs the first 16 calls of each raw ("an
  unordered access view clear's arguments as the title passed them"). The
  view is read from the CPU handle's record (else the GPU handle's) and made
  twice by `HostHeap::make_for_clear`: in a dynamic slot of the host heap
  behind the title's view heap (`State::clear_slot`, which also sets the
  host heaps, as the host's clear wants the GPU handle in the heap set; the
  slot goes back with the list's tables) and in a 1024-slot ring of a heap
  the card does not see (`clear_slot` in `descriptor/host.rs`; the host reads
  that descriptor during the call). Then the host clear is recorded at its
  place in the list, between two UAV barriers. Textures: unordered access
  state, depth textures through their colour twin (`twin_use(.., true,
  true)`, then `twins_written`), texback and levels-on-demand noted.
  Buffers (buffer objects, placed buffer views, windows; host copy in PC
  memory or on the card): the view's range is refreshed and then declared
  written (`Shadow::gpu_writes`), exactly as a shader's UAV view of it, so
  the relay lands the cleared bytes in title memory and no refresh copies
  title bytes over them until then (memory doc §3.6). Float values for raw
  or structured views go in as their bits, for integer formats as integers
  *(inferred)*. Logs: `first unordered access view clear` (once per method:
  what, bytes, size, format, values, rects), `an unordered access view clear
  is left out` (first 16, with why), and on the frame-rate line `UAV clears
  per frame: N, B bytes (R refused, ...)`. Not recorded by the replay
  recorder (`docs/replay.md`).
- Barriers: transitions on host textures only (`list::host_barrier`). Buffer
  barriers, enhanced barrier groups and pipeline syncs become one UAV barrier
  (`list.rs:2141`, `1559`).

---

## 10. VICTORY_* switches read in this half

Rules: `switch::on(name)` means set and not `0` (`switch.rs:9`). Most switches
are read once (`OnceLock`), so changing them at run time does nothing. The
exception is `VICTORY_AB` in diagnostics builds (`ab.rs`). A **D** marks
switches that only work in diagnostics builds.

### 10.1 The ones that change the architecture

| Switch | Values (default) | Effect | Read at |
|---|---|---|---|
| `VICTORY_RELAY` | unset/`2` (on, stage 2), `1` (stage 1), `0` off | Landing thread; also makes `late` stage 3 when `VICTORY_LATE_WAIT` is unset. Off in `cfg(test)` unless set | `shadow.rs:1107`, `host.rs:2484` |
| `VICTORY_LATE_WAIT` | unset (3 if relay else 0), `1`,`2`,`3` | Flights instead of waited submissions. 2: merge title bytes under flights' writes. 3: present sent ahead too. Forced 0 by `VICTORY_GPU_TIMING` | `host.rs:2384` |
| `VICTORY_THIN` | `1` (off) | Thin lists. Needs a host and the line (`thin::on`), and turns on the async line itself | `list/thin.rs:58` |
| `VICTORY_THIN_THREADS` | 1..32 (8) | Translator threads | `list/thin.rs:303` |
| `VICTORY_ASYNC_SUBMIT` | `1` (off; implied by THIN) | The `victory-submit` line. Refuses to start without `late_free_present` and `cuts` | `queue/behind.rs:654` (`ab` at 649) |
| `VICTORY_SUBMIT_DEPTH` | n (64) | Submissions queued before a submit call blocks | `queue/behind.rs:623` |
| `VICTORY_SUBMIT_FRAMES` | n (1) | Presents queued before a present blocks | `queue/behind.rs:631` |
| `VICTORY_JOIN_PARTS` / `VICTORY_JOIN_LISTS` | n (1) / n (48) | Parts/lists prepared as one host submission. Only when `ahead_of_writes()` (`host.rs:3089`); >1 also turns on `refresh_front` | `host.rs:3095` |
| `VICTORY_CARD_STEPS` | `1`,`2` (0) | Card steps; needs relay stage 2 | `host/card_steps.rs:79` |
| `VICTORY_QUEUE_WAIT` | `0` (on) | Queues honour fence waits | `queue.rs:75` |
| `VICTORY_SPLIT_WAITS` | `0` (on) | Hold list by list | `queue.rs:94` |
| `VICTORY_CUT_WAITS` | `0` (on) | Cut lists at value waits | `queue.rs:113` |
| `VICTORY_WAIT_FORCE` | `0` (on) | Present forces held work | `queue.rs:123` |
| `VICTORY_CUT_WRITES` | `1` (off) | Also cut after each bottom-of-pipe value write | `list.rs:2210` |
| `VICTORY_COPIES_IN_PLACE` | `0` (on) | Buffer copies on the card at their place | `host/place.rs:43` |
| `VICTORY_REFRESH_AHEAD` / `VICTORY_REFRESH_FRONT` | `0` (on) / `1` (off) | Refreshes ahead of steps, on threads | `host.rs:7679`, `7698` |
| `VICTORY_LATE_WAIT_LISTS` | n (256) | Most host lists in the air | `host.rs:2610` |
| `VICTORY_LATE_WAIT_WRITES` | `0` (on) | Submissions with bottom-of-pipe writes may fly | `host.rs:2620` |
| `VICTORY_RELAY_COPY` | `1` (off) | Relay fetches on the copy queue | `host.rs:2505` |
| `VICTORY_GPU_TIMING` | set (off) **D** | Each list timed on its own; forces synchronous | `host.rs:1747`, `2381` |
| `VICTORY_PIPELINE_WAIT_MS` / `VICTORY_SCREEN_WAIT_MS` | ms (500 / 10000) | Per-frame pipeline wait budgets | `pipeline/make.rs:189`, `203` |
| `VICTORY_PIPELINE_WORKERS` | 1..64 (⅔ CPUs, 2..16) | Pipeline workers | `pipeline/make.rs:492` |
| `VICTORY_D3D12_DEBUG` / `_GBV` / `_WARP` | on **D** | Debug layer / GPU-based validation / software adapter | `host.rs:849`, `860`, `898` |
| `VICTORY_D3D12_DRED` | `1`, `light`, `faults` **D** | Device-removed data detail | `host.rs:1668-1693` |
| `VICTORY_D3D12_BACKGROUND` | `0` | Disable driver background shader recompiles | `host.rs:919` |

### 10.2 Everything else in this half (location; meaning from the comment at the read)

| Switch | Read at | Note |
|---|---|---|
| `VICTORY_LIST_POOL` | `list.rs:248` | `0`: host lists are not reused |
| `VICTORY_LAZY_PRELUDE` | `host.rs:1225` | `0`: prelude made with every list |
| `VICTORY_SCRATCH_POOL` | `host.rs:1866` | `0`: 4 MiB scratch chunks kept per list |
| `VICTORY_BURY_LAG` | `host.rs:501` | Submissions retired objects wait for. `0`: none |
| `VICTORY_SWEEP` | `host.rs:428` | `0`: idle host objects are never let go |
| `VICTORY_KEEP_FREED` | `host.rs:367` | `0`: freed memory given back at once |
| `VICTORY_TEXTURE_KEEP` | `host.rs:179` | `0`: older VRAM idle rule |
| `VICTORY_WAIT_RESIDENT` | `host.rs:220` | `0`: do not wait for residency |
| `VICTORY_TRAIL` | `host.rs:540` | `0`: no removal-context trail |
| `VICTORY_INDIRECT_CARD_BUDGET` | `host.rs:1738` | `0`: budget in CPU-mapped memory |
| `VICTORY_CARD_REHASH_RUNS` | `host.rs:2147` | Card compare rehash runs (launcher sets 1) |
| `VICTORY_WIDE_MERGE` | `host.rs:2166` | `1`: merges on several threads |
| `VICTORY_FILL_TITLE` | `host.rs:2809` | `1`: fills read title memory (off) |
| `VICTORY_JOIN_COPIES` | `host.rs:3773` | `1`: adjacent copies become one step |
| `VICTORY_DECLARE_ONCE` | `host.rs:3797` | `0`: declare shader writes per dispatch |
| `VICTORY_WALLS_TRACE` | `host.rs:3837` | `1`/`2`: walls trace logging |
| `VICTORY_WIDE_REFRESH` | `host.rs:7607` | `0` off, `1` force; default on multicore |
| `VICTORY_WIDE_FILLS` / `VICTORY_WIDE_TEXTURES` | `host.rs:7868`, `7886` | `0`: serial |
| `VICTORY_COPY_ASK` / `VICTORY_DIRECT_WRITES` | `host.rs:8269`, `8460` | Title-memory copy strategy |
| `VICTORY_HEAP_PASS_ASIDE` | `host/aside.rs:47` | `0`: heap pass inline |
| `VICTORY_CARD_AWAKE` | `host/awake.rs:66` | `1`: keep GPU clocks up (launcher sets) |
| `VICTORY_CARD_SPANS` **D** | `host/spans.rs:69` | Card-side timestamps |
| `VICTORY_PANEL` **D** | `host/panel.rs:39` | On-screen panel (F3) |
| `VICTORY_PLACED_ORDER` | `host/card_steps.rs:103` | `1`: finer refusal for placed copies |
| `VICTORY_SEND_OWN` / `VICTORY_SEND_BAR` / `VICTORY_UNDER_WORDS` | `host/send.rs:27`, `135`, `60` | Memory half |
| `VICTORY_UPCOPY_CHECK` / `VICTORY_CARD_ALL` / `VICTORY_FETCH_CHECK` | `host/upcheck.rs:33`, `954`; `host/torn.rs:35` | Memory-half checks |
| `VICTORY_WAIT_ASK` | `list.rs:2007` | `1`: `VirtualQuery` before reading a waited value |
| `VICTORY_COPY_OVER_TITLE` / `VICTORY_READ_BACK` | `list.rs:1116`, `1209` | Texture-copy sources / read-backs |
| `VICTORY_GPU_LOAD_MS` | `queue.rs:856` | Fake GPU time in timestamp pairs |
| `VICTORY_LATE_WAITS` | `frame.rs:144` | `1`: frame waits sleep to the next tick |
| `VICTORY_DISPLAY_MODE` | `dxgi.rs:238` | `WxH` reported display |
| `VICTORY_FAST_CORES` | `cores.rs:48` | 0..3 (2): which of our threads ask for P-cores |
| `VICTORY_HEAP_SYNC` / `_ASIDE` / `_EARLY` | `descriptor.rs:168`, `183`, `195` | Raw heap-copy mirroring; on its own thread; early rounds (4 ms) |
| `VICTORY_VIEW_INLINE` | `descriptor.rs:897` | `0`: every view a table entry |
| `VICTORY_LENT_HEAP` | `descriptor.rs:591` | `0`: no lent heap for heapless dispatches |
| `VICTORY_DYNAMIC_SLOTS` | `descriptor/host.rs:1494` | Most dynamic slots (2^20) |
| `VICTORY_RAW_VIEWS` | `descriptor/host.rs:1418` | `0`: no raw twins |
| `VICTORY_SHOW_AFTER` | `descriptor/host.rs:292` | Slots show a texture only after its upload ran (launcher sets 1) |
| `VICTORY_KEEP_VIEWS`, `VICTORY_KEEP_SHOWN`, `VICTORY_QUICK_MIRROR`, `VICTORY_VIEW_BYTES`, `VICTORY_VIEW_COPY`, `VICTORY_VIEW_EXTENT`, `VICTORY_SLOT_HOLDS`, `VICTORY_STALE_TEXTURES`, `VICTORY_TABLED_TEXTURES`, `VICTORY_TEXTURE_AWAY_MS`, `VICTORY_LIST_VIEWS` (`toggled!`) | `descriptor/host.rs:4410`, `4482`, `4571`, `1254`, `3452`, `3603`, `3064`, `834`, `920`, `2175`, `5869` | Host slot behaviour. Each `0` restores the older way |
| `VICTORY_INDIRECT_WIDE` / `VICTORY_INDIRECT_REST` | `indirect.rs:280`, `301` | 2^20 groups on AMD / `1`: an over-budget record empties the rest (off by default since f4b48ad: it takes nothing and later records still run) |
| `VICTORY_INDIRECT_WINDOW` / `VICTORY_INDIRECT_BATCH` | `draw.rs:341`, `373` | Clamp windows (off) / batches (1; `2` dispatches do not end batches) |
| `VICTORY_HEAP_TEXTURES`, `VICTORY_TEXTURE_STAGE_MB/_MS`, `VICTORY_TEXTURE_CATCHUP`, `VICTORY_EARLY_TEXTURES_MS`, `VICTORY_BAKE_TEXTURES_MS`, `VICTORY_BAKE_EXACT`, `VICTORY_RECHECK_SOON`, `VICTORY_HEAP_RECHECK`, `VICTORY_HEAP_SPREAD`, `VICTORY_FULL_CARD_PASS`, `VICTORY_REUPLOAD_WHEN_FULL`, `VICTORY_UNWATCHED_FRAMES`, `VICTORY_CARD_ACCOUNTING`, `VICTORY_READY_ONCE` | `draw.rs:2514`, `2522`/`2629`, `2635`, `182`, `198`, `206`, `792`, `628`, `777`, `785`, `996`, `2432`, `4567` | Bindless texture pass budgets and rechecks (borders on the memory half) |
| `VICTORY_BIND_CACHE` | `draw.rs:1189` | `0`: no skip of redundant target binds |
| `VICTORY_ROOT_VIEW_MB` / `VICTORY_ROOT_UAV_MB` | `draw.rs:844`, `1869` | Size limits for root views |
| `VICTORY_WRITE_SKIP` | `draw.rs:274` | `0`: declare every write |
| `VICTORY_THREAD_DIMENSIONS` | `draw.rs:4404` | `0`: DispatchX thread counts taken as groups |
| `VICTORY_SCISSOR_TEST`, `VICTORY_MESH_TEST`, `VICTORY_LIST_SLOTS` | `draw.rs:524`, `533`, `4679` | Experiments / frame listing |
| `VICTORY_ROOT_SRV_DIRECT` | `pipeline/host.rs:355` | Root SRV/UAV as root descriptors |
| `VICTORY_PC_DXC` | `pipeline/host.rs:1088` | Path of `dxc.exe` (tests) |
| `VICTORY_SKIP_PIPELINES` **D** | `pipeline.rs:498` | Comma-separated 16-hex shader hashes whose pipelines are skipped |
| `VICTORY_SHADER_SAVE` | `pipeline.rs:154` | Folder: dump containers + translations |
| `VICTORY_BVH_BUILDER` / `VICTORY_BVH_QUARANTINE` / `VICTORY_ACTIVATE_RAYS` / `VICTORY_RT_RESOLVE` | `pipeline.rs:341` (`builder_asked`, on unless `0`), `312`; `pipeline/make.rs:62-80`, `70` | Ray-tracing gating |
| `VICTORY_PIPELINE_SLOW`, `_LOWEST`, `_WAIT_SHARED` | `pipeline/make.rs:355`, `508`, `272` | Worker policy |
| `VICTORY_EARLY_ALL`, `VICTORY_LIBRARY_MAP/_HANDLE/_LOCK/_TRIM` | `pipeline/library.rs:768`, `86`, `95`, `432`, `375` | Pipeline library |
| `VICTORY_PIPELINE_BOOK` | `pipeline/known.rs:43` | Record every pipeline for `victory warm` |
| `VICTORY_AMBIENT`, `_AIR`, `_SHADE` | `ambient.rs:97`, `110`, `120` | Indirect-light stand-in |
| `VICTORY_F16_NEAREST` | `shader_cache.rs` `f16_scope`, `console_f16_for`; `pipeline.rs` `copy` | Which programs get the console's 16-bit rounding: none by default (`0` too); `1` every program (the translator reads that itself); else a comma list of image file names (the image holding the bytecode pointer, named by `note_title_image`, which `victory-hle`'s `set_image_path` calls; e.g. `amd_fidelityfx_upscaler_xbox.dll`) and 16-digit program hashes. Such translations are cached under their own key (`scoped`). `victory warm` (`known.rs`) does not know where a program came from and makes them with the host's rounding |
| `VICTORY_WAVE_*`, `VICTORY_F16_NEAREST`, `VICTORY_HELPER_OWN`, `VICTORY_FIRSTLANE_OWN`, `VICTORY_FIRST_HALF`, `VICTORY_STRICT_JOINS`, `VICTORY_MESH_OUTPUT_STRICT`, `VICTORY_BVH_FENCES`, `VICTORY_BVH_BOUNDS`, `VICTORY_EARLY_DEPTH` | `shader_cache.rs:65-78` | Read by victory-shader; **only hashed into the cache key** here |
| `VICTORY_CAPTURE*` (incl. `VICTORY_CAPTURE_CONSTANTS`, `draw.rs`), `VICTORY_DUMP_TARGETS`, `VICTORY_DUMP_ONLY`, `VICTORY_DUMP_RAW`, `VICTORY_AB`, `VICTORY_BARRIER_TEST`, `VICTORY_COPY_MODE`, `VICTORY_MEMORY_MAP`, `VICTORY_TIMELINE*`, `VICTORY_POP_*`, `VICTORY_FAULT_BOOK*`, `VICTORY_BOOK`, `VICTORY_SLOT_CHECK` **D** | `capture.rs:15-361`, `ab.rs:26`, `copystat.rs:179/275/532`, `timeline/record.rs:467-474`, `popwatch.rs:46/295/314`, `faultbook.rs:87`, `book.rs:77/86` | Diagnostics |
| `VICTORY_BENCH_*`, `VICTORY_BLOCK_WAY`, `VICTORY_TEST_ROUNDS`, `VICTORY_SEND_CHANGED` | `indirect.rs:974-1910`, `host.rs:10135-10455` | Tests/benchmarks only |

---

## 11. Recipes

### 11.1 Add a D3D12.X method

1. **Find the slot.** Run the title. The `missing` stub logs
   `method not implemented object=<kind> slot=0x.. caller=+0x..`
   (`object.rs:294`). The caller offset is into the title image, so
   disassemble there for the argument shape.
2. **Write it** in the module that owns the object, as
   `extern "system" fn name(this: This, ...) -> HRESULT` (or the real return
   type). `this` is a pointer to our struct. Cast it the way `queue(this)` or
   `heap(this)` do. Treat every title pointer as possibly null.
3. **Put it in the table**:
   - device: `device.rs:420` `FUNCTIONS`, `f[offset / 8] = ...`;
   - list: `list.rs:2260` `METHODS` or `MORE_METHODS`, using the **older
     layout** slot index; `newer_slot` shifts slots ≥ 59 by 2. A method only
     newer lists have goes in `NEWER_METHODS` with its absolute index;
   - queue: `queue.rs:1221` (`newer_slot` shifts ≥ 14);
   - other objects: their `FUNCTIONS` `Table<24|32>`.
4. **Lists that record: make it thin-safe.** In `list/thin.rs`, add a
   `plain!` line (all-value arguments) or a new op (`after(n)`), a
   `put_with` that **copies everything the pointers point at**, and a
   `translate` arm. Then add the slot to `MINE`. The test
   `every_recording_method_is_written_down` (`list/thin.rs`, tests) fails
   otherwise. (The unordered access view clears, `CLEAR_UAV_UINT/FLOAT`, are
   an example of an op whose CPU handle record is copied at the call.) A slot that records nothing goes in its `NOTHING` list.
5. A list method that reaches the host goes through
   `with_state(this, |host_list, state| ...)` (`list.rs:152`). For non-Windows
   builds add an alias in `mod state` (`list.rs:1838`).
6. Work that must happen at submission (title memory reads or writes) is
   recorded as a `Step` via `HostList::defer*` (`host.rs:6833-7300`), never
   done at record time. Bottom-of-pipe effects go in
   `HostList::defer_write`.

### 11.2 Add pipeline state / a stream subobject

1. Size it in `pipeline::subobject` (`pipeline.rs:665`). An unknown kind stops
   parsing the rest of the stream.
2. Keep it in `GraphicsState` (`pipeline/host.rs:22`) and its `read`
   (`:170`). Apply it in `HostPipeline::new` (`stream_pipeline`,
   `graphics_depth_bounds_pipeline` or `mesh_pipeline`).
3. **Include it in `GraphicsState::describe`.** The library name hashes
   `describe()` (`pipeline/host.rs:673`, `751-757`). *(inferred)*: a state
   left out of the name makes two different PSOs share one library entry.
4. If it changes which translated shader a PSO needs, the shader-cache key
   (`shader_cache::build`) must change too.

### 11.3 Debug a wrong or missing draw

1. Use a diagnostics build (`cargo build --profile diag -p victory --features diagnostics`).
2. Read the log line at power-of-two frames: `frame presented draws=.. drawn=..
   dispatches=.. dispatched=..` (`queue.rs:943`). Draws minus drawn are
   dropped draws.
3. Find the reason:
   - "no host pipeline: <reason> (logged once)" (`pipeline/host.rs:404`);
   - "indirect execution dropped" (`list.rs:1927`);
   - "a draw or mesh dispatch with another kind of pipeline" (`draw.rs:1468`);
   - pipeline wait-budget exhaustion: try `VICTORY_PIPELINE_WAIT_MS=30000`,
     or warm with `victory warm --used`.
4. Remove the concurrency: `VICTORY_RELAY=0` (fully synchronous), leave
   `VICTORY_THIN` and `VICTORY_ASYNC_SUBMIT` unset, `VICTORY_CARD_STEPS`
   unset, `VICTORY_JOIN_PARTS=1`. `scripts/play.ps1 -Safe` drops the newest
   ways too. If the bug goes away, bisect the switches.
5. Isolate the pass: `VICTORY_SKIP_PIPELINES=<hash>` (hashes come from the
   "first bind" log, `pipeline.rs:447`), `VICTORY_LIST_SLOTS`,
   `VICTORY_CAPTURE=<dir>`.
6. For GPU faults: `VICTORY_D3D12_DEBUG=1` (+ `VICTORY_D3D12_GBV=1`),
   `VICTORY_D3D12_DRED=light` (the launcher's), `VICTORY_D3D12_WARP=1`
   (a wild access becomes a CPU crash with an address).
7. For per-pass GPU cost and A/B tests: record and replay one frame
   (`VICTORY_RECORD`, [docs/replay.md](../replay.md)), or use
   `VICTORY_GPU_TIMING=1`.
8. For wrong contents, suspect stale host copies first (memory doc).

### 11.4 Add a background thread

Name it `victory-*` and call `crate::cores::keep_on_fast_cores` or
`keep_many_on_fast_cores`. If it may run list or submission code, call
`queue::no_drain_here()`: otherwise `behind::drain` from that thread
deadlocks waiting on itself. Wrap the body in `catch_unwind`, and end the run
with `host::fail` if losing the thread would hang the title (see
`list/thin.rs:318`, `queue/behind.rs:680`).

---

## 12. Invariants, unsafe hot spots, hazards

- **One host direct queue** carries everything the title submits on any
  queue (`Host.queue`). Cross-queue order is simulated by
  `queue::Held` / the line and `ValueWait`. *(Exception: the memory half's
  copy queue for uploads and relay fetches.)*
- **Submission lock**: `Host::submit` holds `Host.submitted` from
  preparation until sent (async) or copied back (sync). Never call
  `Host::execute` from inside a step or while holding a lock a step needs.
  `execute_sync_internal` (`host.rs:5135`) exists for internal uploads from
  thin worker threads, where `execute` would `before_sync` → `drain` and
  deadlock.
- **Never tell early.** Fence values and bottom-of-pipe writes are set only
  after landing (`Late::Signal`, `late_signal`). A `Signal` with flights in
  the air is queued behind them (`queue.rs:723-728`).
- **The title may reset or release lists right after submit.** Everything a
  submission needs is moved out (`list::detach`/`detach_parts`). The title's
  list gets a fresh closed host list from `IDLE`. With thin, everything a
  pointer argument names is copied at the call. Descriptor heaps, constants,
  upload memory and indirect arguments are read later, which the console
  allows until a fence behind the list (`list/thin.rs:20-28`).
- **Objects the title releases may still be on the card**: `host::retire`
  and `retire_resource` keep them until `FINISHED` passes; `HostHeap::kept`
  holds each slot's resource. Freeing early is a GPU page fault and a device
  hang (`host.rs:72-77`).
- **`unsafe impl Send/Sync`** on `Host`, `HostList` holders, `Job`, `Idle`,
  `Recording` internals and `Retired` all rest on "D3D12 objects are
  free-threaded" plus one-thread-at-a-time recording. A title list is
  assumed to be recorded by one thread at a time. `cut` takes
  `&mut *this` without a lock (`list.rs:319-321`).
- **Pointer casts of `This`.** Every method trusts that `this` is ours of the
  right kind. A method put in the wrong table reinterprets memory.
- **Raw title-memory reads** go through `host::read_into` (kernel copy that
  reports missing pages) or under `watch::title_memory_reading`. Direct
  dereference only where the comment proves the memory is committed.
- **Dynamic slots run out** under heavy scenes (2^18..2^20). A refused table
  makes the draw read unmade slots; `State::tables_done` must be called
  after a list runs (`queue.rs:172-177` documents a past regression).
- **Pipeline budget drops draws silently.** A one-off pass whose pipeline
  is not ready is lost for good (`pipeline/make.rs:15-22`).
- **Wait semantics are approximate.** A memory wait holds from its cut
  point. A bottom-of-pipe write completes when its part has run, not at its
  place. Waits not met by a present are forced (`queue.rs:922-927`). A wait
  on unmapped memory counts as met.
- **Present**: only plane 0 (+ one overlay, read at a 72-byte stride) is
  shown. The source must be presentable in a swap format of its family
  (`swap_format`, `host.rs:8631`). Resizing the swap chain settles flights
  first.
- `cfg(test)` turns the relay off (`shadow.rs:1107`), so tests run the
  synchronous path unless they set the switches.
- **Unordered access view clears** (`list/clear.rs`) write title memory the
  way shaders do: only the view's range is declared, so a clear through a
  view smaller than the bytes the title expects cleared clears only the
  view, as on the console. The CPU-handle ring (`CLEAR_SLOTS` = 1024 in
  `descriptor/host.rs`) relies on the host reading that descriptor during
  `ClearUnorderedAccessView*`; a clear refused (no view heap set, no
  dynamic slot, a view the host cannot make, a placed texture's view) is
  left out and counted, never done on the CPU.

---

## 13. Tests for this half

`cargo test -p victory-gpu` runs the unit tests (about 420 `#[test]`s in
the crate across both halves). Many need a D3D12 device and **return early when
`host::host()` is `None`**. On a GPU-less CI they pass vacuously. Some use the
WARP adapter (`warp_*` in `indirect.rs`).

| Area | Tests (file) |
|---|---|
| Object model, private data, IUnknown identity | `object.rs` (5) |
| Device creation, levels, feature queries | `device.rs` (6) |
| Lists: inline fallback, query bounds | `list.rs` (2) |
| Thin streams, table coverage, clear words | `list/thin.rs` (4) |
| Unordered access view clears through title objects: buffers in PC memory and on the card back in title memory, textures (Uint, Float, rects, Float into an integer format), a depth texture through its twin, value conversion | `list/clear/tests.rs` (4) |
| Queues: signals, newer layouts, timestamps | `queue.rs` (5) |
| The async line: ordering, joins, presents | `queue/behind.rs` (4) |
| Pipelines, workers, library, host PSOs | `pipeline.rs` (3), `pipeline/make.rs` (6), `pipeline/library.rs` (5), `pipeline/host.rs` (6, some need `dxc`), `pipeline/known.rs` (2) |
| Descriptors / host heaps | `descriptor.rs` (8), `descriptor/host.rs` (37) |
| Root signatures, command signatures, fences, DXGI | `root.rs` (11), `signature.rs` (5), `fence.rs` (2), `dxgi.rs` (2) |
| Indirect clamp (WARP and host) | `indirect.rs` (25, several `#[ignore]` benchmarks) |
| Submission / relay helpers | `host.rs` (27), `host/card_steps.rs` (9) |
| Draw helpers | `draw.rs` (6), `primitive.rs` (8) |

Useful invocations (all from comments in the code):

```powershell
cargo test -p victory-gpu --lib                       # unit tests
cargo test -p victory-gpu --lib list::thin            # one module
# The clears on WARP, as the launcher runs (a process of their own):
$env:VICTORY_D3D12_WARP='1'; $env:VICTORY_THIN='1'; $env:VICTORY_RELAY='2'
cargo test -p victory-gpu --features diagnostics --lib list::clear -- --test-threads=1
cargo test -p victory-gpu --features diagnostics      # + replay tests (tests/replay.rs)
$env:VICTORY_RELAY='2'; $env:VICTORY_CARD_STEPS='2'
cargo test -p victory-gpu --lib card_steps -- --ignored --test-threads=1
cargo test -p victory-gpu --release --lib indirect::bench -- --ignored --nocapture
cargo test -p victory-gpu --release -- --ignored bare_command_cost --nocapture
```

Ignored tests that "want a process of its own" (`host/upcheck.rs`,
`host/card_steps.rs:1220`) read once-only switches, so run them one at a
time with the stated environment.

---

## 14. Stale or misleading statements found in code comments

These are in the code, not in this page. Each is worth fixing at the source.

- `lib.rs:15-16` says "Planned: `present` (swap chain)". Present exists:
  `queue::present` → `Host::present_planes`. `lib.rs` also omits `cores`,
  `compose`, `ambient` and `host` from its map.
- `host.rs:4-6` says "every submission is waited for … unless
  `VICTORY_LATE_WAIT=1`". `host.rs:2326-2328` says late is "off by default".
  `host.rs:2436` says the relay is "off by default". In fact the relay is
  **on** by default (`shadow.rs:1102-1107`), which makes `late_stage` 3.
- `queue.rs:1-2` says "Submitted lists run on the host GPU and are waited
  for, so fence signals complete at once". `fence.rs:1-2` says "Nothing runs
  on a GPU yet". `query.rs:1-2` says the same. All are from before flights.
- `object.rs:3` says "Our objects are static and never destroyed". Only the
  device and DXGI objects are; the rest are boxed and freed by
  `release_boxed`.
- `pipeline.rs:12-14`: the shortcut says mesh pipelines "keep their
  containers only". `HostPipeline::new` makes mesh pipelines
  (`pipeline/host.rs:426-450`). Only domain, hull and geometry stages and the
  plain graphics description get no host pipeline.
- Comments cite "decision 0007" and "decision 0003". No decisions document is
  in the tree.
- `list.rs:674` says "Every submission of the list has run (submissions are
  waited for)" in `reset`. With flights that holds only because the
  submission detached the host list.

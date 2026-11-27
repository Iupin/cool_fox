# victory-gpu: memory, resources, read-back and replay

This is the memory half of `crates/victory-gpu`: host copies of title memory,
write detection, uploads, read-back of shader writes, texture upload and
residency, GPU-memory budgets, and frame record/replay. The command half (lists,
draws, indirect, descriptors, pipelines, submission threading, present) and the
crate's full module map are in [victory-gpu.md](victory-gpu.md). Where this file
says "the preparing thread" or "a list's steps", the details are there.

Everything here was checked in the code at the commit this file was written
against (f4b48ad). Claims marked *(inferred)* were not checked line by line.
`path:line` is relative to `crates/victory-gpu/src/` unless it says otherwise.
Line numbers drift, so search for the symbol if a line does not match.

---

## 1. The problem

On the console the CPU and GPU share one physical memory and one address space.
A title GPU virtual address is a CPU address, and a shader reads the bytes the
CPU wrote a moment ago (`lib.rs:1-7`). On Windows the card is discrete. Host
D3D12 resources have their own memory, and nothing the title writes reaches them
by itself.

VIctory keeps the console model on the title's side:

- Title heaps and resources are **ordinary host memory**. A resource's "GPU
  address" is its CPU address (`resource.rs:1-5`). `create_heap`
  (`resource.rs:1069`) allocates zeroed memory with `Memory::new`. A committed
  resource gets memory from the **title's own allocator** when the runtime named
  one with `use_allocator` (`resource.rs:946-953`, `Memory::for_resource`). A
  placed resource is `Backing::Placed { heap, offset }` (`resource.rs:1119`). No
  host D3D12 heap backs any of these.
- The host GPU only ever sees **host copies**:
  - Buffers get a `Shadow` (`shadow.rs:28`), made on the first use by a draw.
  - Textures get a `HostTexture` (`resource.rs:1158`), made lazily on first use
    (`resource.rs:1201-1206`, `VICTORY_LAZY_TEXTURES`).
  - Memory outside any title buffer that a root view or a shader names gets a
    **window** (a `Shadow` with `window = true`, 4 MiB grid, `shadow.rs:5224`).
  - Pages a shader reaches by raw address get an **arena** page (`memory.rs`).

So coherence has to be kept by hand, in both directions:

```
 CPU -> GPU  (every submission)
 title thread writes title memory
     |  detected by: GetWriteWatch dirty bits (write-watched reservations)
     |               or a write fault on a read-only guarded block (physical-page views)
     |               or a content hash (texture memory nothing else can see)
     v
 watch.rs journal: page -> number of the asking that last found it written
     |  at submission, for every range the submission reads (deferred "refresh" steps)
     v
 Shadow::update / Staging::refresh: copy the stale pages (or detile the texture) into
 the host copy's CPU-visible memory
     |  card copies only: dirty ranges -> card side (Host::card_before, a list of ours ahead of the submission)
     |  textures: staging buffer -> host texture (CopyTextureRegion recorded in the title's list,
     |            or on a copy queue of our own with VICTORY_UPLOAD_AHEAD)
     v
 host GPU runs the title's translated lists

 GPU -> CPU  (after the submission has run: "landing")
 shader writes to ranges declared written (UAV views)
     |  PC-memory copies: CPU snapshot before; compared after (AVX2, wide threads)
     |  card copies: a compute pass of ours hashes 64 KiB pieces and flags those that differ
     v
 only the differing pieces are fetched (card) or found (PC)
     |  merged: only the bytes the card changed go into title memory (title bytes the card
     |  did not touch keep the title's value), dword runs, on the land pool threads
     v
 title memory; then the flight's read-backs (buffer read-backs, texture read-back:
 texback tiles texels back into title memory), bottom-of-pipe writes and fence signals,
 in submission order
```

A submission "in the air" (sent and not yet landed) is a **flight**. The thread
that lands flights is the **relay**.

---

## 2. Vocabulary

| Term | Meaning (where defined) |
|---|---|
| **host copy** | The host D3D12 resource that stands for title memory: `Shadow` for buffers and windows, `HostTexture` for textures. |
| **page / block** | 64 KiB, the tracking and copy unit (`watch::PAGE`, `watch.rs:36`). The kernel's 4 KiB pages are its 16 **parts** (`VICTORY_PAGE_PARTS`). |
| **asking** | One query of write state. It has a number (`watch::ASKED`). `GetWriteWatch` with reset clears the kernel's bits, so every answer goes into the **journal** (`watch.rs:121`). Then every overlapping host copy can see a write that another copy's asking found. |
| **refresh** | Bringing a host-copy range up to date from title memory at submission (`Shadow::refresh` defers it, and `Shadow::update` does it, `shadow.rs:3133,3681`). |
| **write guard** | For physical-page views (xmem sections), which cannot be write-watched: a host copy leaves each 64 KiB block it took read-only. The first title write faults into our vectored handler (`watch::install`, `watch.rs:2559`). The handler notes the block as written and opens it again. |
| **a block that rests** | A block that a write fault has just opened stays open for `VICTORY_GUARD_REST` µs (3000 by default) instead of being re-protected by the next refresh. During that time it counts as written at every asking (`watch.rs:1329-1356`). This stopped the cycle of protect, fault and reopen up to a dozen times a frame. |
| **view guard / aliases** | One physical page can be shown at several addresses. A block counts as clean only while *every* view of its page is read-only (`watch.rs:17-27`, `cover`, `open`). This does not apply to pages shown at more than `MANY_VIEWS` (64) places. |
| **beside** | Writing title bytes through a view of our own of the same physical pages (`inplace::beside`). The guarded title view stays read-only and no protection changes. Used by landing (`VICTORY_LAND_BESIDE`) and by the title-write emulation in `titlewrite.rs`. |
| **declared range** | A range that a list's shaders may write (its UAV views). Declared with `Shadow::gpu_writes` (`shadow.rs:3197`) and taken at submission with `Shadow::writing` (`shadow.rs:3219`). Until it has landed, no refresh copies title memory over it (`pending`). Declared by shaders' unordered access views (`draw.rs` `readable`, `root_uav`), copies in place (`host/place.rs`), card steps, and the title's unordered access view clears (`list/clear.rs`: the view's range, refreshed first, cleared by the host's clear at its place in the list). |
| **snapshot** | A copy of a PC-memory host copy's declared bytes taken before the submission runs. What differs afterwards is a shader write. Snapshots come from a reused pool (`VICTORY_SNAPSHOT_POOL`). |
| **card copy / two sides** | A big title buffer whose host copy is in VRAM (`VICTORY_CARD_BUFFERS`). The **card side** is `Shadow::buffer`, which shaders see. The **CPU side** is `Card::staging`, an upload heap that refreshes write into. Dirty CPU-side ranges are sent to the card before each submission. Shader writes are found by a hash compare on the card (`shadow.rs:136-166`). |
| **flight** | A submission sent without waiting for it (`VICTORY_LATE_WAIT`, `host.rs:2326-2375`). |
| **settle** | Waiting for the flights and landing them. With the relay, `Host::settle` only waits until the relay has landed them (`host.rs:2453`). |
| **relay** | The thread `victory-relay` (`relay_worker`, `host.rs:3408`). It lands flights in order as soon as the card has run them, without the submission lock. |
| **land / landing** | Putting what a flight's shaders wrote into title memory, then its read-backs, bottom-of-pipe writes and fence signals (`Host::relay_land`, `host.rs:4849`). The merge runs on the `land.rs` thread pool. |
| **card steps** | Steps of a submission that read title memory a flight may still write: constant-buffer fills (stage 1) and the title's buffer copies (stage 2). They are done on the card, in queue order, instead of waiting on the CPU for the flight to land (`host/card_steps.rs`, `VICTORY_CARD_STEPS`). |
| **copies in place** | The title's `CopyBufferRegion` done on the card at its place in the list, not on the CPU at submission (`host/place.rs`, `VICTORY_COPIES_IN_PLACE`). |
| **picked views** | UAVs a shader picks from the heap by a computed index, so the views it writes are not known from its bindings (`picked.rs`, `VICTORY_PICKED_WRITES`). |
| **residency** | In this fork this means memory budgets: host commit-charge admission (`residency.rs`), VRAM budget rules for host copies (`resource::room`, `shadow::card_room_for`), idle sweeps, pool growth and back-off, and `MakeResident` waits (`host.rs:210-348`). |
| **show after** | A bindless slot shows a newly made texture only after the submission holding its upload has finished. Until then it shows a 1x1 grey neutral texture (`descriptor/host.rs:284-293`, `VICTORY_SHOW_AFTER`). |

---

## 3. Buffers: `shadow.rs`, `watch.rs`

### 3.1 Data structures

`Shadow` (`shadow.rs:28-105`), one per title buffer, window or arena page:

| Field | Role |
|---|---|
| `buffer`, `data`, `gpu`, `start`, `len` | The host buffer, its CPU mapping, its GPU VA, and the title range. A title address `a` maps to offset `a - start` (plus the `place` bias for in-place buffers, `Shadow::offset`). |
| `copied: Vec<u64>` | Per page: the asking after which the page was last copied (0 means never). |
| `ranged` | The last and the longest range taken whole, with their asking, for pages that are only partly held. |
| `watched: AtomicU8` | Unknown, write-watched (1), `NOT_WATCHED` (2: copied whole every time) or `GUARDED` (3). |
| `pending` + `flights` | Declared ranges not yet landed. With the relay they are counted per submission (`Flights`, `shadow.rs:1140`), so a range two flights declared stays closed until both have landed. |
| `before` | The snapshot buffer (PC copies only). |
| `merging: RwLock` | Held shared while land jobs write pieces and exclusive while title bytes go in under shader writes (`merge_under_writes`). It is taken before `before`'s lock. |
| `card: Option<Card>` | The two-sides state: `staging`, `before` (card-side twin), `dirty`, and `sent` (numbered sends, so a relay merge can tell title bytes the card had not yet received from bytes the card wrote). |
| `place` | `crate::inplace`: the buffer is placed in a host heap opened on the title memory itself. Nothing is copied either way (`Shadow::placed`, `shadow.rs:3118`). |

Lookup: `BUFFERS: RwLock<BTreeMap<(start, resource), Entry>>` (`shadow.rs:4942`)
holds registered title buffers. `find` / `find_any` / `by_address` / `exact`
return or make a copy. A 1024-entry hash cache sits in front of them (`FOUND`,
`VICTORY_QUICK_FIND`). Windows are in `LOOSE` (`shadow.rs:5422`).

`watch.rs` holds the process-wide state:

- `JOURNAL: RwLock<Journal>` (`watch.rs:145`): `written` maps page to asking
  number, `parts` holds per-4 KiB askings, and `ahead` holds the ranges asked
  ahead for the whole submission (`ask_ahead`, `ask_once`).
- `ASKING` (`watch.rs:202`): page ranges currently being asked about. Asking
  resets the kernel's bits, so no other thread may ask about overlapping pages
  until the answer is in the journal. It is taken before the journal lock and
  never under it.
- `GUARDS: Mutex<Guards>` (`watch.rs:1364-1426`): `protected`, `clean`,
  `absent` (wholly uncommitted, copied as zeros), `held` (placeholders),
  `foreign`, `kernel` (ranges the kernel is writing now), `kept` (ranges an
  overlapped read may still land in, with a deadline), `changing` (mid-remap),
  `aliased`, and `opened` (rest timestamps).
- Lock-free bitmaps `PUBLISHED`, `PROTECTED` and `BESIDE` (`watch.rs:951-969`),
  read by the fault handler without the lock.
- Diagnostics builds time how long `GUARDS` is held, by the line that took
  it (`guards()` and `guards_timed()` are `#[track_caller]` there and hand
  out `held::Held`, which counts on drop): the copystat line `waits for the
  write trackers' locks` ends with `the guards held by: watch.rs:<line> Nx
  T ms (longest L)`, the eight busiest.
- `MAPPING: RwLock<()>` (`watch.rs:2243`): held shared by in-place reads of title
  memory (`title_memory_reading`) and exclusive by the runtime while it remaps
  (`title_memory_remapping`). Without it, a page went away between "is it
  committed?" and the read (runs 71 and 74).

### 3.2 Hooks the runtime calls (victory-hle `xmem` and file I/O)

These are re-exported from `lib.rs`: `title_memory_allocated`, `_freed`,
`_remapping`, `_remapped`, `_unmapping`, `_splitting`, `_reading`, `_written`,
`_guarded` (with `Guarded`), `kernel_writing` (with `KernelWrite`), and
`writing` (exported as `kernel_will_write`). The rules:

- Before the kernel writes title memory (a file read), call `kernel_writing`.
  It opens the guarded blocks and keeps them open until it is dropped. A kernel
  write into a read-only page does not fault into our handler: it fails
  (`ERROR_NOACCESS` or `ERROR_INVALID_USER_BUFFER`), and the read is lost
  (`kernel_writing`'s comment).
- A read that goes on after its call (overlapped, `ERROR_IO_PENDING`) hands
  its `KernelWrite` to `keep_for(time)`: the blocks stay left alone (never
  guarded, told as written at every asking) until the deadline, then are
  guarded at the next refresh, which counts them written. The kernel has
  locked the pages by then, so a late guard would not stop the write, but a
  refresh that made the block clean before the bytes landed would keep the
  old bytes in every copy (`a_kernel_write_kept_past_its_call_stays_open_until_its_time_is_up`).
- `title_memory_guarded(at, len) -> Guarded { blocks, read_only, other_views }`
  says how a failed read's destination is guarded, for the runtime's
  warnings (takes the guards' lock; for reports only).
- DirectStorage writes physical pages through the runtime's own view, so the
  guards do not see it. The runtime reports it with `title_memory_written(..,
  pages = true)`. What happens next is set by `VICTORY_PAGE_WRITES`
  (`watch.rs:2100-2116`).
- A new view of a guarded page makes the page's other views get checked again
  (`views_mapped`, called from `inplace::title_memory_view`).

### 3.3 Fast path and slow path of a buffer refresh

On the preparing thread at submission, or on the wide helper threads, for each
deferred refresh:

1. **`Shadow::nothing_new`** (`shadow.rs:3903`): if the copy is watched or
   guarded, no declared range overlaps, and the journal already says that no
   page was written after `copied`, nothing is done and the kernel is not asked.
   This is the hot fast path. Since 2026-11-27 the journal keeps, for each
   run of 64 pages (`CHUNK`), the newest asking that found a page there
   written (`Journal::chunks`, raised in `Journal::note`, never lowered,
   as the pages' own entries); `Journal::nothing_after` passes over a run
   whose newest write is no newer than the oldest taking of its pages, and
   looks at a run's pages one by one only otherwise. The answer is the same
   (a unit test compares both on 4,000 random cases); the title's 174 MB
   heap is 2,700 pages, nearly all written some time, and this check was
   5 % of the submitting thread (run d0Prof).
2. **`Shadow::update`** (`shadow.rs:3862`) skips declared ranges and refreshes
   the gaps with `update_free`.
3. **`update_free`** (`shadow.rs:3939`) calls `watch::writes(from, to, guarded)`.
   That uses the journal, or `GetWriteWatch` for write-watched memory
   (`kernel_written`, `watch.rs:248`), or guard bits for views (`guard`,
   `watch.rs:1636`). Then `copy_stale` copies only the stale pages, or parts.
4. **Slow path**: memory that is neither watched nor guarded is marked
   `NOT_WATCHED` and copied whole at every refresh (`shadow.rs:3955-3964`).

Host memory handed to the title (2026-11-27, `watched.rs`,
`VICTORY_WATCHED_HEAP`, on): blocks of 64 KiB or more that the title's
general allocator takes from the host (`XMemAllocDefault` in victory-hle,
which the title's own allocation hooks fall back on) and the heaps the title
makes through the device (`resource::Memory::new`) are reservations of their
own made with `MEM_WRITE_WATCH`, so a host copy of a buffer there takes the
fast path above instead of the slow one. Before, such memory came from the
process heap: the title's upscaler scratch buffer (21,045,760 bytes, at a
host address above 2^40) was copied whole twice a frame and sent up whole to
its card copy every frame. Layout: whole 64 KiB of reservation around the
caller's bytes, so an asking about their 64 KiB pages never reaches into the
next reservation (which the kernel refuses); one committed 4 KiB page below
them, whose first word holds the reservation's base for `watched::free`
(victory-hle's block header, `[size, 0]`, lies just below the pointer). The
kernel counts every writer: the title, the kernel's own writes (a `ReadFile`
into the block, tested) and our landings, which are then compared by the next
refresh (`VICTORY_LAND_ECHO`) and not sent up again. Smaller blocks stay on
the process heap.

Copies of title memory use `safecopy::copy` (`safecopy.rs`). It is one
`rep movsb`. A fault inside it is caught by the same vectored handler
(`safecopy::failing_exit`, `watch.rs:2573`), which either opens a block we
guard or makes the copy fail cleanly. It replaced roughly 35,000
`ReadProcessMemory` calls a frame (`VICTORY_SAFE_COPY=0` goes back to them).

Kernel-call batching: `ask_ahead` / `ask_once` (`watch.rs:315,530`) ask once
for all of a submission's ranges that lie within 8 MiB of each other. With
`VICTORY_ASK_EARLY`, the thread `victory-ask-early` asks hot pieces in advance
while the title records.

Refusals remembered (2026-11-27, `VICTORY_ASK_REFUSED`, on): a call the
kernel refuses (the range is not write-watched memory of one reservation)
costs about what an answer does, 25 µs and more whatever the range, and two
in three of a frame's ~540 askings were refused, the same ranges every frame:
view memory (guarded instead), runs of a submission's ranges across two
reservations (then asked by halves, `ask_run`), memory that is not watched.
`kernel_written` keeps the exact page ranges refused (`REFUSALS`) and does not
ask about them again while `REFUSAL_EPOCH` is unchanged and for at most a
second. The epoch moves at every change of the address space we are told of
(`address_space_changed`: `title_memory_allocated`, `_freed`, `_remapping`,
`_remapped`, `_unmapping`, `_splitting`, `views_mapped`, and
`watched::alloc`/`free`); a refusal noted by a call during which it moved is
not kept. Why nothing is lost: a remembered refusal makes the caller do what
a refusal does (ask by halves, ask the guards, copy whole), and a call that
fails resets no kernel bit, so memory that became write-watched since keeps
its bits until it is asked again. The diagnostics line `askings the kernel
refused per frame` counts refusals by caller and those not asked again.

### 3.4 Write faults: the guard handler

`watch::install` (`watch.rs:2559`) registers a first-chance vectored handler,
ahead of the crash reporter's. For a write access violation at `at`:

1. If the block is in `PROTECTED` and `titlewrite::store` can emulate the
   instruction, the store is done through our view and the thread continues.
   The block stays read-only (`VICTORY_STORE_BESIDE`, off by default).
2. Otherwise, up to three tries: if another thread has already opened the
   block, continue. If not, take `GUARDS`, `open` the block in all its views,
   note it in `opened` (rest), and continue.
3. A fault inside our `safecopy` goes to its failing exit.
4. If a remap is under way, `came_back` waits for it and retries (`watch.rs:2344`).
5. Everything else continues the search, so a real crash goes to the reporter.

`titlewrite::import` replaces the title's `memcpy`, `memmove` and `memset`
(`VICTORY_COPY_BESIDE`, off by default). The replacements write guarded parts
"beside" without faulting.

### 3.4a Who writes a range: the writers watch (`writers.rs`, diagnostics)

Added 2026-11-26 to find the title routine that leaves default values in a
scene-constant block in some launches. `VICTORY_WRITERS=<hex address>+<hex
length>,...` (title addresses) and/or `VICTORY_WRITERS_OF=<16-hex shader
hash>:s<slot>+<dword offset>,...` (a table view of a shader, named as the ops
listing names it) start it. The switches are read only in diagnostics builds
(`victory_hle::watch_writers`, called from `apps/victory/src/run.rs`
`diagnostics`); with them unset nothing is armed and every hook below is one
atomic load.

How it works:

1. The 4 KiB pages holding each range are made read-only for the time window
   (`VICTORY_WRITERS_FROM` seconds after the title starts, for
   `VICTORY_WRITERS_FOR` seconds; default: from the start to the end).
   Outside the window this facility leaves the pages as it found them. A
   range none of whose pages is there yet (not committed) is tried again
   every 100 ms while the window is open (`none of the N pages ... can be
   watched yet`, said at the 1st, 2nd, 4th... try). Code is named as the
   loader maps it: the title's executable is the prepared copy
   `victory-image-<hash>.dll`, whose rvas are the original executable's.
2. A write into such a page faults into `watch::install`'s handler, which
   hands it to `writers::on_exception` first. Under the guards' lock
   (`watch::writer_opens`) the 64 KiB block is opened exactly as the guards'
   own fault path would (`open`, `Guards::opened`, the fault book), so the
   journal and every host copy still see the write; then the 4 KiB page is
   made writable. The write is noted (below), the trap flag is set, and the
   thread goes on. The instruction runs unchanged; the single-step exception
   after it reads the bytes written, makes the page read-only again and
   clears the flag. A `rep movs`/`rep stos` is stepped until its `rip`
   moves on (one trap per element), and is noted once with the bytes it
   covered. An instruction that faults again in another watched page (a
   store across pages) adds that page to the step.
3. Noted per (instruction, offset in the range): count, first and last
   frame and time, size (`titlewrite::store_extent`, the store decoder of
   the title-write emulation; `?` where it does not decode), the first four
   distinct values and the last, how often the value changed, the first and
   last thread, and for the first `VICTORY_WRITERS_DETAIL` (64) writes a
   stack of `VICTORY_WRITERS_STACK` (12, at most 32) frames through the
   loader's `unwind_here`, of which the first is kept and the others are
   counted when different. Past the detail only counts and values are kept.
   Writes elsewhere in a watched page are counted per instruction only.
   Code is named with the loader's `locate` (`module+0xrva`).
4. Coexistence: the guards' `unguard` calls `writers::reguard` for each block
   it makes writable, which makes the watched pages in it read-only again
   (not those being stepped or written by the kernel). `kernel_writing`
   holds the watched pages open for the kernel (`writers::kernel_begin`
   before the blocks open, `kernel_open` under the lock) and notes the write
   as `kernel`, with the first caller outside our image as its instruction,
   when the `KernelWrite` is dropped (`keep_for`: when its time is up).
   Writes told after the fact are noted as told: `kernel_will_write` as
   `kernel (told after)`, DirectStorage page writes
   (`title_memory_written(.., true)`) as `kernel (page write)`, landings
   beside the guards (`landed_beside`) as `gpu (landed)`. A landing that
   writes the title address itself faults like any write and is named by its
   `safecopy` instruction in our image. `title_memory_remapped` re-arms
   watched pages there. Other views of a watched page (`inplace::
   shown_elsewhere`, at arming) are watched too and their writes noted at
   the range's offset.
5. `VICTORY_WRITERS_OF`: `draw.rs` `State::writers_of` (from `prepare_for`,
   diagnostics builds, while `writers::looking()`) reads the view at the
   named binding of each draw whose pipeline has that shader, as
   `view_words` does (`view_range`: a constant view, or a placed buffer
   view), and `writers::bound` takes up to 4 distinct addresses per binding
   (double-buffered blocks show at two), each a range of its own, armed when
   the window opens.

Output (info level, all lines start with `writers:`): `watch asked for`;
`<binding> #n is title memory <address>+<len>`; `watching <range> at ...:
N of M pages read-only (K at other views)`; `words of <range> ... at the
start of its watch` and `... at the window's end` followed by 8 words a line
(the first 256 words); `new writer <code> (<kind>) <range>+<offset> xN size S
... values [...] last ... thread ... stack [a < b < ...]` the first time each
(instruction, offset) has a value; `table at 10 s` every 10 s while something
new came, `table at the window's end` and `table at exit` (the title's `exit`,
`TerminateProcess` of itself, a suspend's end), with one row per writer and
the busiest 16 instructions that wrote elsewhere in the pages. Values are
shown as little-endian words with their meaning as floats.

Cost: each write into a watched page is two exceptions and two
`VirtualProtect`s (some 10-20 µs), past the detail limit too; a string store
is one trap per element. A range written thousands of times a frame costs
milliseconds a frame: use the window.

Limits:

- While one thread steps a write, the page is writable: another thread's
  write to the same 4 KiB page in that moment is not seen.
- Views of a watched page mapped after its range was armed are not watched;
  writes through them are missed (the guards see them).
- A kernel write into a watched page that does not come through
  `kernel_writing` fails, as it would into a guarded block: a host API the
  title calls directly with an output buffer there, or a future caller of
  `kernel_will_write` ahead of its write (none exists: DirectStorage's
  retries are inside `kernel_writing`, `xmem::pages_written` comes after the
  bytes). Title calls of `VirtualProtect` on the page undo the watch.
- Checks that ask Windows whether memory is read-write see the watched pages
  as read-only (`inplace::private_whole`, used with `VICTORY_INPLACE` only).
- With `VICTORY_STORE_BESIDE=1` a watched write is not emulated beside the
  guards; it takes the path above.
- At most 128 pages (views included), ranges of at most 64 KiB, 2048
  writers and 512 instructions elsewhere; more are counted as not kept.

### 3.5 Card copies (two sides)

The rule for which buffers get them is `card_for` / `card_room_for`
(`shadow.rs:482-612`), deciding through the pure function `card_admit`
(`shadow.rs:354`, unit-tested in `card_limit_tests`) with the limits of
`card_limits()` / `card_limits_from` (`shadow.rs:169-311`):

- `VICTORY_CARD_BUFFERS` is on.
- The size is at least the least size (`CardLimits::least`) and below 1 GiB,
  because the compare counts in 32 bits.
- VRAM use plus the size stays under 80% of the budget, or 97% with
  `VICTORY_CARD_ROOM_FOR_BUFFERS`, which is on.
- A small copy (under the tier's default least size, `CardLimits::tier_least`)
  fits in the small copies' share (`small_most`), counted in
  `CARD_SMALL_BYTES`. Small copies exist only when `VICTORY_CARD_BUFFERS_MIN_KB`
  / `_MIN_MB` / `VICTORY_CARD_ALL` set the least below the tier's.
- All copies together stay under the cap (`most`, counted in `CARD_BYTES[0]`).
- A copy that is turned down is never asked for again (`lookup` asks once). One
  turned down for VRAM room gives up its kept room (`CARD_REFUSED`); one turned
  down by the share or the cap does not (`CARD_CAPPED`). Both are logged as
  "no card copy for a title buffer of N KiB: ..." (the first 64, then every
  32nd of each kind); the cap line names the share or the cap, the bytes held,
  the small copies' bytes and both limits.

Defaults by local VRAM budget (`default_card_limits_mb`; the
`card buffer limits` log line prints the resolved values):

| Budget | Least size | Cap | Small copies' share |
|---|---|---|---|
| unknown (WARP) | 16 MiB | 512 MiB | 384 MiB (3/4 of the cap) |
| ≤ 12 GiB | 16 MiB | max(512 MiB, budget/7): 1,170 at 8 GiB, 1,316 at 9, 1,462 at 10, 1,755 at 12 | 3/4 of the cap |
| > 12 and < 20 GiB | 4 MiB | 2,048 MiB | the whole cap (no share) |
| ≥ 20 GiB | 1 MiB | budget/4, at most 8,192 MiB | the whole cap (no share) |

The cap never gets smaller as the budget grows (1,755 at 12 GiB, 2,048 just
above). An explicit `VICTORY_CARD_BUFFERS_MB` sets the cap and removes the
share (first come, first served, as before 2026-11-26). The share exists
because with the launcher's `_MIN_KB=64` some 2,900 copies of 64 KiB buffers
and the world's buffers filled a 512 MiB cap before the upscaler's 21 MB
scratch buffer came (`docs/findings.md`, 2026-11-26); three quarters, not
less, so that the 1-16 MiB world buffers measured on the card at that time
still fit when the cap has room.

Per submission (`Host::card_before`, `host.rs:4247`; `card_back`, `host.rs:4994`):

```
 CPU side dirty ranges --(list of ours, ahead on the queue)--> card side
 submission lists run (shaders write the card side)
 CARD_COMPARE compute pass: per 64 KiB piece of each declared range, hash
   card side vs "before" twin (16 bytes of hash per piece, card_hash_bytes)
 flagged pieces fetched (one more wait, or the relay's fetch list), then merged
   into title memory and the CPU side (Shadow::card_merge / card_merge_relay)
```

What changed on the CPU sides is taken by `shadow::card_dirty`. Since
2026-11-27 (`VICTORY_CARD_TOUCHED`, on) it takes only the copies in
`TOUCHED`, a list a copy is put into when its `touched` flag goes from unset
to set; before, every submission looked at all ~3,700 card copies (3.8 % of
the submitting thread in a gameplay profile). The rule it rests on is the
old one: what puts ranges into `dirty` sets `touched` under `dirty`'s lock,
and what takes `dirty` clears `touched` under it, so a change after a take
puts the copy into the list again and one before is in what the take took.
A copy changed before it was registered is put in by `carded`.

Writes outside declared ranges are not found. Later shaders see them, and they
reach the title only when a declared range later covers their piece
(`shadow.rs:163-166`). `picked.rs` covers views picked by index.
`host/upcheck.rs` (`VICTORY_UPCOPY_CHECK=1`) is the diagnostic for this
assumption.

### 3.6 Read-back and landing (GPU to title)

**Without the relay** (`VICTORY_RELAY=0`): after the submission runs,
`Shadow::copy_back` (`shadow.rs:3331`) compares each declared range with its
snapshot and writes only the changed bytes to title memory. Then
`copy_back_done` reopens the range for refreshes.

**With the relay**, which is the default (see §9):

```
 preparing thread                       victory-relay                     land pool (victory-land-N)
 ----------------                       -------------                     -----------------------
 prepare + send submission N  --LINE-->  wait fence >= flight N
 (no wait)                               GetDeviceRemovedReason check
 prepare N+1 ...                         relay_hand_back: look (scan)  -->  find differing pieces
                                         relay_card_back: fetch pieces       (AVX2, 32 B at a time)
                                         (drawing queue, or copy queue
                                          with VICTORY_RELAY_COPY=1)
                                         merge ---------------------------> safecopy::merge per piece,
                                                                            dword runs, beside views
                                         copy_back_done per flight
                                         read_backs (texback, buffer backs)
                                         Late::Write / Late::Signal, in order
                                         FINISHED = N; bury(N); notify LINE_LANDED
 Host::settle / relay_wait  <------------ (waiters wake on LINE_LANDED)
```

Ordering invariants (`host.rs:2459-2467`, `land.rs:19-23`):

- Flights land in `LINE` order on one thread. A later flight's bytes never reach
  title memory before an earlier flight's.
- What follows a flight (its bottom-of-pipe writes and fence signals) is never
  told before the flight has landed (`landable`, `host.rs:2537`).
- In a landing, pieces in the same 64 KiB of title memory are one job, kept in
  order. Card-copy fetches come before PC-copy merges.
- `merge_piece` (`shadow.rs:1185`) never stores into the card-visible source:
  the card may be writing it for a later flight.
- Sends to card copies are numbered under `QUEUE_ORDER` (`host.rs:2523`). With
  `VICTORY_RELAY_COPY` a fetch runs beside later submissions, so bytes sent
  after its flights are excluded from the merge (`card_merge_relay`). `torn.rs`
  (`VICTORY_FETCH_CHECK`) catches pieces that were fetched while another flight
  was writing them.

When the preparing thread must wait (the six points in `host.rs:2342-2360`):

- it makes a synchronous submission of ours;
- it reads title memory a flight may write, which is `late_blocked`;
- it refreshes a stale range under a flight's declared write, which is
  `stale_under_flights` (stage 2 merges the title's changed bytes instead,
  `late_exact`, `Shadow::merge_under_writes`);
- a queue waits on a memory value;
- the title waits on a fence with no event;
- too many lists are in the air (`late_most`).

With `relay_exact` (stage 2) the submission waits only for the flights that
declared that memory.

Read-back to title memory beside the guards: `VICTORY_LAND_BESIDE` writes
through `inplace::beside`. Then `watch::landed_beside` notes the blocks, so the
next refresh takes them. `VICTORY_LAND_TAKEN` skips that re-take when one copy
alone landed the same bytes (`land.rs:495-511`). `VICTORY_LAND_ECHO` compares
4 KiB pages of landed pages instead of resending whole blocks to the card
(`shadow.rs:2189-2204`).

### 3.7 Card steps and copies in place

`host/card_steps.rs:10-84`: with `VICTORY_CARD_STEPS=1` (constant-buffer fills)
or `=2` (fills and the title's buffer copies), which works only together with
relay stage 2, a list of ours goes to the card ahead of the submission's lists
in the same `ExecuteCommandLists`. It copies from the host copy the flights
write into our buffer, then either into the destination's host copy or by
repointing the fill's own descriptor slot. The destination is declared as a
shader write, so the relay lands it.

Card steps are refused, and the old wait taken, when:

- a dependent step cannot be taken (a texture, expanded vertices, a fill of
  another memory kind);
- another host copy of the bytes is refreshed;
- a page is unmapped.

`plan`, `holder` and `elsewhere` decide this. The motivation was 25-30 ms of a
105 ms frame spent waiting for skinning passes that write through a view of the
whole 174 MB heap.

Copies at their place (`place.rs`, below) and card steps (2026-11-27):

- A fill behind a copy at its place that reads only bytes the copy writes is
  done on the card from the copy's source (`forward`,
  `VICTORY_PLACED_FORWARD`, on). The title copies a few hundred bytes of
  constants into its ring with a `CopyBufferRegion`, and a draw behind it in
  the same list reads them as a constant buffer; the ring lies in the heap the
  skinning passes declare written, so the fill is a card step, done in front
  of the lists, before the copy. Every such submission was refused and waited
  60-80 ms for the flights (all the refusals of the 2026-11-27 runs). Taken
  only where it is what the CPU reads after the wait: nothing between copy and
  fill writes the fill's bytes or the copy's source, no copy at its place
  ahead of the fill writes the source, source and destination do not meet.
  A copy at its place is in the way only of the card steps behind it.
  `VICTORY_CARD_STEPS_CHECK=1` reads back what each such fill was given and
  compares it with title memory after the landing.
- Two holes closed: with card steps taken, the CPU's waits for a copy at its
  place were skipped. A copy whose source is taken from title memory at
  submission (no shader writes that memory) while a flight may write it, or
  behind a card step that writes it, read it too early; a step left on the CPU
  behind a copy that flights keep out of title memory read its destination
  without the copy. Both are refused now (`PLACED_SOURCE`, `PLACED_READ`), as
  the CPU's way waits for them (`Placed::waits`, `place::read_by`).
- Planning them costs the submitting thread some 7 ms a frame for the
  5,000-8,000 fills of the ring (the timeline of run f1Diag1, four frames:
  the slots 5.4 ms a frame, choosing 2.9, the list 2.5, what holds their
  bytes 1.5). What holds each fill's bytes is looked up by pieces of memory
  (`ByPiece`, `VICTORY_HOLDER_INDEX`, on) instead of over every range the
  flights declared.

`host/place.rs` (`VICTORY_COPIES_IN_PLACE`, on): the title's buffer copy runs in
the destination's host copy at its list position. Its bytes count as a shader
write from then on. Copies larger than 8 MiB out of memory that no shader writes
still go on the CPU.

### 3.8 Raw-address memory and upload memory

- `memory.rs`: shaders that use raw 40-bit addresses (the console's BVH builder)
  reach memory through a page **directory**. Each 64 KiB title page maps to
  copy number and offset. Pages outside buffers get **arena** pages
  (`shadow::by_address`). A page that is missing when a shader touches it is a
  lost access: the read returns 0 and the store is dropped. Missing pages are
  reported back once a frame (**faults**). Before BVH dispatches the host
  pre-populates what it can see (`memory.rs:29-43`). Only buffers of at most
  `BACK` bytes come back to the title; structures stay on the card. Arena card
  sides are capped by `VICTORY_MEMORY_CARD_MB` (4096).
- `upload.rs`: `allocate_upload` memory comes in 64 KiB blocks from arenas in
  [4 GiB, 2^37), because addresses must fit in 40 bits. Arenas live as long as
  the process: the directory keeps a copy at an address for good.
  `VICTORY_UPLOAD_LOW=0` takes this memory from the process heap instead.
- `inplace.rs` (`VICTORY_INPLACE`, 0 by default): title memory opened as a host
  heap with `OpenExistingHeapFromAddress(1)` / `...FromFileMapping`, so nothing
  is copied. The constraints are measured in `inplace.rs:9-36`. Note
  `inplace.rs:76-78`: level 2 must not be combined with late wait, and late wait
  is now on by default (§9).

---

## 4. Textures: `texture.rs`, `texture/ahead.rs`, `pool.rs`, `tiles.rs`, `texback.rs`

### 4.1 Layout and detiling

Title textures live in title memory in the console's tiled RDNA2 layout.
`victory_formats::texture::Layout` (`crates/victory-formats/src/texture.rs`,
which follows AMD addrlib) gives sizes, mip placement (the mip tail first,
mip 0 last), and `detile` / `tile` between tiled memory and linear rows. Layouts
with pipe or bank XOR swizzle (`Swizzle::xor`) cannot be read
(`resource.rs:1559`). `resource::texture_layout` and `footprint` turn a
`ResourceDesc` into a layout. `pc_format` and `target_format` map formats.

Depth textures that shaders also write as UAV get a colour **twin**
(`resource::Twin`, `twin_texture`, `VICTORY_DEPTH_TWINS`). The host copies one
into the other both ways.

### 4.2 Upload path (title-filled textures)

`fills(desc)` holds for a texture with no render-target, depth or UAV flag: the
title fills it, and its host copy can be rebuilt from title memory
(`resource.rs:1495`). Any other texture's contents exist on the host only.

```
 recording thread (title list call / victory-list-N)
   texture::upload / upload_from (texture.rs:657,735)
     once per list per first level; skipped if host_wrote (host copied into it)
     staging() -> Staging (staging buffer laid out as host footprints;
                  from pool::staging or a buffer of its own)
     if uploaded && !dirty: only defer_texture (the check at submission)
     else record_upload: CopyTextureRegion staging -> host texture in this list;
          defer_host: mark uploaded / !dirty when submitted
 preparing thread, at submission (deferred texture step)
   Staging::refresh -> refresh_timed (texture.rs:1263):
     write-watched memory: watch::writes; unchanged since last.asked -> nothing
     view memory: guard bits (VICTORY_TEXTURE_GUARD); if unwritten, hashed only every
       8th frame by address turn, or every check while "hot" (64 frames after first
       check or after the hash caught an unseen change; VICTORY_TEXTURE_HOT)
     partly mapped: read through copy_mapped (zeros for holes), at most every 30 frames
     else digest() (128-bit non-cryptographic) of tiled bytes; changed -> detile()
   detile(): Layout::detile per subresource into the staging mapping;
     levels not held -> zeros
 drawing queue: the recorded copy runs with the list
```

Shortcuts (`texture.rs:13-16`):

- Any change detiles the whole texture.
- Only textures read through descriptor tables are brought up to date per draw.
  Textures picked by index are handled by the texture pass in `draw.rs`
  (`heap_textures`), which is the command half.
- Plane 0 is uploaded. Depth and stencil planes are not.

A change found at submission is uploaded by the *next* list that samples the
texture, which is one frame late if all of a frame's lists were recorded before
the check (`texture.rs:819-823`). Bake draws (`BAKING`) always record the copy.

Movie planes (`Staging::movie_plane`: R8, linear, one mip) use a per-list
upload buffer (`list.plane_upload`) instead of the shared staging buffer.

Remapping races: with `VICTORY_TEXTURE_NO_WAIT=1`, which the launcher sets, a
refresh that meets a remap in progress does not wait. It reads through the
kernel (`refresh_unlocked`) or fills what is there (`fill`,
`VICTORY_TEXTURE_FILL`). A short read makes the slots be looked at again
(`descriptor::host::read_short`).

### 4.3 Staging and texture pools

`pool.rs` cuts textures and staging buffers from 64 MiB blocks (`BLOCK`). A
single allocation costs about 310 µs on the RX 7900 XTX, against about 6 µs for
a piece of a block.

| Rule | Where |
|---|---|
| Pieces larger than half a block (larger than a block with ripe blocks) are not pooled | `take_ripe`, `pool.rs:227` |
| A piece given back rests `REST` = 2500 ms before reuse. The module comment says "two seconds", which is out of date. | `pool.rs:76,301` |
| **Ripe blocks**: `VICTORY_RIPE_BLOCKS` (12) blocks are made ahead, and pieces are cut from blocks that every submission begun before their creation has finished. A device loss came from lists already on the card reading a new block's texture. | `pool.rs:87-125` |
| Growth back-off: a failed block creation blocks growth for 500 ms. Existing free ranges are still used. | `Pool::grow`, `pool.rs:278` |
| Spare empty blocks: 64 (`SPARE_KEPT`) while VRAM use is below 85% and `keep_freed` is on, else 2. Staging: 8; 1 under pressure; 0 under critical commit. | `pool::room`, `pool.rs:44-71` |
| Speculative (picked, not bake) staging may grow a new block only with `residency::allow_staging_growth` | `pool.rs:389-395` |
| A reused staging piece is zeroed so unmapped levels upload zeros | `pool.rs:403-408` |

### 4.4 Uploads ahead on a copy queue (`texture/ahead.rs`)

With `VICTORY_UPLOAD_AHEAD=1` (off in code, on in the launcher), the *first*
upload of a texture picked by index runs on a copy queue of our own. Threads
`uploads ahead N` read, detile and copy it as soon as the texture pass has made
it. The texture must be new: no list has used it, it is in the common state,
not tiled, not aliased, and in a fresh pool block (`claim`, `ahead.rs:96-144`).
At most 96 MiB is in flight (32 MiB on hosts with 32 GiB or less) and 16 MiB or
64 textures per batch. `Job::then` runs the slot flip when the copy has run.
`drain` waits up to 2 s for jobs; bake paths use it.

### 4.5 Bindless slot visibility

`descriptor/host.rs:284-293` (`show_after`, `VICTORY_SHOW_AFTER`, off in code,
on in the launcher): a slot that is given a texture new to it keeps showing the
neutral 1x1 grey texture until the submission holding the upload has finished.
The slot is then made (`Flip`, `texture_slot_show_after`). The reason: three
device losses (n08, n11, n15) were page faults in a texture that had been made
and shown in the last tenth of a second. Lists already on the card read the
slot by index. Related protections:

- `VICTORY_RIPE_BLOCKS`;
- `VICTORY_WAIT_RESIDENT`, on: new memory is created `CREATE_NOT_RESIDENT`, then
  `MakeResident` is called and waited for (`host.rs:210-348`);
- `VICTORY_KEEP_FREED`, on: freed memory is kept while there is room, so a
  stray read hits stale memory instead of a page fault (`host.rs:353-368`);
- `bury_lag`: objects are dropped 64 submissions late, or 16 on hosts with
  32 GiB or less (`host.rs:491-503`).

The slot mechanics belong to the descriptor code; see
[victory-gpu.md](victory-gpu.md).

### 4.6 Levels: held levels and levels on demand

- `VICTORY_HELD_LEVELS` (on, `resource.rs:1231-1245`): a placed texture's host
  copy starts at the first level its views show. Larger levels are another
  texture's memory, or nothing.
- `VICTORY_LEVELS_ON_DEMAND=1` (off in code, on in the launcher; it needs
  tiled-resources tier 2, `tiles.rs:72-108`, and disables held levels): the
  texture is a host `CreateReservedResource`. A level gets 64 KiB tiles from our
  64 MiB heaps (`HEAP_TILES` 1024) through `UpdateTileMappings` on the queue.
  That happens only in a submission-time step of a list that also writes the
  whole level. `Staging::held_levels` decides which levels are held. Views start
  at the first held level (`show_from`). Tiles are not reclaimed while the copy
  lives (`tiles.rs:1-50`). It applies to 2D, single-layer, multi-level textures
  of at least `VICTORY_LEVELS_MIN_KB` (1024).
  - A reserved texture is never cut from a texture-pool block:
    `host_texture_as(.., reserved = true)` skips the pool branch. Until
    2026-11-26 the pool branch ran first and returned a fully allocated piece,
    `Tiled::make` then refused it, and the texture was made the old way, so
    with the pool on (the default) **no texture was ever made level by level**.
    Measured on an RTX 3080 (10 GB) at one spot in the world: before, the
    accounting line said "0 are made level by level" with 2,931 MB of levels
    not mapped; after, about 1,500 to 1,800 such textures alive holding
    roughly 500 MB, with about 3,000 MB not allocated.
  - To check it is working, look for the info line
    `textures with levels on demand: N alive ...` (`tiles.rs`), or
    `VICTORY_CARD_ACCOUNTING=1` in a diagnostics build.
  - Known gap: the pool does not hand back the blocks this empties. In the
    same measurement the pool held 5,376 MB in blocks for about 2,200 MB of
    pooled textures, so the card still read as nearly full.

### 4.7 Sparse (console tiled) memory

The console's sparse textures are **not** host tiled resources. The title maps
physical pool pages into its tiled heap with `CopyPageMappingsBatchX`.
`queue.rs:1151` (`copy_page_mappings`) runs it at once through the runtime's
page mapper (`resource::use_page_mapper` / `use_many_page_mapper`, which is
victory-hle xmem). The mapping changes title memory itself. The watch hooks then
see the remap, and host copies refresh from the new pages. Partly mapped
textures read zeros for holes (`texture.rs:1348-1384`). `inplace.rs:31-32`
records that tile mappings into heaps opened on title memory remove the device.

### 4.8 Texture read-back (`texback.rs`)

Some textures are written by shaders and read by the title's CPU: the ocean
height and displacement maps, which keep boats afloat. For kinds listed in
`VICTORY_TEXTURE_BACK` (`<w>x<h>f<dxgi format>`; unset or `1` means
`256x256f41,256x256f16,64x64f16,128x128f36`; `0` means none, `texback.rs:34`),
a list that writes such a texture copies it to a read-back buffer when it closes
(`texback::record`). Once the list has run, `read_backs` (`host.rs:3847`, on the
relay thread for flights) calls `texback::land`, which tiles the texels into
title memory in the console layout. Tickets are invalidated when the memory is
remapped, replaced or released (`texback.rs:60-125`). The selection is by kind,
not by texture, at about 1 MB per list.

### 4.9 Idle sweep

`host::sweep` (`host.rs:424`) runs once a second before a submission on the
submitting thread. It calls `residency::refresh`, then `pool::room`, the library
trim, `shadow::let_go_idle` (windows), `texture::let_go_idle` (staging) and
`resource::let_go_idle` (host copies of title-filled textures idle for 4 or more
frames and held only by their title texture; `resource.rs:2016`, budget-ordered
when `vram_keep` sets a limit). A swept texture is made again and re-uploaded on
its next use. `VICTORY_SWEEP=0` keeps everything.

---

## 5. Residency and budgets

### 5.1 Host memory (commit charge): `residency.rs`

| Function | Rule |
|---|---|
| `sample_commit` | `GlobalMemoryStatusEx().ullAvailPageFile`, sampled at most every 32 ms (`residency.rs:45`). |
| `refresh` (once a second) | Pressure is entered at 3 GiB or less free physical memory or 4 GiB or less commit, and left only at 5 GiB physical **and** 6 GiB commit or more (`next_pressure`, `residency.rs:98`). Each change is logged. |
| `allocation_error` | `E_OUTOFMEMORY` from an upload, texture or pool allocation sets `RETRY_UPLOAD_AT` 500 ms ahead (`residency.rs:23`). |
| `admit_texture_pass(bake)` | Refused during that window. Otherwise needs commit of at least 256 MiB (bake) or 512 MiB (`has_upload_reserve`). |
| `allow_staging_growth(bake)` | A new 64 MiB staging block needs at least 256 MiB (bake) or 1024 MiB (`has_growth_reserve`). |
| `critical_commit` | Less than 1536 MiB of commit, or inside the OOM window. |
| `prefetch_limit` | Per-frame texture-pass bytes: 16 MiB if critical; 64 MiB under pressure or on a host with 32 GiB or less that has at least 1 GiB of live staging; else the budget unchanged. Bakes and explicit requests bypass it (`draw.rs:2648-2658`). |
| `limited_host_ram` | Total physical memory of 32 GiB or less. It changes `bury_lag` to 16, `ahead` MOST to 32 MiB, and `ram_idle_ms`. |
| `note_staging_deferred` / `take_staging_deferred` | A thread-local flag: `texture::staging` refused because uploads are refused (`texture.rs:1894`). The texture pass in `draw.rs` retries the slot later. |

`host::ram_idle_ms` (`host.rs:374`) sets idle timeouts for system-RAM caches:
5 min with `keep_freed` and at least 12 GiB free, then 10 s, 5 s or 2 s as free
memory falls.

### 5.2 VRAM

| Knob | Rule |
|---|---|
| `resource::room` (`resource.rs:1676`) | New host copies of picked textures are always allowed up to `texture_floor_bytes` (2 GiB or budget/4). Beyond that, (VRAM used − free pool texture room + `card_room_kept` + `targets_room_kept`) must be below `room_percent` (90%) of the budget. `VICTORY_TEXTURE_BUDGET_MB` replaces the rule with a byte cap on title-filled copies. With no budget reported the limit is 4 GiB. |
| `targets_room_kept` | `VICTORY_CARD_ROOM_FOR_TARGETS_MB` (3072, at most 8192) minus the bytes of render targets and other host-written textures. |
| `shadow::card_room_kept` | Room reserved for card copies not yet made: the `card_limits` cap, at most 1 GiB (`SEEN`), minus what was made or refused for VRAM room (`CARD_REFUSED`; copies refused by the cap or the small share give up nothing). Since 2026-11-26 the cap of a ≤ 12 GiB budget is above 1 GiB from about 7 GiB of budget, so 1 GiB is held back there (512 MiB before), the same as in the measured-fine run with `VICTORY_CARD_BUFFERS_MB=1280`. |
| `card_room_for` | Card copies of buffers allowed up to 80% (or 97%) of the budget, plus the `card_limits` cap and, on ≤ 12 GiB budgets with the default cap, the small copies' share (§3.5, `card_admit`). |
| `pool::room` | Spare blocks kept below 85% use. |
| `host::video_memory` (`host.rs:9375`) | The DXGI budget and usage source, `[budget, used, ..]`. |

Diagnostics: `VICTORY_CARD_ACCOUNTING=1` (`draw.rs:2432`, line text
`resource::card_accounting`) and `VICTORY_MEMORY_MAP` (`copystat.rs:532`, on in
diagnostics builds) list where the memory is.

---

## 6. Threads in this half

| Thread | What it runs here |
|---|---|
| Title threads | Write title memory. Take guard faults (`watch::install` handler, `titlewrite::store`). Call the runtime's xmem hooks. |
| Recording threads (title, or `victory-list-N` with `VICTORY_THIN`) | `Shadow::refresh` / `gpu_writes`, `texture::upload`, which only defer steps into the `HostList`. |
| Preparing thread (`victory-submit` with async submit; see the command-half doc) | Deferred refreshes, `Staging::refresh`, `Shadow::writing` (snapshots), `card_before`, the sweep, `stale_under_flights` merges. |
| `victory-wide-N` (`shadow.rs:1684`) | Snapshot copies (`take_snapshots`) and compares (`find_all`) in 1 MiB jobs (`VICTORY_WIDE_BACK`). |
| `victory-relay` | Landing (§3.6). It never takes the submission lock. |
| `victory-land-N`, `victory-under-N`, `victory-views-N` / `victory-records-N` | `land.rs` pools: merges of pieces, merges under shader writes, view scans. |
| `victory-late-wait` | Settles flights when the relay is off. Resets landed lists (`land()`) and calls `queue::release_held`. |
| `uploads ahead N` | `texture/ahead.rs` copy-queue uploads. |
| `victory-ask-early` | Early write-watch askings (`VICTORY_ASK_EARLY`). |
| `victory-aside` | The heap buffer-view pass for a list (`host/aside.rs`, `VICTORY_HEAP_PASS_ASIDE`). |

---

## 7. Replay (`replay/`, `tests/replay.rs`)

Replay is only in diagnostics builds (`lib.rs`: `#[cfg(all(windows, feature = "diagnostics"))] pub mod replay`).

### 7.1 Recording

`replay::record::from_env(&device)` is called right after `D3D12CreateDevice`
(`host.rs:908-910`) when `VICTORY_RECORD` is set. `hook.rs` replaces the method
tables of single host COM objects with patched copies (`Shim`, 512 slots), so
other objects of the same class are untouched. The recorder (`record.rs`) keeps
object creation parameters, descriptor slot contents and texture states
(`states.rs`) from creation onward. It records lists only while armed: a few
frames before the capture, or always with `VICTORY_RECORD_ALWAYS=1`. Our own
calls that must not be recorded run under `record::unrecorded()`.

`snap.rs` writes the frame:

- At the frame's first submission the card is drained and its memory copied out
  through 4 read-back buffers of 256 MiB each.
- Before each later submission, the CPU-changed 4 KiB pages of what that
  submission reads are written as deltas.
- The title stands still during the capture.

Capture folder `<VICTORY_RECORD>/capture-<n>/`:

| File | Content |
|---|---|
| `frame.bin` | `b"VICFRAME"`, `u32` version (`wire::VERSION` = 1), then a `wire::Frame`. All fields are little-endian and in order, with no cross-version compatibility (`replay/wire.rs:1-8`). `Frame` holds the adapter, vendor, device, `notes`, heaps, resources, roots, codes, pipelines, signatures, descriptor heaps, query heaps, lists (`List { kind, commands: Blob }`), submits (`Submit { frame, lists, deltas, views }`), contents and presents (`wire.rs:559-577`). |
| `contents.bin` | The frame-start contents (`Content`: buffer bytes, or texture subresources in `GetCopyableFootprints` layout; `ZERO` when all zero). |
| `deltas.bin` | `Delta { res, at, len, file }`: bytes the CPU put in CPU-visible buffers before each submission. |
| `shaders.bin` | Shader code, once each (`Code { at, len, hash: blake3 }`). |
| `present.bin` | The presented picture. |
| `notes.txt` / `failed.txt` | What could not be recorded, or why the capture failed. |

### 7.2 Playing

`victory replay <folder> [...]` (`apps/victory/src/replay.rs`, through
`victory_hle::replay::play`) calls `play::replay(folder, &Options)`
(`replay/play.rs:455`). `play::find` takes the folder itself, or its
highest-numbered `capture-<n>` that has a `frame.bin`. Each loop restores what
the previous loop changed, then sends the frame without waiting between
submissions. Card timestamps give the report. `play/merge.rs` keeps card-written
words while applying the CPU deltas (`--no-merge` turns that off). `edit.rs`
holds the experiments. `play/{diverge,pixel,look,upcopies}.rs` are the analysis
modes. The flag reference in `docs/replay.md` matches the CLI.

### 7.3 Uses and limits

Uses: per-pass card times, A/B tests of shader variants (`--shaders`,
`--compare`), barrier and clamp experiments, "what drew this pixel", divergence
hunting, and checking undeclared writes (`--upcopies`).

Limits, as listed in `docs/replay.md`: no reserved resources, bundles, ray
tracing, render passes or enhanced barriers. Multisampled texture contents and
texture levels over 256 MiB are not kept (verified in `snap.rs:485-515`).
Addresses inside indirect arguments are not rewritten. Recording a world frame
can trip the title's own watchdog (README).

### 7.4 Tests

- `tests/replay.rs` (`a_frame_is_recorded_and_replayed`): a small D3D12 program
  with clears, indirect draws of textures picked from a heap, mesh shaders,
  compute, CPU changes between submissions, replaced descriptors and aliased
  textures. It runs without the recorder, then with it, then replays, and every
  picture and buffer must match. It needs a hardware or WARP adapter and dxc for
  mesh shaders. `VICTORY_TEST_D3D_DEBUG=1` adds the debug layer.
- `tests/replay_big.rs` (`#[ignore]`): frame-sized stress (17k textures,
  1.5M-slot heap, 30 submits, 40k calls). Tuned with `BIG_TEXTURES`, `BIG_CARD`
  and `BIG_FOLDER`.

---

## 8. Environment switches (this half)

Format: **name**, read at `file:line`, default, then the effect. "on" means on
unless set to `0` (the `toggled!` macro, `switch.rs:19-43`, which also allows a
runtime A/B through `VICTORY_AB` in diagnostics builds). "off" means it needs
`=1` unless stated otherwise. **L** marks a switch the `Play-VIctory.cmd`
launcher sets, with the value it sets.

### Coherence of buffers and the relay

| Switch | At | Default | Effect |
|---|---|---|---|
| `VICTORY_LATE_WAIT` | host.rs:2384 | 3 if the relay is on, else 0 (0 with `VICTORY_GPU_TIMING`) | Flights: 1 = sent without a wait, 2 = exact merges under flight writes, 3 = present no longer settles. |
| `VICTORY_RELAY` | shadow.rs:1107, host.rs:2484 | stage 2 (off in `cfg(test)`); **L**=2 | 0 off, 1 the relay thread, 2 also exact waits per declared range. Off when snapshots are kept. |
| `VICTORY_RELAY_COPY` | host.rs:2505 | off; **L**=1 | Relay fetches go over a copy queue. |
| `VICTORY_FETCH_CHECK` | host/torn.rs:35 | off | Hash fetched pieces to catch torn fetches. |
| `VICTORY_CARD_STEPS` | host/card_steps.rs:79 | 0; **L**=2 | 1 fills, 2 fills and copies on the card (§3.7). |
| `VICTORY_PLACED_ORDER` | host/card_steps.rs `placed_order` | off | Cut a submission behind an in-place copy instead of refusing card steps. |
| `VICTORY_PLACED_FORWARD` | host/card_steps.rs `placed_forward` | on | A fill behind an in-place copy that reads only its bytes is done on the card from the copy's source (§3.7); `0`: refused, the submission waits. |
| `VICTORY_CARD_STEPS_CHECK` | host/card_steps.rs `check` | off | `1`: what those fills were given is read back and compared with title memory after the landing (a warning per frame line if any differ). |
| `VICTORY_HOLDER_INDEX` | host/card_steps.rs `holder_index` | on | What holds a card step's bytes is looked up by pieces of memory; `0`: over every declared range, per fill. |
| `VICTORY_COPIES_IN_PLACE` | host/place.rs:43 | on | Title buffer copies on the card at their list position. |
| `VICTORY_FILL_TITLE` | host.rs:2809 | off | An experiment named in the card-steps docs. |
| `VICTORY_JOIN_COPIES` | host.rs:3773 | off; **L**=1 | Join adjacent title copies into one kernel copy. |
| `VICTORY_CARD_REHASH_RUNS` | host.rs:2147 | off; **L**=1 | Rehash card pieces in joined runs. |
| `VICTORY_PICKED_WRITES` / `_MB` | picked.rs:60,76 | 0 | 1 declare picked-view writes, 2 also land them. |
| `VICTORY_UPCOPY_CHECK` | host/upcheck.rs:33 | off | Check that sends never overwrite newer card bytes (costly). |
| `VICTORY_HEAP_PASS_ASIDE` | host/aside.rs:47 | on | The heap buffer-view pass runs on `victory-aside`. |
| `VICTORY_CARD_BUFFERS` | shadow.rs:169 | on | Two-sided card copies of big buffers. |
| `VICTORY_CARD_BUFFERS_MIN_MB` / `_MB` | shadow.rs:291-292 | set by the VRAM budget (§3.5 table; ≤ 12 GiB: 16 MiB / max(512, budget/7) MiB) | Least size and total cap for card copies. Setting `_MB` also removes the small copies' share. |
| `VICTORY_CARD_BUFFERS_MIN_KB` | shadow.rs:300 | unset; **L**=64 | Least size in KiB (overrides `_MIN_MB`). Copies under the tier's default least size then share at most 3/4 of the default cap on ≤ 12 GiB budgets. |
| `VICTORY_CARD_ALL` | shadow.rs:511 | off | Every buffer gets two sides (small ones in PC memory). |
| `VICTORY_CARD_ROOM_FOR_BUFFERS` | shadow.rs:415 | on | Reserve VRAM for card copies. The doc at shadow.rs:386 says "off", which is stale; the code and shadow.rs:404 say on. |
| `VICTORY_CARD_HASH_LEN` | shadow.rs:4437 | on | Hash only the title buffer's own bytes, not the 64 KiB-rounded buffer. |
| `VICTORY_CARD_TOUCHED` | shadow.rs `touched_list` | on | Submissions take only the card copies whose CPU side changed (§3.5); `0`: look at all of them, as before 2026-11-27. `VICTORY_AB`-switchable. |
| `VICTORY_CARD_WINDOWS_MB` | shadow.rs:635 | 0 | Cap on windows with a VRAM side. |
| `VICTORY_WINDOWS_BAR_MB` | shadow.rs:673 | 0 | Cap on windows in `GPU_UPLOAD` (BAR) heaps. |
| `VICTORY_WINDOWS_OWN` | shadow.rs:711 | off | Two sides for windows. |
| `VICTORY_WINDOW_KB` | shadow.rs:5240 | 4096 grid | Window grid (1 MiB or 256 KiB experiments). |
| `VICTORY_WINDOW_TAILS` | shadow.rs:5279 | off | Windows that run to the cell end. |
| `VICTORY_MEMORY_CARD_MB` | shadow.rs:5384 | 4096 | VRAM cap for raw-address arena pages. |
| `VICTORY_SNAPSHOT_POOL` | shadow.rs:1472 | 1024 | Snapshot buffers kept for reuse. |
| `VICTORY_WIDE_BACK` | shadow.rs:1638 | on; **L**=1 | Snapshots and compares on `victory-wide-N`. |
| `VICTORY_WIDE_SPIN` | shadow.rs:1657 | 0 µs | Spin before a wide worker sleeps. |
| `VICTORY_EXACT_BACK` | shadow.rs:2267 | on | Write back dword runs, not first-to-last byte. Needed by `VICTORY_LAND`. |
| `VICTORY_SEND_CHANGED` | shadow.rs:2247 | off (`!=0` turns it on) | Send only changed dwords to card copies. |
| `VICTORY_LAND_ECHO` | shadow.rs:2204 | on (`play.ps1` sets 0) | Compare landed 4 KiB pages before resending. |
| `VICTORY_UNDER_WIDE` | shadow.rs:2351 | on | `merge_under_writes` on several threads. |
| `VICTORY_KEEP_SNAPSHOTS` / `_MB` | shadow.rs:2713,2543 | off / 256 | Keep snapshots between submissions (disables the relay). |
| `VICTORY_REFRESH_CHECK` | shadow.rs:3978 | off | Sample 256 B of each refreshed block to find unseen writes. |
| `VICTORY_QUICK_FIND` | shadow.rs:4971 | on | 1024-entry lookup cache. |
| `VICTORY_LAND` | land.rs:46 | on (needs `SAFE_COPY` and `EXACT_BACK`) | Merge landings on the land pool. |
| `VICTORY_LAND_THREADS` | land.rs:54 | cores/4, from 1 to 4 (at most 16) | Land pool size. |
| `VICTORY_LAND_BESIDE` | land.rs:62 | on | Land through our own view of physical pages. |
| `VICTORY_LAND_ANYTIME` | land.rs:475 | on | Land beside even during a remap. |
| `VICTORY_LAND_TAKEN` | land.rs:511 | on | Do not re-take blocks that one copy landed. |
| `VICTORY_LAND_SKIP` | land.rs:69 | off | Skip pieces an earlier fetch already holds. |
| `VICTORY_UNDER_THREADS` / `VICTORY_VIEW_THREADS` | land.rs:336,351 | 6 / 6 or 4 | Helper pool sizes. |
| `VICTORY_INPLACE` / `_MB` | inplace.rs:89,97 | 0 / 4096 | 1 windows, 2 buffers too, opened in place. Do not use 2 with late wait. |
| `VICTORY_INPLACE_PROBE` / `_MB` | inplace.rs:62,1236 | 0 / 64 | The driver probe and census. |
| `VICTORY_UPLOAD_LOW` | upload.rs:42 | on | Upload memory below 2^37. |
| `VICTORY_BVH_SIZES_WAIT`, `VICTORY_BVH_TRACE` | memory.rs:819,1091 | off | BVH diagnostics. |
| `VICTORY_MEMORY_DUMP` | memory.rs:1197 | unset | A folder for raw-memory dumps. |

### Write detection

| Switch | At | Default | Effect |
|---|---|---|---|
| `VICTORY_VIEW_GUARD` | watch.rs:1809 | on | Guard all views of a page together. |
| `VICTORY_OPEN_ONE_VIEW` | watch.rs:1326 | on | A fault opens only the view that was written. |
| `VICTORY_GUARD_REST` | watch.rs:1348,1352 | 3000 µs | Rest time of a block just opened by a fault; 0 means none. |
| `VICTORY_GUARD_ASK` | watch.rs:1646 | off | Always ask Windows whether a range is a view. |
| `VICTORY_GUARD_HOLES` | watch.rs:1762 | on | Classify ranges that start in holes correctly. |
| `VICTORY_KNOWN_HOLES` | watch.rs:1070 | on (with light refresh) | Cache placeholder holes until the runtime reports a change. |
| `VICTORY_LIGHT_REFRESH` | watch.rs:826 | on | Answer guarded ranges from bits, without the guard lock. |
| `VICTORY_PAGE_PARTS` | watch.rs:54 | on | Track 4 KiB parts. |
| `VICTORY_WATCHED_HEAP` | watched.rs `on` | on | Blocks of 64 KiB or more of host memory handed to the title (its allocator's, device heaps) are write-watched reservations (§3.3); `0`: the process heap, copied whole at every refresh. |
| `VICTORY_TAKE_PARTS` | watch.rs:787 | on | Re-take only the parts written. |
| `VICTORY_ASK_ONCE` | watch.rs:524 | on | One write-watch asking per submission. |
| `VICTORY_ASK_REFUSED` | watch.rs `ask_refused_again` | on (remembered) | `0`: ask the kernel again about ranges it refused, every time, as before 2026-11-27 (§3.3). `VICTORY_AB`-switchable. |
| `VICTORY_ASK_EARLY` | watch.rs:403 | unset = off; a value = every N ms (2 if it does not parse) | Asks while the title records. |
| `VICTORY_PAGE_WRITES` | watch.rs:2116 | `guards` (bit 1) | What to do with DirectStorage page writes: `0`, `1`, `slots`, `all`. |
| `VICTORY_SAFE_COPY` | safecopy.rs:325 | on | `rep movsb` with fault recovery instead of kernel copies. Also installs the handler. |
| `VICTORY_COPY_BESIDE` / `VICTORY_STORE_BESIDE` | titlewrite.rs:70,78 | off | Title `memcpy`/`memset` and faulting stores done beside the guards. |
| `VICTORY_SAME_RECORDS` | titlewrite.rs:185 | on | A titlewrite detail; not analysed further. |
| `VICTORY_WRITE_SITES` | titlewrite.rs:273 | unset | A file for fault-site statistics. |
| `VICTORY_FAULT_BOOK` / `_AT` | faultbook.rs:87-88 | unset / `150,210,270` | Binary log of write faults (diagnostics). |
| `VICTORY_WRITERS` / `_OF` | writers.rs `Config::from_switches` | unset | The writers watch (§3.4a, diagnostics builds): `<hex address>+<hex length>,...`; `<shader hash>:s<slot>+<offset>,...`. |
| `VICTORY_WRITERS_FROM` / `_FOR` | writers.rs `Config::from_switches` | 0 / to the end | Its window, seconds after the title starts. |
| `VICTORY_WRITERS_DETAIL` / `_STACK` | writers.rs `Config::from_switches` | 64 / 12 (at most 32) | Writes per (instruction, offset) with a stack; frames per stack. |

### Textures

| Switch | At | Default | Effect |
|---|---|---|---|
| `VICTORY_LAZY_TEXTURES` | resource.rs:1487 | on | Host copies made on first use. |
| `VICTORY_HELD_LEVELS` | resource.rs:1243 | on (off with levels on demand) | Copies start at the first viewed level. |
| `VICTORY_LEVELS_ON_DEMAND` | tiles.rs:81 | off; **L**=1 | Reserved-resource textures, given tiles per level. |
| `VICTORY_LEVELS_MIN_KB` | tiles.rs:120 | 1024 (at least 64) | Size floor for that. |
| `VICTORY_DEPTH_TWINS` | resource.rs:463,474,550 | on; `stencil`, `nodepth` variants | Colour twins for depth UAV textures. |
| `VICTORY_ALIAS_COPY` | resource.rs:1421,1427 | on; `1` = single-level only | Copy through `Through` buffers into views the title placed in another format. |
| `VICTORY_PLACED_FILLER` | resource.rs:3039 | off | Keep the filler words of a description in the placed-texture key. |
| `VICTORY_TEXTURE_GUARD` | texture.rs:1302 | on | Guard bits for view-memory textures, hashing every 8th frame. |
| `VICTORY_TEXTURE_HOT` | texture.rs:133 | on | Hash every check for 64 frames when hot. |
| `VICTORY_TEXTURE_NO_WAIT` | texture.rs:376 | off; **L**=1 | Do not wait for remaps; read through the kernel. |
| `VICTORY_TEXTURE_FILL` | texture.rs:394 | on | First fill of what is there during a remap. |
| `VICTORY_TEXTURE_COPY` | texture.rs:458 | off | Read textures through the kernel always (a device-loss experiment). |
| `VICTORY_TEXTURE_ONCE` | texture.rs:677 | on | One-shot staging for picked textures without asking the watch. |
| `VICTORY_TEXTURE_SAME` | texture.rs:1620 | on | Skip re-uploads of byte-identical memory. |
| `VICTORY_COMMITTED_QUERY` | texture.rs:1697 | off | Use `VirtualQuery` walks to ask whether memory is committed. |
| `VICTORY_MAPPED_BY_VIEWS` / `_BY_PAGE` | texture.rs:1782,1792 | on / off | How mapped bytes are counted. |
| `VICTORY_TEXTURE_POOL` | pool.rs:84 | on | Pooled 64 MiB blocks. |
| `VICTORY_RIPE_BLOCKS` | pool.rs:101 | 12 | Blocks made ahead. |
| `VICTORY_UPLOAD_AHEAD` | texture/ahead.rs:46 | off; **L**=1 | First uploads on our copy queue. |
| `VICTORY_SHOW_AFTER` | descriptor/host.rs:292 | off (`!=0` turns it on); **L**=1 | Slots show a new texture after its upload. |
| `VICTORY_TEXTURE_BACK` | texback.rs:34 | 4 water kinds | Texture read-back kinds; `0` turns it off. |
| `VICTORY_TEXTURE_CATCHUP` | draw.rs:2635 | on | Triple the texture-pass budget for a backlog (command half). |
| `VICTORY_BENCH_FILE`, `VICTORY_TILES_TEST_CARD` | texture.rs:2225, tiles.rs:800 | unset | For tests and benches only. |

### Residency and lifetime

| Switch | At | Default | Effect |
|---|---|---|---|
| `VICTORY_TEXTURE_BUDGET_MB` | resource.rs:1681 | unset | Byte cap instead of the VRAM percentage rule. |
| `VICTORY_TEXTURE_ROOM_PERCENT` | resource.rs:1768 | 90 (50 to 97) | Share of the VRAM budget for picked-texture copies. |
| `VICTORY_CARD_ROOM_FOR_TARGETS_MB` | resource.rs:1729 | 3072 | VRAM reserved for render targets. |
| `VICTORY_TEXTURE_SWEEP` | resource.rs:2020,2070 | on; `whole` = whole table each look | Let idle host copies go. |
| `VICTORY_TEXTURE_KEEP` | resource.rs:2037 | keep (any value except `0` or `1`) | `0` or `1` disables the budget-ordered keep and uses `vram_idle_ms`. It is odd that `1` also disables it. |
| `VICTORY_CARD_ACCOUNTING` | draw.rs:2432 | off | The VRAM accounting line. |
| `VICTORY_SWEEP` | host.rs:428 | on | The once-a-second sweep. |
| `VICTORY_KEEP_FREED` | host.rs:367 | on | Keep freed memory while there is room. |
| `VICTORY_BURY_LAG` | host.rs:501 | 64, or 16 with 32 GiB RAM or less | Submissions of delay before freeing. |
| `VICTORY_WAIT_RESIDENT` | host.rs:220 | on | `CREATE_NOT_RESIDENT` with a `MakeResident` wait. |
| `VICTORY_MEMORY_MAP` | copystat.rs:532 | on (diagnostics) | The memory line. |
| `VICTORY_SLOT_CHECK`, `VICTORY_BOOK` | book.rs:86, book.rs:18 | off / on with DRED | The address book of VRAM owners. |

### Replay and diagnostics

| Switch | At | Default | Effect |
|---|---|---|---|
| `VICTORY_RECORD` | replay/record.rs:61 | unset | The capture folder; turns the recorder on. |
| `VICTORY_RECORD_AT` | replay/record.rs:62 | trigger file only | Seconds after the device was made, or `f<presents>`. |
| `VICTORY_RECORD_FRAMES` | replay/record.rs:63 | 1 | Frames per capture. |
| `VICTORY_RECORD_ALWAYS` | replay/record.rs:67 | off | Record lists from the start. |
| `VICTORY_REPLAY_GBV` | replay/play.rs:3422 | unset | GPU-based validation in replay. |
| `VICTORY_REPLAY_PEEK` | replay/play/diverge.rs:975 | unset | Extra diverge output. |
| `VICTORY_TEST_D3D_DEBUG` | tests/replay.rs:1598 | unset | The debug layer in the replay test. |
| `VICTORY_POP_WATCH` / `_FOLDER` / `_ALL` | popwatch.rs:46,295,314 | unset | Pop and flicker detector on presented frames. |

---

## 9. Invariants, hazards and surprises

**Invariants**

1. A declared range is never refreshed from title memory until every flight
   that declared it has landed (`pending` + `Flights`).
2. Landing writes only bytes the card changed (`merge_piece`,
   `safecopy::merge`). Title bytes the card did not touch keep the title's
   value. A byte both sides wrote is the shader's if the shader wrote it later
   (`host.rs:2409-2413`). This is a race on the console too.
3. Flights land in order, and fences and bottom-of-pipe writes come after their
   flight's landing (§3.6).
4. Pool pieces are never reused within 2500 ms or while a list names them.
   Pieces come from ripe blocks.
5. Objects let go are dropped only `bury_lag` submissions after the last one
   that may use them (`retire`, `bury`, `host.rs:462-519`).
6. `ASKING` is taken before the journal lock and never under it. `merging` is
   taken before `before`. The fault handler must never block on a lock that a
   thread mid-copy of guarded memory holds. *(inferred: this is why it uses
   lock-free bitmaps and `try`-style retries.)*
7. An in-place read of title memory holds `title_memory_reading`. It must not
   be taken twice by one thread, nor held across anything that maps memory
   (`watch.rs:2414-2416`).

**Unsafe hot spots**

- `watch::install::on_write`: it rewrites `Rip` (`fail`) and emulates title
  stores (`titlewrite::store`, decoding x86 in `titlewrite/decode.rs`).
- `writers::on_exception`: sets and clears the trap flag of title threads,
  holds per-thread step state across two exceptions, and changes 4 KiB page
  protections inside the guards' 64 KiB blocks. Its ordering rules: a slot's
  `stepping`/`kernel` count goes up before the guards open a block (so
  `reguard` leaves the page writable), the page is made writable under the
  guards' lock (no refresh can make the block clean in between), and a page
  made read-only at the end of a step re-checks `live` and is given back
  (`watch::writer_retired`) if the watch let it go meanwhile. The handler
  takes the guards' lock and then the watch's state lock, never both at
  once.
- `safecopy.rs`: hand-written copy and merge routines whose faults are recovered
  by the handler.
- `Shadow::data` raw mappings (`unsafe impl Send/Sync` on `Staging`,
  `texture.rs:136-140`).
- `pool::Upload` mapped pointers, and the `Reclaim` COM-like object attached to
  host textures (`pool.rs:412`).
- The replay method-table shims (`replay/hook.rs`).
- `host::references`, which calls `AddRef` and `Release` to read the reference
  count (`host.rs:407`).

**Known device-loss causes** (from code comments and docs):

- A page fault in a texture made and shown within about 100 ms while earlier
  lists read the slot by index. The mitigations are `VICTORY_SHOW_AFTER`, ripe
  blocks, `VICTORY_WAIT_RESIDENT`, `KEEP_FREED` and `bury_lag`. The root cause
  is unknown (`pool.rs:91-98`).
- Reads past a view on AMD into memory that was just freed (`host.rs:353-364`).
- Tile mappings into heaps opened on title memory (`inplace.rs:31-32`).
- Device removal is checked in `relay_land` (`host.rs:4862`). `fail` adds DRED
  and address-book data when `VICTORY_D3D12_DRED` is set (`host.rs:5908`,
  `book.rs`). `tdr.rs` logs the TDR registry settings.

**Stale comments and surprising defaults**

- The defaults of `VICTORY_RELAY` and `VICTORY_LATE_WAIT` are now on (stage 2
  and stage 3). Their doc comments still say "off by default" (`shadow.rs:1084-1086`,
  `host.rs:2326-2327`, `host.rs:2436`).
- `VICTORY_INPLACE=2` is therefore unsafe by default (`inplace.rs:74-78`).
- `VICTORY_CARD_ROOM_FOR_BUFFERS` contradicts itself in its docs
  (`shadow.rs:386` against `shadow.rs:404`).
- Several behaviours the README presents as features of the fork are off in the
  code and on only through `Play-VIctory.cmd`: `SHOW_AFTER`, `UPLOAD_AHEAD`,
  `CARD_STEPS`, `RELAY_COPY` and `LEVELS_ON_DEMAND`. A plain
  `victory run` does not get them.
- In-crate tests run with the relay off (`cfg(test)`, `shadow.rs:1104-1107`), so
  unit tests do not exercise the default landing path. Tests that need it enable
  it themselves *(inferred)*.

---

## 10. How to debug

| Symptom | Steps |
|---|---|
| **Stale texture** (old picture, a mip missing) | 1. Is it in view memory written through another view? Try `VICTORY_TEXTURE_HOT`, and `VICTORY_TEXTURE_GUARD=0` (hash every check). 2. Is it partly mapped? It is re-read at most every 30 frames (`texture.rs:1353-1359`). 3. With `VICTORY_LEVELS_ON_DEMAND` or `HELD_LEVELS`, check the `tiles` STATS line, and that `levels_waiting` retries once a second. 4. Texture changed but uploaded a frame late: expected (`texture.rs:819-823`). |
| **Stale buffer data** | `VICTORY_REFRESH_CHECK=1` names blocks changed with no write seen. `VICTORY_UPCOPY_CHECK=1` names sends that overwrote newer card bytes. `VICTORY_PAGE_WRITES=all` for DirectStorage writes. Try `VICTORY_VIEW_GUARD=0` or `VICTORY_GUARD_REST=0`. |
| **Missing upload / black or grey slot** | Grey means the neutral texture: the upload was not run yet (`SHOW_AFTER`) or staging was refused. Check the log for "host memory pressure" and `E_OUTOFMEMORY` (residency). Check `resource::room` (`VICTORY_CARD_ACCOUNTING=1`) and refused card copies ("no card copy for a title buffer"). Try `VICTORY_TEXTURE_BUDGET_MB`, or `VICTORY_TEXTURE_ROOM_PERCENT=95`. |
| **Flicker / pops** | `VICTORY_POP_WATCH=<from>-<to>` with `VICTORY_POP_FOLDER` saves frames around blips. A/B a suspect switch at runtime with `VICTORY_AB` (diagnostics). For outdoor flicker see `VICTORY_INDIRECT_REST` (command half). |
| **Lost GPU writes in title memory** (physics wrong, objects vanish) | Is the writer's view declared? Try `VICTORY_PICKED_WRITES=2`. Is it a texture? Check its kind against `VICTORY_TEXTURE_BACK`. Run `victory replay --upcopies` on a capture. |
| **Device removed** | Diagnostics build with `VICTORY_D3D12_DRED=light` (or `faults`) plus the address book. Then `VICTORY_SHOW_AFTER=1`, `VICTORY_RIPE_BLOCKS`, `VICTORY_WAIT_RESIDENT`, `VICTORY_KEEP_FREED`, `VICTORY_BURY_LAG`. Reproduce on a capture with `--debug-layer` or `VICTORY_REPLAY_GBV`. `VICTORY_SLOT_CHECK=1` finds slots that name freed memory before a loss. |
| **Out of memory** | The `residency::refresh` warning line gives free physical memory, commit, mirrors and staging. Pool growth backs off 500 ms after a failure. Lower `VICTORY_SNAPSHOT_POOL` or `VICTORY_CARD_BUFFERS_MB`. `VICTORY_MEMORY_MAP=1` lists the big allocations. |
| **Title memory holds the wrong values** (constants differ between launches) | Diagnostics build: `VICTORY_WRITERS=<address>+<len>` or `VICTORY_WRITERS_OF=<hash>:s<slot>+<offset>` with a `_FROM`/`_FOR` window (§3.4a) names each writing instruction as `module+rva` with values and a stack; `victory code-bytes <image> <rva> <len>` writes that code for a disassembler. |
| **Frame stalls waiting** | The frame line's `waiting for landings: ...` spans name the reason (`relay_wait`, `host.rs:3360-3367`). Try `VICTORY_CARD_STEPS=2` and `VICTORY_RELAY_COPY=1`. |

---

## 11. Tests in this half

Do not run them while another build holds the target directory.

- `cargo test -p victory-gpu` covers the unit tests in the files below. Many
  need Windows and some need a D3D12 adapter; WARP is used where noted.
- `cargo test -p victory-gpu --features diagnostics` adds the `replay/*` unit
  tests and `tests/replay.rs`.
- `-- --ignored` runs the cost benches and `tests/replay_big.rs`. Use
  `--release --nocapture` for meaningful numbers.

| File | Tests (ignored) | Covers |
|---|---|---|
| `watched.rs` | 3 | A write-watched block is zeroed and aligned, the kernel answers for its whole 64 KiB pages (uncommitted ones too), the journal tells its writes to every asker, a `ReadFile` into it is seen. |
| `watch.rs` | 43 | The look at the journal by runs of 64 pages answers as page by page does. Refusals remembered until the address space changes, and view memory refused before missing no write (through the other view, twice, and a kernel read). The journal is shared by askers. Kernel reads into guarded blocks, and kept past their call. Views and aliases (`a_write_through_another_view_is_reported_at_the_guarded_one`). Rest. Holes. Remap races (`no_write_is_lost_between_askers`, `no_mapping_is_lost_between_askers`). |
| `writers.rs` | 12 | The writers watch on memory of the test's own: a write is told with its instruction, offset, size, value and count and is done; a write beside the range is counted only; a string store across two pages; a write through another view; a kernel read (`ReadFile`) refused while watched and landing under `kernel_writing`, told as the kernel's; the guards still report their block written; detail stops while counting goes on; the window arms and disarms; a range mapped after the window opened is watched then; a binding found by a draw is watched; switch parsing. |
| `shadow.rs` | 34 (1) | The touched list takes a changed card copy once (changed before and after registering, many times). Card piece arithmetic against `CARD_COMPARE`. `Flights`. `merge_piece`. Dword runs. Landings against title bytes. `card_limits`. Windows. |
| `land/tests.rs` | 8 (1) | Pool order and panics, merges, landing beside, waits for remaps. |
| `safecopy.rs` | 8 (1) | Every length and alignment. Only changed dwords are put. Faults become failures. |
| `pool.rs` | 7 | Ranges, spares, growth back-off, rest, ripe blocks, reclaim. |
| `residency.rs` | 3 | Reserves, pressure hysteresis, prefetch limits. |
| `host/card_steps.rs` | 9 (1) | Planning and refusal rules, the order behind flights. |
| `host/upcheck.rs` | 11 (10 ignored: GPU) | Undeclared writes, copy order. |
| `host/torn.rs`, `host/place.rs` | 3 (1), 1 (1) | Torn fetches, copies in place. |
| `tiles.rs` | 4 (1) | Levels on demand on WARP, or the card with `VICTORY_TILES_TEST_CARD=1`. |
| `texture.rs`, `texture/ahead.rs` | 6 (3), 2 | Uncommitted tails read as zeros, digest, detiling. Cost benches are ignored. |
| `resource.rs`, `inplace.rs`, `memory.rs`, `upload.rs`, `texback.rs`, `titlewrite*.rs`, `popwatch.rs` | 13, 7, 2, 2, 1, 5, 3 | Depth twin copies, in-place rules, the directory, arenas, kinds, x86 store decode. |
| `replay/*.rs` | about 25 | Wire round-trips (truncation refused), states, hooks, edits, `find`. |
| `tests/replay.rs` | 1 | End-to-end record and replay, compared byte for byte. |
| `tests/replay_big.rs` | 1 (ignored) | Frame-sized cost. |

Related tests in the command half: `indirect::tests::new_blocks_under_readers`,
`indirect::tests::raw_loads_past_views` and
`indirect::tests::descriptors_written_under_readers` probe the device-loss
hypotheses named above.

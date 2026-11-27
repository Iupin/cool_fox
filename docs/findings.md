# What was learned on the RX 7900 XTX (2026-11-23 to 2026-11-25)

One PC: i9-12900K (8 fast cores, 8 slow ones, 24 threads), 64 GB, Radeon RX 7900 XTX
24 GB, Windows 11, 1440p, Series X profile. Figures are of diagnostics builds with a
screenshot a second. Scenes are not the same from run to run, so read differences
under about 5 % as nothing. What a switch does is in the comment where it is read.

## Where a frame goes

Median of whole runs, same part of the prologue: 280 ms, then 124, 103, 75, now 46-65.
On the water (first boat mission) 46-53 ms.

The title needs 33 ms. Of a 55 ms frame on the water:

- **The thread that prepares submissions: 48 ms.** It is the wall. Steps of the lists
  18 (refreshes of host copies 10), steps done on the card 7, waiting for descriptor
  records 6, asking the write watch 5, copies in the card's memory 4, the title's bytes
  under shader writes 4.
- **The card: about 41 ms busy**, the next wall.
- **List calls: 36,000 a frame, 230 ms of work** spread over eight threads. In a street
  chase 67,000 calls and 430 ms.
- Descriptor records copied as plain bytes: 400,000 slots looked at, 40,000 mirrored a
  frame, most of them empty.
- Landing what shaders wrote: 16 MB a frame put as dwords, on a thread of its own.

## The one idea behind most of it

On the console the CPU and the graphics processor share memory. Here they do not, so
everything one writes and the other reads must be noticed and moved. Every large cost
is one of four: **noticing** writes of the title (write watch, guards), **noticing**
writes of shaders (hashes on the card, compares on the CPU), **moving** them, and
**waiting** until an order is safe. Gains came from making each of them narrower
(dwords, not pages), wider (threads, AVX2) or later (on the card, behind the frame),
in that order of size:

| What | Frame before | after |
|---|---|---|
| Descriptor records of draws' tables left to the draws | 280 ms | about 124 |
| Our hash passes over card copies, 64 threads a piece | card 79 ms | 66 |
| Preparing on a thread of its own, refreshes and records spread | 103 | 75 |
| List calls written down and translated by eight threads (`VICTORY_THIN`) | 75 | 68 |
| Submissions joined, card steps, textures sent ahead | 68 | about 63 |
| Title bytes under shader writes compared with AVX2 on six threads | 14 ms of it | 4-7 |
| Stores into guarded memory looked at first, fewer guards reset | 69 ms of threads' time | about 6 |

(Each figure is from the run in which it was measured; they overlap and do not add up.)

## Faults found, and what they were

- **Boats sank, the sea plane went under.** The ocean's height and displacement maps are
  textures that compute passes write and the title's physics sample on the CPU. They were
  never brought back to title memory. Fixed by Geckomacabre's read-back as merged in
  fxceless/victory-fork f5c03f3b; on by default here (`VICTORY_TEXTURE_BACK=0`: off).
  With it the boat stays up through a whole run; without it sank in every one.
- **A device loss about every 16 minutes**: a page fault inside a texture made in the same
  tenth of a second. A slot showed it before its upload had run.
  `VICTORY_SHOW_AFTER=1`; none in the sessions since (likely fixed, not proven).
- **A rare lost write** (6 of 200 runs of a stress test): a view was mapped between our
  look at its guards and our lock, and views were opened one after another. None in 603
  runs after.
- **Blocky patches**: indirect dispatches cut at 65,535 groups.
- **Square holes and dashed edges in shadows**: pixel shaders that read their quad's
  neighbours with wave reads; on AMD helper lanes take no part in them.
- **Cube reflections too blurred, wrong materials**: DXIL's `Log` is base 2, and
  normalized buffer codes were one step off.
- **Every ray missed**: the dispatch flag that says counts are threads, not groups.

## Measured and not worth it

- Unordered early pipelines for everything (AMD's compiler crashed).
- Series S profile, smaller textures, a second drawing queue, `ASYNC_SUBMIT` off.
- Mapping pages on sixteen threads: no more pages mapped, frames longer.
- Scalar buffer loads four dwords a call (`VICTORY_SHADER_OPT=127`): no gain on this card.
- Wave sizes, fused multiply-adds, fast reciprocals, raw picked buffers, leaving out
  barriers: nothing to 0.1 ms each. Exact RDNA float rules (`VICTORY_RDNA_FLOAT`): 0.8 ms.
- Running the console's shader code natively on the card: the Windows driver takes no raw
  code, the two generations differ, and a Vulkan host layer ends at the same compiler. What
  is left to gain from shader code was guessed at under 1 ms.
- An on-screen panel of numbers (`VICTORY_PANEL`): it helps finding where a frame goes but
  costs some, so it is off and F3 shows it.

## What is next

1. The preparing thread: split what one submission does over two threads in a row; ask
   for the next submission while this one is sent; view the card's own buffers where fills
   copy them now.
2. The card: small buffers' copies in its own memory (`VICTORY_CARD_BUFFERS_MIN_KB=64`,
   after fxceless c36971b2; not measured here yet), and the passes that run for nothing
   without rays.
3. Stalls: 270-660 s of a session were full of frames over 200 ms, almost all while new
   pipelines were made. `victory warm`, and a quarter of the workers for the slow ones.
4. Outdoor flicker: suspected to be one bad indirect record that uses up its list's
   budget, so that every later draw of it is empty. The log's frame line counts them;
   `VICTORY_INDIRECT_REST=0` is the experiment.
5. Memory, 40-46 GB private: lists' allocators 9 GB, windows 7, uploads 4
   (`VICTORY_MEMORY_MAP=1` lists it).

## The other repositories, compared

Read up to 2026-11-25: Marvisak/VIctory, fxceless/victory-fork, DexNom/VIctory,
Belobug/VIctory-Belobug-AMD, FQY55kai/ZJOU-VICTORY. What was taken is in the history,
each commit naming its source. In short:

- Marvisak's commits hardly apply any more; nearly all of what they fix is here, often
  another way. Taken: nearest normalized codes, d16 views, shader model flags.
- fxceless: the base. Newest taken: texture read-back, small buffers on the card, hashes
  over a buffer's own bytes.
- DexNom: thread counts of ray dispatches, cube gradients, joined sparse ranges.
- Belobug: pipeline workers for screen passes, the ambient stand-in's idea, pipeline and
  constant buffer bookkeeping (romainhedouin).
- ZJOU: render targets not set again; some shader corrections are being looked at.

## 2026-11-26: card copies on a 10 GB card

RTX 3080 10 GB (local budget 8.3-9.3 GB), 32 GB, Series S profile, the launcher
(`Play-VIctory.cmd`, which sets `VICTORY_CARD_BUFFERS_MIN_KB=64`). 3.1 fps: a frame
took about 320 ms, 230 of them the card's time in the upscaler's ten or so compute
passes. They all read and write one scratch buffer of 21,045,760 bytes that the title
makes in the first world frame, after the world's own buffers. By then the copies in
the card's memory had filled their cap (512 MiB on budgets of 12 GiB or less), the
buffer got no copy there, and every pass went over the bus. Nothing said so: only a
copy turned down for the card's room was logged, not one turned down by the cap.

| Cap, least size | Upscaler buffer on the card | Frame |
|---|---|---|
| 512 MiB, 64 KiB (the launcher) | no | about 320 ms |
| 640 MiB, 64 KiB | no: some 2,900 buffers of 64 KiB, about 190 MiB, filled it again | – |
| 640 MiB, 1 MiB | no: the world's buffers of 1 MiB or more came to about 640 MiB first | – |
| 1,280 MiB, 1 MiB | yes: 82-83 copies, 687-688 MiB | 83-95 ms, the card 4-15 ms |

With 1,280 MiB the card used 7.7-7.9 GB of its 8.7-8.8 GB budget, no copy was turned
down for room, and textures were not short. What the title wanted on the card: about
690 MiB of buffers of 1 MiB or more, about 880 MiB with the small ones at 64 KiB.

What changed (`crates/victory-gpu/src/shadow.rs`, `card_limits_from`, `card_admit`):

- The cap on budgets of 12 GiB or less is a seventh of the budget, at least 512 MiB:
  1,214-1,360 MiB on that card, around the 1,280 measured. At 12 GiB it is 1,755 MiB,
  under the 2,048 of the next tier, so a larger budget never gets a smaller cap. The
  larger tiers are as they were. The room the texture queue keeps for copies not made
  yet (at most 1 GiB) is then 1 GiB on these cards, as in the 1,280 run.
- With the default cap of that tier, copies under its own least size (16 MiB) together
  take at most three quarters of the cap; a quarter stays for the bigger ones, so a
  late big buffer is not crowded out by thousands of small ones. Three quarters and not
  one: the world's buffers of 1-16 MiB were on the card in the 1,280 run and should
  still fit while the cap has room. An explicit `VICTORY_CARD_BUFFERS_MB` keeps first
  come, first served.
- A buffer turned down by the cap or by that share is logged as one turned down for
  room is (the first 64, then every 32nd), naming which one, the bytes held and both
  limits.

Not measured in the game yet; a unit test replays the order of the session above.

## 2026-11-26: posterised launches came from running without ray tracing

The title's last composite pass (`PS_CompositeNMB`) came out posterised in
about two launches in three, for the whole session: each colour channel on
a ladder of levels about 1.26 apart. Unattended launches graded by the
roughness of the presented frame's histograms (`docs/bug-journal.md`,
2026-11-26, has the method), good of graded:

| configuration | good | bad |
|---|---|---|
| builder off, stock launcher | 3 | 5 |
| builder off, none of the 19 launcher switches | 2 | 5 |
| builder off, without `THIN`/`JOIN_PARTS` | 1 | 5 |
| builder off, other diagnostics (watches, constant capture, writers watch, a forced settings listener, a pipeline wait) | 9 | 17 |
| **builder off, all** | **15** | **32** |
| builder off, ray-resolve and ray-activation programs kept | 2 | 0 |
| `VICTORY_BVH_BUILDER=1` | 7 | 0 (one more crashed before grading) |
| ray tracing on by default (stock launcher) | 2 | 0 |

At 32 % good, 9 good in a row has a chance of about 3 in 100,000. Change:
ray tracing is on unless `VICTORY_BVH_BUILDER=0` (`pipeline::builder_asked`),
so the builder's stages run and the resolve and activation programs are no
longer left out. Accepted on this evidence without a further proof batch.
The mechanism is not shown (inferred: what the left-out programs write each
frame is never written and holds whatever that memory held at the start).
Measured on the RTX 3080 10 GB machine (Ryzen 9 5900X, 32 GB, release build, stock launcher) in the prologue's opening scene: 100 to 108 ms a frame (9 to 10 fps) with ray tracing on, of which some 66 ms is preparing on the CPU and 14 to 17 ms on the card, against 67 to 70 ms (14 to 15 fps) with it off: about a third of the frame rate. The card held about 8.5 GB of a 9.0 GB budget and the process about 20 GB. In the playable open world the frame rate was 8 to 12 fps as seen in play (not taken from the log). Long sessions were not measured; one launch ended with the PC's memory 98 % in use.

## 2026-11-27: the submitting thread's own work, cut by a seventh

RTX 3080 10 GB, Ryzen 9 5900X, 32 GB, Series S profile, the launcher's
switches (`Play-VIctory.cmd`), ray tracing on, release builds; each launch
300 s with Enter pressed by script, measured from the world's first frame
(the pixel shader `6318f669e4f17d65` first bound): "gameplay" is 95 s to the
end (the player idle in a cell, 22-27 frame lines of 5 s), "cinematic"
30-72 s. Medians of the frame lines; [low-high] the spread within a launch.
Launches were one at a time on the machine (a shared lock, and any launch
that met another game was thrown away: one was, 36 GB on a 32 GB machine,
out of memory). One baseline launch ended at 36 s in a crash in the title's
own code (a write to 0); it is left out.

What changed, each its own commit and switch:

1. Blocks of 64 KiB or more of host memory handed to the title (its
   allocator's fallback `XMemAllocDefault`, device heaps) are write-watched
   reservations (`watched.rs`, `VICTORY_WATCHED_HEAP`). The upscaler's
   scratch buffer (21 MB) is no longer copied whole twice a frame nor sent
   up whole: "copied whole (memory not watched)" 5.3 → 0.0 ms a frame, bytes
   sent up to card copies 56 → 31-39 MB a frame. Its landings are still
   compared by the next refresh (`VICTORY_LAND_ECHO`: 41-44 MB a frame "held
   already and left") and their pages reset by the next asking (pages found
   written 6,500 → 12,000 a frame), so of the 41.6 MB copied some 40 MB of
   reading stays.
2. Kernel refusals of write-watch askings remembered until the address
   space changes (`VICTORY_ASK_REFUSED`): refused askings 357 → 140 a frame
   (some 200 a frame not asked again).
3. Card copies whose CPU side changed are kept in a list instead of looking
   at all ~3,700 at every submission (`VICTORY_CARD_TOUCHED`; was 3.8 % of
   the submitting thread in a profile).
4. The write journal is looked at by runs of 64 pages in the fast path that
   says a refresh copies nothing (was 5 % of the submitting thread).
5. Diagnostics: who holds the guards' lock (below), and askings refused and
   answered by caller.
6. Cherry-picked: rorodev6/victory-nvidia-megafork d1fe0bd (Jeremy
   Goldberg), the card compare in an `ExecuteCommandLists` of its own; one
   more queue call per submission, in all "after" numbers.

Release, gameplay (fps, ms a frame, "preparing" (the waits for
flights are counted apart, as the frame line's "on the GPU"), the copies of refreshes,
bytes sent up, refused card steps a frame, CPU seconds a frame):

| launch | fps | ms/frame | preparing | copies | sent up | steps refused | CPU s/frame |
|---|---|---|---|---|---|---|---|
| before 1 | 10.2 [8.8-11.4] | 97.6 | 79.1 | 30.2 ms 114 MB | 57 MB | 0.00 | 0.98 |
| before 2 | 11.9 [10.7-12.4] | 84.3 | 71.7 | 29.1 ms 124 MB | 63 MB | 0.00 | 0.90 |
| before 4 | 10.0 [8.6-11.0] | 99.7 | 73.5 | 27.9 ms 109 MB | 54 MB | 0.10 | 0.99 |
| before 5 | 10.8 [7.9-12.4] | 92.6 | 73.2 | 28.0 ms 117 MB | 59 MB | 0.09 | 0.92 |
| after (1, 2) 1 | 9.2 [8.2-9.9] | 108.0 | 63.4 | 22.6 ms 94 MB | 34 MB | 0.50 | 0.93 |
| after (1, 2) 2 | 6.6 [4.7-7.9] | 151.2 | 65.3 | 29.1 ms 100 MB | 38 MB | 0.88 | 1.03 |
| after (all) 1 | 10.8 [7.2-12.3] | 92.6 | 66.9 | 23.4 ms 93 MB | 35 MB | 0.09 | 0.93 |
| after (all) 2 | 9.9 [4.0-14.0] | 100.5 | 59.5 | 21.4 ms 97 MB | 39 MB | 0.56 | 1.00 |
| after (all), `VICTORY_PLACED_ORDER=1`, 1 | 8.1 [7.1-9.0] | 122.9 | 43.2 | 17.1 ms 102 MB | 41 MB | 0.62 | 0.92 |
| after (all), `VICTORY_PLACED_ORDER=1`, 2 | 11.8 [10.8-12.7] | 84.9 | 65.2 | 23.4 ms 95 MB | 36 MB | 0.08 | 0.93 |

("steps refused": the median of the launch's last 20 lines.)

Preparing fell from 71.7-79.1 ms (median 73.4) to 59.5-66.9 (all changes;
63.4-65.3 with the first two): some 10 ms, more than the spread of the
four launches before (7.4 ms). The frame rate did not rise: the spread of
fps is as wide as before and its median as high. What took the gain: steps
the card does instead of the CPU (`VICTORY_CARD_STEPS=2`) are refused more
often once preparing is quicker, and each refusal waits for the flights in
the air, 60-80 ms. Before, 0.00-0.10 submissions a frame were refused;
after, 0.09-0.88 (three of six launches over 0.5), and the frame lines with none refused reach 12-14 fps
(after 2: 14.0 at best) where the ones before stood at 11-12. The refusal
named in the diagnostics launches is always the same: a fill of upload
memory in the title's 174 MB heap (its ring of constants) under shader
writes declared over the whole heap (the skinning passes), "not done on the
card: a copy at its place in a list writes what it reads, or reads or writes
what it writes". `VICTORY_PLACED_ORDER=1` (cut the submission behind such a
copy) is not shown to help: 0.62 refused a frame and 8.1 fps in one launch,
0.08 and 11.8 in the other; it stays off.

Diagnostics build, gameplay, before → after (all), ms a frame of the
submitting thread unless said: asking the write watch 15.4 → 15.1 (all
threads' kernel time 14.8 → 14.2: fewer refusals, more pages reset); the
lists' steps 32.9 → 30.4, of them refreshes 24.4 → 21.2; copies in the
card's memory 13.7 → 9.4; the title's bytes under flights' writes 5.8 → 6.4;
planning steps on the card 6.8 → 7.8; waiting for flights 0.1 → 14.6 (the
refusals above); guards' lock waits of all threads 73 → 62 ms; CPU of the
process 0.91 → 0.98 s a frame (at a lower frame rate: per second 10.2 → 9.7
cores).

What is left, from a sampling profile of the submitting thread in gameplay
(`VICTORY_PROFILE`, 10 ms, 70 s; per frame at the 10 fps of the profiled
launch): the kernel's write-watch askings 13.8 ms (7 of them the ranges of a
submission that lie alone, ~210 calls a frame, the rest the joined runs);
waiting for the refresh threads 7.5; copies of title memory on this thread
4.7; the driver's `ExecuteCommandLists` and list closes 6; waiting for the
heaps' records 3.1; the guards' lock 2.2. The guards' lock is held about
10 ms a frame in all, most of it by the write-fault handler (446 times, 4.6
ms, the longest 6.4 ms) and the guards' own asking (`guard`, 5.6 ms), while
threads wait for it 62-143 ms a frame: a holder that stands in a kernel call
keeps a queue of title threads waiting (inferred: `VirtualProtect` behind the
address space's lock while a long `GetWriteWatch` holds it).

Checked in a diagnostics launch of all the changes with
`VICTORY_REFRESH_CHECK=1`, `VICTORY_UPCOPY_CHECK=1` and
`VICTORY_FETCH_CHECK=1` (320 s, frames captured): no host copy found stale
after its refresh; no device removed; fetched pieces caught mid-write 0.73 a
frame, all found whole when fetched again; the up-copy check named only
buffers in the runtime's own title memory (below 2^40, none of the new
write-watched blocks) and, every time, "no shader was declared to write the
buffer": shader writes through views nobody declares, the gap §3.5 of the
memory doc describes. The captured frames show the red glow with a black
blot over the hair in the prison-window scene at about 490 frames after the
world's first; the same is in captures of a build without these changes
(another agent's launch fixScan, 00:14 the same night), so it is not from
them.

The cinematic (30-72 s) is dominated by loading stalls and says little:
preparing 96-193 ms before (four launches), 110-167 after (four; one of
them with a build of ours running beside it).

`VICTORY_RELOOK_MS=20000` (one diagnostics launch against one at the default
4,000): the heap pass's "looking at slots" 21.9 → 15.4 ms a frame in the
cinematic and 17.1 → 13.8 in gameplay, beside the recording, not on the
submitting thread; warnings of a texture picked by index that changed with
nothing to tell of it 215 → 212. One launch each is within the spread
between launches, so the default stays 4,000.

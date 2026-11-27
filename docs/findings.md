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

## 2026-11-27: the texture pool kept a 10 GB card over its budget

RTX 3080 10 GB (DXGI budget 8.7-9.0 GB), Ryzen 9 5900X, 32 GB, Series S profile, stock
launcher with ray tracing on, release build. Unattended launches of 330 s (the world's
first frame at 67-80 s): the cinematic is 5-70 s after that frame, idle gameplay
70-250 s. Medians of the 5-second log lines; one launch per row, so read single-launch
differences in fps and in the cinematic's queue as noise. Before and after are the same
executable, before with `VICTORY_POOL_COMPACT=0`; the card's use, the pool and the
texture counts come from the new pool line (`texture pool: ...`), the queue from
`textures picked by index, the queue`, waits from `new memory waited for`, RAM from the
launch script's samples of the process.

**What was wrong.** With the old pool (64 MiB blocks cut first-fit, a block given back
only when wholly empty) the world's load filled 71-78 blocks, 4.5-5.0 GB, and then let
two thirds of its textures go; those that stayed were in nearly every block, and are
held by descriptor slots, which the sweep cannot let go. For the rest of the launch the
blocks held 4,544-4,992 MB for 1,866-2,307 MB of textures, and the card stood at
100-102 % of its budget. The room rule counted all of that free space as room, but the
textures the queue makes are mostly levels on demand, which take tiles of their own
and cannot use it, so "room" was an illusion that alternated with a 3 GiB reserve for
render targets that were 1.36 GB. (Raising the room share alone made it worse: 9,576 MB
in use.)

**What changed** (cards whose budget is 12 GiB or less; `VICTORY_POOL_COMPACT`): blocks
of 16 MiB, cut from the fullest block with room; a new block only while all blocks stay
within a quarter of the budget and within what textures hold plus a sixteenth; past
that a texture gets an allocation of its own (exactly what the pool off does, and given
back exactly when it goes); empty blocks kept come to a thirty-second of the budget;
the room rule counts free pool space only up to that sixteenth, the floor is a sixth of
the budget, and once the world's render targets are made (they rose by 512 MiB within
20 s) only 512 MiB is kept for more of them; the sweep, when it must let go, first lets
go of textures whose going gives memory back. Larger cards are unchanged.

| launch | window | fps | ms/frame | card in use / budget MB | pool blocks / held MB | room yes | due / oldest (frames) / waiting | MakeResident in window: calls / waited / longest | "no card copy" lines | private / free RAM MB |
|---|---|---|---|---|---|---|---|---|---|---|
| before 1 | cinematic | 5.4 | 184 | 8,897 / 9,046 (98 %) | 4,544 / 1,958 | 1/11 | 956 / 79 / 5,808 | 13,000 / 10.3 s / 388 ms | 66 | 26,157 / 501 |
| before 1 | gameplay | 10.4 | 96 | 9,176 / 9,046 (101.5 %) | 4,544 / 1,866 | 0/31 | 452 / 8 / 266 | 14,000 / 5.9 s | 0 | 26,018 / 1,616 |
| before 2 | cinematic | 5.0 | 201 | 8,956 / 8,945 (100 %) | 4,992 / 2,307 | 2/11 | 11,266 / 371 / 7,639 | 11,000 / 12.0 s / 807 ms | 75 | 26,590 / 667 |
| before 2 | gameplay | 11.5 | 87 | 9,182 / 8,956 (102.4 %) | 4,992 / 2,227 | 2/30 | 163 / 8 / 81 | 14,000 / 5.9 s | 0 | 26,742 / 1,521 |
| before 3 | cinematic | 3.6 | 282 | 8,730 / 9,020 (97 %) | 4,672 / 2,205 | 2/11 | 1,559 / 616 / 4,052 | 9,000 / 7.3 s / 938 ms | 28 | 23,516 / 946 |
| before 3 | gameplay | 9.7 | 104 | 8,980 / 8,964 (100.1 %) | 4,672 / 2,075 | 0/29 | 160 / 8 / 170 | 13,000 / 7.9 s | 2 | 25,709 / 1,152 |
| after 1 | cinematic | 4.3 | 230 | 7,564 / 8,751 (87.6 %) | 2,304 / 2,028 | 4/11 | 3,042 / 208 / 7,499 | 12,000 / 10.8 s / 451 ms | 0 | 23,764 / 2,238 |
| after 1 | gameplay | 11.1 | 90 | 7,769 / 8,724 (89.1 %) | 2,304 / 1,894 | 1/30 | 72 / 8 / 110 | 15,000 / 10.5 s | 0 | 24,246 / 2,281 |
| after 2 | cinematic | 2.8 | 352 | 7,708 / 8,728 (88.4 %) | 2,320 / 1,982 | 4/11 | 6,953 / 24 / 8,335 | 9,000 / 21.2 s / 813 ms | 0 | 24,892 / 836 |
| after 2 | gameplay | 11.4 | 88 | 7,764 / 8,787 (88.3 %) | 2,320 / 1,996 | 1/30 | 314 / 8 / 86 | 15,000 / 9.5 s | 0 | 24,922 / 2,161 |
| after 3 | cinematic | 4.2 | 239 | 7,907 / 8,752 (90.3 %) | 2,320 / 1,929 | 0/11 | 2,256 / 400 / 9,074 | 11,000 / 10.1 s / 496 ms | 0 | 25,097 / 970 |
| after 3 | gameplay | 13.1 | 76 | 7,788 / 8,729 (89.2 %) | 2,320 / 1,932 | 1/30 | 366 / 8 / 40 | 15,000 / 6.5 s | 0 | 24,386 / 3,476 |

("After 2" had another, small `victory.exe` of another session beside it from 85 s
into the world; its budget and RAM were not touched. A launch that had a second game
beside it, and the launches before the fix below, are left out.)

In short: the card went from 100-102 % of its budget in gameplay to 88-89 %, the most
seen 91.7 % (before: up to 103.7 %); the blocks from 4.5-5.0 GB to 2.3 GB, 82-86 % of
them held; more of the title's textures had a host copy (2.4-2.5 GB against 1.9-2.2 GB
at the end); no buffer was refused a card copy (28-75 such lines in the cinematic
before); the process took about 1.5 GB less private memory and the PC had about 1 GB
more free in gameplay. Gameplay fps: 9.7-11.5 before, 11.1-13.1 after. The cinematic
is noisy both ways (2.8-5.4 fps; its queue between 956 and 11,266 slots due in either
case) and shows no clear change. What got worse: `MakeResident` calls, because 10,000
to 11,700 textures a launch past the pool's line have memory of their own: in gameplay
6.5-10.5 s waited in 180 s against 5.9-7.9 s, the longest wait 451-813 ms against
388-938 ms. The room rule now says no in gameplay because the card is at its line
(90 % less 512 MB kept for targets), not because of reserves that never fill: the
queue there had 40-110 slots waiting against 81-266.

Measured and rejected on the way: the cap at a third of the budget instead of a
quarter (2,880 MB). The load then filled 3,072 MB of blocks, two thirds of its
textures went, the blocks stayed at 3,072 MB for 1,136 MB of textures (those left are
held by slots and the sweep cannot let them go), the card sat at 94 % and no texture
was made for the rest of the launch. The quarter is what bounds the load's leftovers.

A fault found on the way: the aliasing barrier before a texture's first upload was put
on textures with memory of their own too, which the host refuses; with the compact pool
those are common and both first launches ended in the load (`docs/bug-journal.md`,
2026-11-27).

Device losses: none. Besides the three launches above (330 s each), five launches of
200 s with the same build (the world at 75-81 s, so the cinematic and the start of
gameplay) had no `removed`, `DXGI_ERROR`, `0x887a...` error, "device lost", "host GPU
failure" or list that did not close (one launch matched `0x887a` only in a page address
of an info line). In them the card stood at 86.8-87.9 % (the most 91 %), the blocks at
2,224-2,320 MB for 1,762-2,057 MB of textures. Why this does not undo what the pool's
rules guard against (runs n08, n11, n15: a texture shown in memory just made):
textures that go past the pool's line take the path the pool off takes, which made
every texture in its 240 s launch without a loss: memory made not resident, then
`MakeResident` and the mapping waited for (`VICTORY_WAIT_RESIDENT`), and slots that
show it only after its upload has run (`VICTORY_SHOW_AFTER`); pieces still come from
ripe blocks, a piece still rests 2.5 s, a block is still let go only when wholly empty,
and objects are still dropped `bury_lag` submissions late. What changed is which ripe
block a piece is cut from, the block size, and when a texture is made at all.

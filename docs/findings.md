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

# VIctory on an RX 7900 XTX

## Run the codec_d Windows build

Download and extract the Windows x64 ZIP from the [private releases](https://github.com/codec-d/cool_fox/releases/latest). Open PowerShell in the extracted folder and point the launcher at your own complete Xbox dump.

```powershell
$env:VICTORY_GAME_DIR = 'D:\Games\MyDump'
.\Play-VIctory.cmd pareto-residency
```

The launcher selects Series S and the RTX-tested residency settings. It builds its shader cache locally under `%LOCALAPPDATA%\VIctory\cache-pareto`. Set `VICTORY_CACHE_DIR` to reuse an existing cache outside the checkout. Windows x64, an AVX2 CPU, and a Direct3D 12 GPU with shader model 6.6 and mesh shader support are required.

To build from this checkout, install the pinned Rust toolchain and the MSVC C++ build tools with a Windows SDK, then run `cargo build --release --locked -p victory`. The same launcher finds `target\release\victory.exe`. Prerequisites, build configurations and checks are in [docs/building.md](docs/building.md). The sections below the next one describe the upstream AMD fork and its separate launcher.

## Where this checkout stands on a 10 GB card (2026-11-26)

Measured on one machine, with `Play-VIctory.cmd pareto-residency` and no other
settings:

| | |
|---|---|
| CPU | AMD Ryzen 9 5900X (12 cores, 24 threads) |
| Memory | 32 GB |
| GPU | NVIDIA GeForce RTX 3080, 10 GB (32-lane waves; the layer's VRAM budget is about 8.3 to 9.3 GB) |
| Display | 3440x1440 at 164 Hz; the title renders 1280x720 and upscales to 1920x1080 |
| System | Windows 11 Pro, build 26300 |
| Console profile | Series S |
| Build | release, Rust 1.98.1, MSVC 14.51 |

What to expect on this machine now, with ray tracing on (the default):

| | |
|---|---|
| Intro and loading | 30 fps |
| The prologue's opening scene | 9 to 10 fps (about 100 to 108 ms a frame: some 66 ms preparing on the CPU, 14 to 17 ms on the card) |
| **The playable open world** | **8 to 12 fps** (as seen in play; not yet measured from the log) |
| Video memory | about 8.5 GB in use of a budget of 9.0 GB |
| Process memory | about 20 GB |

How it got there, in the prologue's first room:

| | Frame rate | A frame |
|---|---|---|
| As this checkout started | 2.9 to 3.1 fps | 320 to 350 ms, 235 to 286 ms of it on the card |
| After the two memory fixes below, ray tracing off | 14 to 15 fps | 67 to 70 ms, 0 to 3 ms on the card |
| With ray tracing on, as the title is meant to run | 9 to 10 fps | 100 to 108 ms, 14 to 17 ms on the card |

Ray tracing costs about a third of the frame rate here. It is on by default
all the same: the title always runs with it, and with it left out about two
launches in three were posterised (below). Long sessions and memory over time
were not measured; one launch with ray tracing on ended when the PC's memory
was 98 % in use, and 32 GB is little for it.

What changed, with the measurements, is in [docs/findings.md](docs/findings.md)
and the crate documents under [docs/crates](docs/crates):

- **Levels on demand took effect.** The launcher turned them on, but every
  texture that qualified was cut from a pool block first, so none was made that
  way. About 3 GB of the card had been given to texture levels the title never
  loaded.
- **Buffer copies on cards of 12 GB or less.** The cap on title buffers kept
  in the card's memory was 512 MB and filled before the title's upscaler made
  its 20 MB buffer, so each of its passes read PC memory over the bus: about
  230 ms a frame. The cap now grows with the card, and small buffers cannot
  take all of it.
- **`ClearUnorderedAccessViewUint` and `Float`** on command lists, which the
  upscaler calls and which were missing.
- Clippy and the tests pass again (`cargo clippy` both ways; 967 tests pass, 45 are ignored).

- **Ray tracing is on by default** (`VICTORY_BVH_BUILDER=0` or
  `scripts\play.ps1 -NoRayTracing` turns it off). With it off, 32 launches of
  47 were posterised for the whole session: smooth shading collapsed to a few
  levels in each colour channel. With it on, none of 9 at first; that was
  taken as the fix without a longer run of launches, and it was not the whole
  story: on 2026-11-27, with ray tracing on, 10 of 16 launches between 00:36
  and 05:00 were posterised again (below). How leaving it out causes the
  fault, and why a third of such launches were right all the same, is not
  known; `docs/bug-journal.md` (2026-11-26 and 2026-11-27) has what was ruled
  out and how to take the search up again.
- **No more hotspot on the player's hair.** About 450 frames into the first
  scene a bright spot grew from the hair until the picture was white (7 of 9
  launches). The cause was 16-bit float rounding in AMD's upscaler, which the
  title ships as `amd_fidelityfx_upscaler_xbox.dll`: the host rounds toward
  zero, the console to nearest. That file's programs now round as the console
  does by default (0 of 11 launches with the hotspot; `VICTORY_F16_NEAREST=0`
  brings the host's rounding back).

Known here and not solved:

- Some characters' skin is black in some launches, with or without the fault
  above (perhaps textures that arrive late; not looked into).
- **Posterisation comes and goes and its cause is not known**: on
  2026-11-27, 15 launches up to 00:05 were clean; 10 of 16 between 00:36 and
  05:00 were posterised for the whole session, whatever the 16-bit rounding;
  then 8 strictly alternated launches on an idle machine (05:22-06:09) were
  all clean (`docs/bug-journal.md`, 2026-11-27).
- **The frame is bound by the CPU**: about 66 ms of 100 is the layer
  preparing submissions.
- More is wrong in the open world; it has not been written down yet.
- Three of the title's compute programs keep 64-lane waves and do not
  translate for a 32-lane card; their dispatches are left out.
- An NVIDIA card shows shaders 1,000,000 view slots and the title's heap has
  1,245,184: views past the limit read a null slot.
- Tens of thousands of textures wait for room on a 10 GB card.

A fork of [VIctory](https://github.com/Marvisak/VIctory), a compatibility layer
that runs an Xbox Series title natively on Windows from files the user supplies.
This one is about **speed on AMD RDNA 3**: the world at 30 frames a second on
one PC, without giving up the picture. It is not there yet.

**This repository contains no game files, code or assets.** You need your own
copy of the game. It is not affiliated with or endorsed by the game's developer
or publisher.

## Where it stands (2026-11-25)

Tested on one machine only: Windows 11, Intel i9-12900K, 64 GB RAM, Radeon
RX 7900 XTX, 1440p, the Series X profile.

| | |
|---|---|
| Menus, videos, loading screens | 30 fps |
| Prologue, indoors | 50-75 ms a frame (13-20 fps) |
| First mission with the boat | 46-53 ms (19-22 fps) |
| Street chase with many cars and people | 100-165 ms (6-10 fps), single frames far longer |
| Outdoors on foot, the camp | 60-90 ms |
| Flying | 42-57 ms |
| Longest session so far | 41 minutes, no crash |
| Memory | 40-46 GB of private memory: **64 GB of RAM recommended** |

It started at 280 ms a frame in the same scenes two days earlier.

What is known to be wrong:

- Trees, plants and far buildings flicker outdoors in some places.
- Rooms with little direct light are too dark with `-NoRayTracing`.
- Textures of what just came into view appear late while the title streams.
- The first run with a new driver or a new build stutters while the driver makes
  pipelines (`victory warm` makes them ahead).
- Recording a frame in the world makes the title's own watchdog end it.

## How to run it

You need Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with shader
model 6.6 and mesh shaders, Rust (the toolchain is pinned), the MSVC build
tools, and your own dump of the title.

```powershell
git clone https://github.com/avatarmanz/VIctory-7900XTX
cd VIctory-7900XTX
copy victory.example.toml victory.toml      # then set your paths, outside this folder
cargo build --locked --profile diag -p victory --features diagnostics
scripts\play.ps1 -Exe <path of your copy's executable>
```

`scripts\play.ps1 -Safe` leaves out the newest, faster ways; try it first when
something is wrong. `-NoRayTracing` turns the title's ray tracing off (faster;
`VICTORY_AMBIENT` then stands in for its indirect light).

Before the first run, ahead of time (both can be stopped and go on later):

```powershell
target\diag\victory.exe shaders <the shader archives>   # every shader translated, minutes
target\diag\victory.exe warm --used                     # pipelines earlier runs drew with, for a new driver or build
```

Commands, debugging switches and upstream's own description:
[docs/README-fxceless.md](docs/README-fxceless.md). Every `VICTORY_...` switch is
described in a comment where it is read.

## What is different here

In short; the reasons and the measurements are in [docs/findings.md](docs/findings.md).

- **List calls are written down, not carried out, on the title's threads**
  (`VICTORY_THIN`): eight threads of ours translate them.
- **Submissions are prepared by a thread of ours and joined**
  (`VICTORY_ASYNC_SUBMIT`, `VICTORY_JOIN_PARTS`), with what can be spread over
  threads spread: refreshes, compares (AVX2), descriptor records, landings.
- **What shaders wrote comes back beside the write guards**, by the dwords that
  changed, on a thread of its own (`VICTORY_RELAY`, `VICTORY_LAND`).
- **Buffer fills that need bytes still on the card are done on the card**
  (`VICTORY_CARD_STEPS`) instead of waiting for it.
- **The title's writes are caught more cheaply**: one view opened at a fault,
  blocks that rest, places in the title's code that look before they write.
- **Textures go up on a copy queue of their own** and a slot shows one only after
  its upload ran, which also ended a device loss that came every 15 minutes or so.
- **Boats float**: textures that shaders write and the title's CPU reads go back
  to title memory (from fxceless and Geckomacabre, on by default here).
- **Frame record and replay** with per-pass times and shader variants compared
  turn by turn ([docs/replay.md](docs/replay.md)).

## Credits

This is a tree of other people's work with more on top. The history is kept, and
commits taken from elsewhere name the commit and its author. Every author in
that history is listed in [docs/CONTRIBUTORS.md](docs/CONTRIBUTORS.md).

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its
  contributors: the loader, the runtime services, the Direct3D 12 back end and
  the shader recompiler.
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)**: the fork
  this one started from, and the people whose pull requests it carries.
- **[DexNom/VIctory](https://github.com/DexNom/VIctory)**: ray tracing fixes,
  cube gradients, sparse textures.
- **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**:
  the ambient light idea, pipeline workers, and work by romainhedouin.
- **[FQY55kai/ZJOU-VICTORY](https://github.com/FQY55kai/ZJOU-VICTORY)**.
- **[myokex/victory-amd](https://github.com/myokex/victory-amd)**: the guard over
  every view of a page (with Spexivus, joroyt and tightfall), and Joshua Odom's
  reads into title memory.
- **Geckomacabre** and **cxy0072019-sketch**: the texture read-back.
- **[XCL](https://github.com/ivall/xcl)** (GPL-2.0-only) was read for how the same
  title behaves under another layer; no code of it is here.
- **codec_d**: the NVIDIA Wave32 lowering, the Series S two-plane fix, the
  residency budgets for 32 GB of memory and the pareto-residency launcher this
  checkout runs with.
- This fork's changes were made with [Claude Code](https://claude.com/claude-code).

## License

GPL-3.0-or-later, as VIctory. See [LICENSE](LICENSE); the one file that is not
covered is named in [docs/asset-policy.md](docs/asset-policy.md).

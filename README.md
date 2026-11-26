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

| | Before | After |
|---|---|---|
| The prologue's first room, settled | 2.9 to 3.1 fps (320 to 350 ms a frame, 235 to 286 ms of it on the card) | 14 to 15 fps (67 to 70 ms, 0 to 3 ms on the card) |
| Intro and loading | 30 fps | 30 fps |

One room is not the whole title: busier scenes are slower, and long sessions
and memory over time were not measured here.

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
- Clippy and the tests pass again (`cargo clippy` both ways; 944 tests pass, 45 are ignored).

Known here and not solved:

- **Some launches are posterised.** A launch is either right throughout or
  wrong throughout: smooth shading collapses to a few levels in each colour
  channel, and some characters' skin is black. It shows in the title's last
  composite pass, as if its colour tables were read without filtering, though
  the tables, the curves, the constants, the pipeline and its samplers were
  each found to be right. What differs between a good launch and a bad one is
  not found. `docs/bug-journal.md` has what was ruled out and the switches
  added to look (`VICTORY_DUMP_TARGETS` for 3D and 1D textures,
  `VICTORY_CAPTURE_CONSTANTS`, `VICTORY_ROOT_LOG`, `VICTORY_LINEAR_SAMPLERS`).
- **The frame is now bound by the CPU**: about 53 ms of 67 is the layer
  preparing submissions.
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
- Rooms with little direct light are too dark without `-RayTracing`.
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
something is wrong. `-RayTracing` turns the title's ray-traced indirect light on.

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

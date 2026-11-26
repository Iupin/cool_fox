# Documentation index

Read in this order when new to the tree. Rules for keeping these current are
in [../CLAUDE.md](../CLAUDE.md): every change updates the doc that owns the
code it touches.

## Orientation

| Doc | What it answers |
|---|---|
| [../README.md](../README.md) | what this is, current state, how to run a release |
| [architecture.md](architecture.md) | the crates, the data flow, import resolution, shader stages in brief |
| [building.md](building.md) | prerequisites, build configurations, tests, lints, pre-commit checklist |

## Reference, one per subsystem (kept current)

| Doc | Covers |
|---|---|
| [crates/victory-gpu.md](crates/victory-gpu.md) | D3D12.X → host D3D12: objects, command lists, draws, descriptors, pipelines, submission threads, present; full module map of the crate |
| [crates/victory-gpu-memory.md](crates/victory-gpu-memory.md) | title memory ↔ GPU coherence: write guards, shadows, uploads, textures, residency budgets, read-back, frame replay |
| [crates/victory-shader.md](crates/victory-shader.md) | console shader → IR → structurisation → resource tracing → host shader; cache |
| [crates/victory-hle.md](crates/victory-hle.md) | console API implementations, host pass-through policy |
| [crates/victory-loader.md](crates/victory-loader.md) | prepare, map, bind, TLS, start; crash reporter |
| [crates/victory-pe.md](crates/victory-pe.md) | PE32+ and import-library reading |
| [crates/victory-cli.md](crates/victory-cli.md) | every `victory` subcommand and flag |
| [crates/support-crates.md](crates/support-crates.md) | victory-core, victory-formats, victory-pipeline, victory-game, xtask |
| [reference/env-switches.md](reference/env-switches.md) | every `VICTORY_*` switch: where read, values, default, effect |
| [reference/repo-tooling.md](reference/repo-tooling.md) | asset-policy mechanics, xtask, git hook, CI, launch scripts, config resolution |
| [asset-policy.md](asset-policy.md) | what may never enter the tree |
| [replay.md](replay.md) | recording and replaying a frame |

## History and rationale (append-only; not reference)

| Doc | Covers |
|---|---|
| [findings.md](findings.md) | measurements and the reasons behind this fork's designs |
| [bug-journal.md](bug-journal.md) | bugs found, how, and the fix |
| [fork-ray-tracing.md](fork-ray-tracing.md) | ray-tracing work carried from another fork |
| [README-fxceless.md](README-fxceless.md) | the parent fork's README: commands and debugging switches as of the fork point |
| [CONTRIBUTORS.md](CONTRIBUTORS.md) | every author in the git history (`git shortlog -sn --all`) |

Where a history doc and a reference doc disagree, the reference doc and the
code win; fix the reference doc if the code says otherwise.

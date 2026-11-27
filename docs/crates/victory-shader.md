# `victory-shader`

Console shader containers in, host D3D12 shaders out. About 49k lines in 40 `.rs`
files (including `build.rs` and one bench). The crate does not translate the
console's DXIL bitcode. It translates the **native GFX10 (RDNA2) machine code**
that the console compiler stores beside the bitcode, because that bitcode can call
console-only intrinsics (`src/lib.rs:1-7`). The output is a PC DXIL container
(SM 6.6, validator 1.6 layout). The crate writes it directly with its own LLVM 3.7
bitcode writer and MD5-variant container hash. It needs no LLVM, DXC or `dxil.dll`
at run time.

Line numbers below are as of commit `f4b48ad`. Statements marked *(inferred)* were
not checked against running code.

---

## 1. Inputs, outputs, dependencies, public API

### Input formats

| Container / blob | Where | Parsed by |
|---|---|---|
| DXBC container (`DXBC` header, FourCC parts) | everything | `dxbc::Container::parse` (`src/dxbc.rs`) |
| `DXIL` part → program header + `XDAT` index at +0x18 | console DXIL containers | `native::native_programs` (`src/native.rs:426`) |
| `XDAT` entry kind `0x0d` → `XBCH` v3 (zlib) → `FSSM` v0x101 | the native programs, one record per HW stage (LS HS ES GS VS PS, MS, CS) | `native.rs:574` `unpack`, `:645` `programs` |
| `ACCC` part starting `FSPX` v1 | compute shaders built by another compiler (the AMD FidelityFX upscaler) | `native::fspx_program` (`native.rs:474`) |
| `ISG1` / `OSG1` / `PSG1` / `PSV0` | signatures, mesh per-primitive outputs, vertex output interpolation | `emit/mod.rs:960` `signature`, `:901` `vertex_output_interpolation` |
| SGD5 files (the engine's shader archive) | outside this crate | `victory-game::sgd5`, `apps/victory/src/shader.rs` (`sources`) give the containers to this crate |

The `native.rs` module doc (lines 1-69) is the reference for the FSSM record and
stage-data layouts: wave32 flag, VGPR count, root layout table, used-resource table,
PS inputs and export patches, VS inputs and outputs, CS `COMPUTE_PGM_RSRC2` and group
size, MS outputs and limits. Unknown versions and implausible counts are rejected on
purpose (`Malformed`), so a format change shows up as an error and is not misread.
A PC-layout `DXIL` part (no `XDAT`) gives no programs.

`NativeProgram` (`native.rs:164`) is the unit of work. It holds `stage: HwStage`,
`wave64`, `vgprs`, `layout: Vec<RootParam>` (where root parameters arrive in user
SGPRs), `used: Vec<UsedResource>`, `io: StageIo`, and `code`.
`NativeProgram::patched_code` (`:183`) applies each render target's full-32-bit
export patch, so pixel exports carry raw values. Pipeline creation on the console
does the same. Translation always works on the patched code.

### Output

`emit::HostShader` (`src/emit/mod.rs:335`):

| Field | Meaning |
|---|---|
| `stage` | `HwStage` translated (only Vs, Ps, Cs, Ms reach here) |
| `dxil` | PC DXIL container with our computed hash (`emit/container.rs:337`) |
| `user_data: Vec<UserData>` | dwords of the `b0 space0` user-data buffer: `index`, root `slot`, `RootType`, `dword` |
| `samplers`, `static_samplers` | immediate samplers as console words and as D3D12 static samplers `s<n> space0` (`emit/sampler.rs`) |
| `root_cbvs/srvs/uavs` | root parameter indices bound as `b/t/u<index>` in `ROOT_CBV_SPACE` = 1 |
| `root_indexed_views` | heap views whose index the host evaluates at bind time (`resource::RootIndexedView`) |

The binding contract (descriptor heap direct indexing, table values as byte offsets
×32 or ×16, raw/structured/typed view kinds, vertex fetch through root SRVs, texture
atomics) is in the module doc `emit/mod.rs:1-81`. Read it before touching
`victory-gpu`'s root-signature code. The reserved root slots and host-computed
user-data kinds are in `emit/shader.rs:8-101`:

| Constant | Value | Purpose |
|---|---|---|
| `SLOT_STRIDES` / `SLOT_TABLE_STRIDES` | root SRV 255 / 252 | per-heap-slot buffer stride and format kind (`VIEW_INTEGER`, `VIEW_SIGNED`, `VIEW_STRIDE_BITS`), `SLOT_STRIDES_COUNT` = 2^21 |
| `SLOT_MEMORY` / `SLOT_FAULTS` | root SRV 254 / root UAV 253 | title memory by raw address: a directory of 64 KiB pages (`MEMORY_PAGES` = 2^21, `MEMORY_READ_ONLY` bit) and a fault list (`MEMORY_FAULTS` = 4096) |
| `USER_MEMORY_BASE` 0x100, `USER_VIEW_ADDRESS` 0x101, `USER_VIEW_BIAS` 0x103, `USER_ROOT_ADDRESS` 0x104, `USER_DISPATCH_THREADS_X` 0x106 | `RootType::Other(code)` user-data dwords that the host computes | see the doc comments |

### Cargo features and dependencies (`Cargo.toml`)

- Dependencies: `thiserror`, `zlib-rs` (XBCH inflate). On Windows, `windows-sys` with
  `LibraryLoader` (to load `dxil.dll` in `emit/validator.rs`). `windows` is optional
  for normal builds and is a dev-dependency for tests.
- `host`: enables `emit::host` (D3D12 device, pipeline creation and draw/dispatch
  helpers). `apps/victory` turns it on. The module is also built under `cfg(test)`
  on Windows (`emit/mod.rs:91`).
- `diagnostics`: only gates `VICTORY_SHADER_LOOP_LIMIT` (`emit/mod.rs:144`) and is
  mixed into `translation_fingerprint` (`:286`). `apps/victory`'s `diagnostics`
  feature forwards to it.
- `build.rs` exports `VICTORY_SHADER_SOURCE_HASH` (two-lane FNV-1a over `src/**/*.rs`)
  and `VICTORY_SHADER_SWITCHES` (every `"VICTORY_*"` string literal in `src`). See §6
  for why the cache does not actually use these.
- Bench: `benches/guard_runs.rs` (`harness = false`), which times structurisation only.

### Public API (what other crates call)

| Item | Used by |
|---|---|
| `emit::translate(&[u8]) -> Result<HostShader, ShaderError>` (`emit/mod.rs:395`) | `victory-gpu/src/shader_cache.rs:118` (the runtime path), `apps/victory/src/replay.rs:480` |
| `emit::emit(c, native, p, st, io, res)` (`:433`) | `apps/victory/src/translate.rs:608` (offline batch, which runs the stages itself) |
| `native_programs`, `NativeProgram`, `HwStage`, `RootType`, `UsedResource`, `ResourceType`, `Container`, `FourCC`, `DxilProgram` | `victory-gpu/src/pipeline.rs`, `draw.rs`, `apps/victory/src/shader.rs` |
| `spill::build`, `spill::is_spilled`, `structure::structure`, `resource::track`, `resource::{Role, Source, RootIndexedView}` | `victory-gpu/src/picked.rs:155-163`, `pipeline.rs:293`, `draw.rs:13` |
| Host facts: `set_host_max_wave`, `set_host_rdna`, `set_view_heap_limit`, `set_raw_views` (and getters `view_heap_limit`, `raw_views`, `host_settings`) | `victory-gpu/src/host.rs:938-941`, `descriptor/host.rs:1453-1536`; the app sets them from env vars for offline work |
| Settings: `loop_limit`, `DEFAULT_LOOP_LIMIT`, `group_sync`, `shader_opt`, `helper_own`, `builder_fences`, `builder_bounds`, `rdna_float` | `victory-gpu/src/shader_cache.rs` (cache key) |
| `emit::rect_list_gs` (`emit/rect.rs`) | `victory-gpu/src/pipeline/host.rs:392,711`, `primitive.rs:16` |
| `emit::host::Host`, `stub_vs`, `stub_ps`; `emit::validator::Validator`; `emit::census` | `apps/victory/src/translate.rs` |
| `gfx10` (decode, SP3 text) | `apps/victory/src/shader.rs` (`shader-info --disasm`), `victory-gpu/src/pipeline.rs:372,398` |
| `ShaderError { NotDxbc, Malformed, Isa{offset,reason}, Unsupported{offset,what} }` (`lib.rs:28`) | everyone. `what`/`reason` are `&'static str`, which is what makes failures tallyable |

---

## 2. Module map

| File | Lines | Role |
|---|---:|---|
| `build.rs` | 65 | Source hash and `VICTORY_*` switch list as compile-time env vars |
| `benches/guard_runs.rs` | 48 | Timing of `structure()` on synthetic EXEC-narrowing programs |
| `src/lib.rs` | 38 | Module list, re-exports, `ShaderError` |
| `src/dxbc.rs` | 183 | DXBC container parse/build, `FourCC` constants (DXIL, ACCC, PSV0, ISG1, OSG1, PSG1, SFI0...) |
| `src/dxil.rs` | 211 | `DxilProgram` header parse, `ShaderKind`, `XboxExtras` (XDAT prefix / xbop trailer detection) |
| `src/native.rs` | 1254 | XDAT → XBCH → FSSM / FSPX extraction, `NativeProgram`, root layout, stage I/O records |
| `src/gfx10/mod.rs` | 522 | Decoder: encodings, instruction boundaries found by following control flow from offset 0, `Flow`, `decode` |
| `src/gfx10/fields.rs` | 830 | Operand fields per encoding (`Fields`, `Operand`, `Valu`, `Dpp`, `Sdwa`), register widths from the opcode name and wave size |
| `src/gfx10/opcodes.rs` | 1333 | Opcode → name tables generated from Mesa `aco_opcodes.py` (mesa-22.3.0, MIT notice kept) |
| `src/gfx10/sp3.rs` | 930 | AMD SP3 disassembly text (`Instruction::text`) |
| `src/ir.rs` | 2148 | SSA IR: `Program`/`Block`/`Inst`/`Phi`/`Reg`/`Value`; `build`; SGPR-spill unparking (`unspill`); irreducible-loop splitting; program variants (`thread_edges`, `without_early_outs`, `without_skips`) |
| `src/spill.rs` | 380 | Compute programs with a spilled user-data table (the BVH builder): rewrites its `s_load`s into entry SGPR values; `spill::build` = `ir::build(patched_code)` + this |
| `src/structure.rs` | 1890 | EXEC removal: lane masks → BDDs (`Bools`, `Atom`), per-lane branches (`LaneExit`), per-instruction guards; `structure_wide` |
| `src/resource.rs` | 4151 | Traces every descriptor and memory address to root parameters (`Source`, `Address`), waterfall proofs, BVH node sources, root-indexed views |
| `src/io.rs` | 919 | "I/O raising": input VGPR/SGPR meanings, `v_interp_*` pairs, exports (`Io`, `Input`, `Interp`, `Export`, `Target`) |
| `src/bvh.rs` | 710 | CPU reference of `image_bvh(64)_intersect_ray` (node formats, slab test, sort network, watertight triangle test) |
| `src/emit/mod.rs` | 3052 | Entry points `translate`/`emit`, the wave64-on-32 retry ladder, `widen`, signatures, switches, `translation_fingerprint`, many tests |
| `src/emit/lower.rs` | 13414 | IR → DXIL lowering, one machine instruction at a time (`Lower`), wave-size policy, memory by address, descriptors and handles |
| `src/emit/intersect.rs` | 599 | BVH node test as inline DXIL; included as child module `lower::intersect` via `#[path]` (`lower.rs:23-26`) |
| `src/emit/wave.rs` | 1292 | Whole-wave sections (`s_or_saveexec -1` … restore): symbolic simulation of DPP/permlane/readlane reductions and scans → DXIL wave ops |
| `src/emit/own_bits.rs` | 914 | Proof that a wave OR only steers a loop over each lane's own bits, so each host wave may use its own OR |
| `src/emit/threading.rs` | 218 | Jump threading of uniform diamonds (`constant_edges`), used as a last retry |
| `src/emit/shader.rs` | 1008 | `Shader` under construction: dx.op declarations, resources, metadata, flags, binding constants, `finish` (runs the optimiser, writes the container) |
| `src/emit/bitcode.rs` | 1279 | DXIL module model (`Module`, symbolic `V`) and LLVM 3.7 bitcode writer (unabbreviated records only) |
| `src/emit/bitcode/opt.rs` | 2228 | Exact clean-up passes over the module (§3.6) |
| `src/emit/bitcode/census.rs` | 174 | Diagnostic count of emitted instructions by kind (`VICTORY_SHADER_CENSUS`) |
| `src/emit/container.rs` | 492 | PC container parts (SFI0, ISG1, OSG1, PSV0, DXIL, plus PSG1 from `shader.rs:954`), signature encoding, `MESH_OUTPUT_LIMIT` (32 KiB), MD5 container hash |
| `src/emit/sampler.rs` | 187 | Console sampler words → `StaticSampler` |
| `src/emit/rect.rs` | 484 | Our geometry shader that completes console rectangle lists (topology type 19) |
| `src/emit/host.rs` | 1515 | Test/batch D3D12 host: `Host::new(warp)`, root signatures, compute/graphics/mesh pipelines, `draw`, `dispatch` |
| `src/emit/validator.rs` | 221 | `IDxcValidator` from `dxil.dll` (`VICTORY_DXIL`, else newest SDK, else `LoadLibrary`) |
| `src/emit/valu_tests.rs` | 2538 | GPU tests: synthetic VALU programs run on the hardware adapter + WARP (38 tests; the int8 dot products, `v_cvt_pk_u8_f32` and `v_fma_mix_f32` among them) |
| `src/emit/diff_tests.rs` | 1330 | Differential HLSL tests: console DXC + translate vs PC DXC, both run on the GPU |
| `src/emit/intersect_tests.rs` | 702 | BVH lowering vs `bvh.rs` reference on the GPU and WARP |
| `src/emit/waterfall_tests.rs` | 438 | Waterfall loops over per-lane descriptors, run on the GPU |
| `src/emit/wave64_tests.rs` | 738 | Console wave64 on 32-lane hosts: reductions in widened branches |
| `src/emit/size_tests.rs` | 371 | Descriptors read for sizes and counters, run on the GPU |
| `src/emit/permlane_tests.rs` | 254 | Row permutations on ≥32-lane hosts |
| `src/emit/cube_lod_tests.rs` | 139 | Cube sampling with gradients selects the mip by footprint |
| `src/emit/probe_tests.rs` | 74 | `#[ignore]` tool: runs DXIL files from `VICTORY_PROBE` on the card (host compiler semantics and timing) |

---

## 3. The pipeline

```
container bytes
  │ dxbc::Container::parse                              (dxbc.rs)
  ▼
DXIL or ACCC part ── native_programs / fspx_program ──► Vec<NativeProgram>  (native.rs)
  │ translate picks the first Vs|Ps|Cs|Ms (FSSM slot order LS HS ES GS VS PS MS CS)
  ▼
spill::build  = ir::build(patched_code, wave64, vgprs) + spill::user_data
  │   gfx10::decode → blocks by leaders → unspill → effects → link → split_irreducible → rename
  ▼
ir::Program (SSA over register dwords, EXEC/SCC/M0 explicit)
  ├─► structure::structure ───────────────► Structured   (per-lane exits, guards, mask BDDs)
  ├─► resource::track(p, layout, stage, Some(&st)) ─► Resources (descriptor Sources, Addresses, waterfalls, bvh)
  └─► io::raise(p, native) ─────────────────► Io        (inputs, interps, exports)
        (order in translate_plain, emit/mod.rs:422-429: structure first because tracking
         per-lane descriptors needs it, but its error is reported after the others')
  ▼
emit::emit ── retry ladder (wave64 on 32-lane hosts, widen, threading) ─┐
  ▼                                                                      │
emit_program: signatures, interpolation modes, mesh stride → lower::lower(Parts)
  ▼
Lower::run: per IR block in RPO, Lower::inst per instruction → bitcode::Module
  ▼
Shader::finish: Module::optimise(VICTORY_SHADER_OPT & MODULE) → flags fixed → metadata → bitcode::write
  → container (SFI0 ISG1 OSG1 [PSG1] PSV0 DXIL) + MD5 hash
  ▼
HostShader
```

`emit::translate` (`emit/mod.rs:395`) wraps all of this. If the result is
`Unsupported` with `what == lower::FIRST_HALF`, it translates again inside
`lower::with_first_half_pairs`. Only those programs get the second pass, because
turning it on for everything changed 8,551 programs that already translated in order
to fix 9 (comment at `:389-394`).

### 3.1 Decode (`gfx10`)

`gfx10::decode` follows control flow from offset 0, not a linear sweep, because
constant tables follow the code (`gfx10/mod.rs:1-6`). An `Instruction` keeps
`offset`, `encoding`, `opcode`, every `words` dword, and a name resolved once.
`Instruction::fields(wave64)` decodes the operands. Lane-mask operands are 2 dwords
in wave64 and 1 in wave32. An opcode with no name in `opcodes.rs` fails in `ir::build`
with `Unsupported { what: "unknown opcode" }` (`ir.rs:177-180`).

### 3.2 IR (`ir.rs`)

Key types: `Program { wave64, vgprs, blocks, inputs }`, `Block { offset, preds, phis,
insts, exit: Exit }`, `Exit::{Jump, Branch{taken,not_taken}, End, Indirect}`,
`Inst { machine, fields, reads, writes }`, `Phi { reg, value, args }`.

Invariants:

- **One `Inst` per machine instruction.** The exceptions are the scheduling-only
  ones in `DROPPED` (`ir.rs:1299`: `s_nop`, `s_waitcnt*`, `s_clause`, `s_setprio`…)
  and conditional SOPP branches to the fall-through, which become plain jumps
  (`:194-199`). Opcode and decoded fields are kept. Nothing is raised to higher-level
  operations at this stage.
- **SSA over register dwords.** `Reg::{S(code), Scc, V(n)}`. Scalar codes: 0-105
  SGPRs, 106/107 VCC, 108-123 trap temps, 124 M0, 126/127 EXEC, ≥128 `PARKED` codes
  that no machine word can hold (`ir.rs:24-39`). Each dword written is a new `Value`.
  `Program::inputs` lists registers read before any write (user SGPRs, system values,
  fetched attributes).
- **Implicit operands are explicit.** Every vector instruction reads EXEC. A VGPR
  write also reads the old value, because lanes outside EXEC keep it (`:214-219`).
  M0-relative VGPR access names the whole possible range up to `vgprs`.
- **Block 0 is the entry** and may not be branched to. The CFG is made reducible by
  `split_irreducible` (`:1071`), which copies blocks so every loop has one entry.
  Copies keep their `offset`.
- **Unparking** (`unspill`, `:241-268`). When the compiler parks an SGPR in a VGPR
  lane (`v_writelane`/`v_readlane` at a constant lane), the pair becomes `s_mov_b32`
  to and from a spare or `PARKED` code. Later stages then see plain copies and not
  cross-lane operations.
- **Variants** used by retries: `thread_edges` (`:693`), `without_early_outs`
  (`:733`, drops `s_cbranch_execz` to an end-only block), `without_skips(p, deep)`
  (`:780`).

Register effects per instruction live in `ir::effects` (`ir.rs:1336`) and
`writes_scc` (`:1615`). An instruction the IR cannot express returns
`Unsupported { what: <opcode name> }` (for example `s_movrel*`, `s_call_b64`,
`:1364-1369`).

### 3.3 Structurisation / EXEC removal (`structure.rs`)

The result is the program as **one lane** runs it, with no EXEC (module doc,
`structure.rs:1-17`):

- Lane masks (EXEC, compare and carry results, the SGPRs they are saved to) become
  per-lane booleans: reduced ordered BDDs in `Bools` over `Atom::{Lane(v), Uniform(scc),
  Phi(v), Quad(v), Opaque(v), Constant(mask)}` (`:37-67`). The node cap is
  `MAX_NODES = 2^21`. Past it the error is `Isa{"lane conditions too large"}` (`:1414`).
- A branch on a wave-wide mask test (`s_cbranch_execz/vccz`, SCC of a mask op) becomes
  a branch on the lane's own bit. `LaneExit::Branch { when, taken, not_taken }`. A
  condition made only of `Uniform` atoms is a uniform branch.
- A path-sensitive pass proves, for each vector instruction, that EXEC holds in every
  lane that reaches it. `Structured::guards[b][i]` is `None` when reaching the
  instruction is enough, else the residual per-lane condition, which becomes an `if`.
- Pixel helper lanes count as live.
- The CFG keeps its shape. `check_reducible` re-checks it (`:1421`).
- The fixpoint loop `structure_wide` (`:471`) re-runs with joins it found wrong as
  atoms (`unresolved`, `derived_phis`).
- Other outputs: `reach` (what holds on block entry), `masks` (lane bit per mask's
  low-dword value), `highs` (wave64 high → low), `active` (masks set only in lanes
  that reach the writing block, so a ballot there is exact), `phi_masks`,
  `wave_any`, `scc_some`, `wide`.
- `structure_wide(p, &wide)` keeps the closing branches of the blocks in `wide` as
  the console takes them: one decision for the whole wave, with EXEC staying a guard
  after them. It is used by `emit::widen` so the two host waves of a console wave can
  meet at barriers inside the branch.

### 3.4 Resource tracing (`resource.rs`)

`track(p, layout, stage, lanes)` (`resource.rs:204`) walks every instruction:

| Instruction kind | Result |
|---|---|
| `Fields::Image` (`image_bvh*`) | `bvh: (site, BvhNodes::Memory{word1,word3})` if dwords 1 and 3 are constants with type `bvh::DESCRIPTOR_TYPE`, else a traced descriptor + `BvhNodes::Resource` (`:260-285`) |
| `Fields::Image` | `descriptors`: resource (+ sampler) `Source` |
| `Fields::Buffer` (not `*_inv`) / `s_buffer_*` | descriptor `Source` |
| `s_load_*` / `s_store_*` | `scalar: (site, Address)` via `smem_address` |
| `global_*` | `scalar: (site, Address)` via `flat_address`, per-lane dynamic term |

`Source` (`:60`): `Loaded{load,first}` (dwords of a scalar load from a table or heap),
`Built{address,bits,words}` (buffer descriptor built around a root descriptor address),
`Constant(words)` (immediate sampler), `Phi`, `Select` (SCC), `Fetched` (per-lane
`global_load`), `Lanes` (per-lane `v_cndmask`), `LanePhi` (per-lane control-flow join).

`Address { param, offset, dynamic }`: a root parameter index, a constant, and at most
one computed term. `param == CODE` is a constant table after the code read through
`s_getpc_b64`. `param == MEMORY` is a raw title address (BVH builder and traversal).
In that case lowering takes the 64-bit address from the operands. The reasons that
fall back to `MEMORY` are in `RAW` (`:51`). Tracing works by data flow over copies,
agreeing phis and additions (`Tracer::sum`, `origin`, `propagate_copies`). It fails
on the first untraceable access with a reason string, for example
`"an address not from a root parameter"` in a non-compute stage.

**Waterfall loops** (`Waterfall`, `:187`; `Tracer::waterfall`, `:1990`). A
`v_readfirstlane_b32` of a descriptor compared with every lane's own `v_cmp_eq` is
proven to read each lane's own dwords. The loop then runs once with a non-uniform heap
index. The proof needs `lanes` (the structured form). Without it, per-lane descriptors
chosen by control flow are refused. Unproven first-lane offset reads stay as uniform
reads of lane 0.

Other outputs: `params` (entry SGPR value → layout index and dword), `lane_copied`,
`restored_lanes` (lanes overwritten by a uniform scalar, added in `8cc31ab`), and
`root_indexed_views` (`RootIndexedView`: table slot + root constant → heap index,
evaluated by `victory-gpu` at bind time).

**Spilled user data** (`spill.rs`). A compute program whose layout entries start at
user-data index 16 past its user SGPRs reads its parameters through a 40-bit table
pointer in `s0`/`s1`. `spill::user_data` rewrites those loads into entry values, so
tracing and lowering treat them as ordinary parameters. `is_spilled` is also the
crate's test for "this is a BVH builder program". It changes lowering (fences, counted
dispatch) and is used by `victory-gpu/src/pipeline.rs:293`.

### 3.5 I/O raising (`io.rs`)

`raise(p, native)` (`io.rs:197`) maps entry values to `Input`s:

- PS: VGPRs in `SPI_PS_INPUT_ADDR` order, `PS_VGPRS` at `:88`.
- VS: vertex id v0, primitive id v2, instance id v3, fetched attributes.
- CS: thread id v0-v2, group id SGPRs after user data.
- MS: wave info s3, group v5.

It pairs `v_interp_p1`/`p2` into `Interp::Interpolated` and records `Interp::Vertex`
for `v_interp_mov` and `Interp::Weighted` when barycentrics are computed. It lists
`Export`s with `Target::{Color, Depth, Null, Position, Param, Unread, Primitive,
PrimitiveParam}`. A pixel lane whose EXEC bit is clear at the `done` export is
discarded.

### 3.6 Emission and lowering (`emit/`)

`emit_program` (`emit/mod.rs:790`) does the following, then calls
`lower::lower(&Parts{native,p,st,io,res,inputs,outputs,prims,mesh_stride})`:

1. Builds signatures from ISG1/OSG1/PSG1.
2. Sets interpolation modes. Defaults are as DXC declares them. VS outputs take theirs
   from PSV0 (`:824`). A conservative depth output makes the position centroid.
3. Drops `SV_PrimitiveID` and `SV_CullPrimitive` rows from PSG1.
4. Computes the mesh group stride from PSV0 (`:849-860`).

**`lower.rs` organisation:**

- `lower` → `lower_counted(parts, counted)` (`:113`). This retries while a phi has no
  DXIL form (`Stop::Retry`). The second time round, that phi is `Missing`.
  `counted_in_threads` (`:99`) makes BVH builder 1-D dispatches stop lanes past
  `USER_DISPATCH_THREADS_X`.
- `Lower::new` (`:1391`) decides the stage-wide policy: thread counts, LDS, **wave
  size** (§4), `fence_only` barriers, mesh barrier removal, `MaskData`, and
  first-half interpolation pairs.
- `Lower::run` (`:2331`) does the following in order:
  1. counted-dispatch prologue
  2. one DXIL block per IR block in RPO
  3. a loop-iteration counter phi at every loop header (`LOOP_LIMIT` = 2^20 per lane
     per loop entry, `:401`; overridable via `loop_limit()`)
  4. whole-wave sections (`find_sections` → `emit/wave.rs`)
  5. inputs, mesh prologue, then per block: phis, `Lower::inst` for each
     instruction, `Lower::exit`
  6. `fill_phis`
  7. `Shader::finish`
- Values: every register dword is an `i32` and SCC an `i1` (`Val::{I32, Bit,
  Missing(why)}`). Float ops bitcast. A `Missing` value only fails the program when
  something **with an effect** needs it (`:5395-5404`): exports, stores, atomics,
  barriers, DS writes, branches. Otherwise the instruction's writes become
  `Missing(why)` and lowering continues.
- Guards: `Lower::guard(b,i)` reads `Structured::guards`. `begin_guarded` and
  `end_guarded` wrap the lowered instruction in a per-lane `if` and join the old
  register values.

**Opcode dispatch** is `Lower::inst` (`lower.rs:5294`). It matches on the `Fields`
variant, then the per-family function matches on the **opcode name string**:

| `Fields` | Function | Notes |
|---|---|---|
| `Sop` | `sop` (`:5514`) | scalar ALU, mask ops, saveexec |
| `Sopp` `s_barrier` | inline (`:5349-5370`) | DXIL `barrier` mode by `group_sync()`, `fence_only`, `no_barrier` |
| `Sopp` other | no-op | branches are handled by `exit` |
| `Smem` | `smem` (`:6121`), `descriptor_data`, `code_smem`, `memory_smem` | constant buffers, raw loads, descriptors read as data |
| `Valu` `v_interp_*` / `Vintrp` | `interp` (`:10392`) | |
| `Valu` | `valu` (`:7721`) → `valu_op` (`:7910`) → `valu16` (`:9255`), `packed16` (`v_pk_*`, `:9661`), `integer` (`:9774`), `compare` (`:10199`), `dpp` (`:7781`) | big `match name` |
| `Image` `image_bvh*` | `bvh` in `emit/intersect.rs:64` | refused at once on failure (it has no side effect, but silently missing tests broke two trace programs) |
| `Image` | `image` (`:10721`) | sampling, loads, stores, atomics, cube helpers |
| `Buffer` | `buffer` (`:11569`), `format_load` (`:6794`) | MUBUF/MTBUF, typed formats converted in code |
| `Exp` | `export` (`:12105`) | |
| `Ds` | `ds` (`:4892`) | LDS, swizzle, (b)permute, float min/max |
| `Flat` `global_*` | `flat` (`:1809`) → `memory_flat` (`:3252`) / `Lower::memory` (`:3113`) | memory by address via `SLOT_MEMORY` |
| `Flat` other | `Err("flat memory")` | |

BVH builder atomics (`buffer_atomic*`/`global_atomic*` in a spilled program) get a
memory fence before and after when `builder_fences()` is on (`:5341-5346`).

**`Shader` and bitcode.** `emit/shader.rs` owns a `bitcode::Module` (`s.m`) and types
(`s.t`). `Shader::op(block, name, attr, ret, opcode, args)` declares and calls
`dx.op.*`. `Shader::finish` (`shader.rs:530`) does the following:

1. Fixes pixel input interpolation modes.
2. Sizes user data.
3. Runs `Module::optimise(shader_opt() & opt::MODULE)` (`:557`).
4. Clears feature flags whose last use was optimised away. The validator compares
   them.
5. Writes resources, metadata and the container.

`bitcode.rs` keeps values symbolic (`V`) and numbers them at write time: functions,
then module-level constants, then `main`'s instructions in block order. Records are
unabbreviated, with LLVM 3.7 codes.

**Optimiser passes** (`emit/bitcode/opt.rs:41-70`). All are claimed exact. The bit
values are `VICTORY_SHADER_OPT` bits:

| Bit | Name | Effect |
|---|---|---|
| 1 | `DEAD` | drop effect-free unread instructions |
| 2 | `FOLD` | integer constant folding and identities (never float), plus `prune` of one-way branches |
| 4 | `REUSE` | CSE of effect-free ops dominated by an equal one |
| 8 | `FACTS` | branch-condition facts in blocks only one side reaches |
| 16 | `LOADS` | same-place reads within a block with no intervening effect |
| 32 | `FIRST` | *lowering-time*: first-lane read of a split wave64 without the "which half has a lane" exchange when EXEC is full (`lower.rs:8848`) |
| 64 | `WIDE` | *lowering-time*: one raw load call per four dwords of one console load (`lower.rs:6083`). Off by default |

`MODULE` = 1|2|4|8|16, `ALL` = 127, `DEFAULT` = `ALL & !WIDE` = 63. `optimise`
(`opt.rs:231`) iterates merge/prune up to 4 rounds, then `drop_dead`.

---

## 4. Vendor and stage specifics

### Wave size (the "Wave32 lowering")

The host facts come from `victory-gpu/src/host.rs:932-942`:
`set_host_max_wave(WaveLaneCountMax)`, and
`set_host_rdna(amd && min == 32 && max == 64)`. `host_max_wave() == 0` means unknown,
and then no host-specific forms are used.

| Stage / case | What the translation declares or does |
|---|---|
| PS, VS | `MaskData::Host` (`lower.rs:408-422`): no `WaveSize`. Masks read as numbers are allowed only for wave-size-agnostic uses (counts, first lane, count below, equality). |
| MS | `MaskData::None`. No wave size; only `uniform_test` mask uses. |
| CS, console wave32 | `WaveSize(32)` only if `console_lanes` (DPP, permlane, swizzle, mbcnt, readlane/writelane, mask as data, lane-constant masks, wide branches; `lower.rs:1423-1434`) or one wave with LDS. |
| CS, console wave64, host max 64 (AMD RDNA) | `WaveSize(64)` under the same conditions. |
| CS, console wave64, host max 32 (NVIDIA) | `lower.rs:1446-1547`. With no half-crossing ops and (no LDS or group ≤ 32), it runs as 32-lane waves with threads relabelled (`halves`, `wave_base` counter in one LDS dword). Otherwise, if LDS accesses are all in all-lane blocks or `lds_meetings` can order them, it splits into two host waves with barriers (`split_lds`). Crossing ops then go through `exchange` (2 LDS dwords per console wave, `exchange_halves` with group barriers), `lane_uniform` and `own_bits`. Otherwise the wave stays 64, or `need(block)` records the blocks that stop it. |

`VICTORY_WAVE_WHY=1` prints `WAVE64-WHY …` reasons (`lower.rs:1502-1538`).

**The retry ladder in `emit::emit`** (`emit/mod.rs:444-683`) runs when the program is
CS, wave64, `host_max_wave()==32`, and the first attempt still needs 64 lanes or
failed with a `needs_every_lane` reason. The steps, in order:

1. Forms `without early-outs`, then `without skips`.
2. Exact local waves (`with_local_waves`, each host wave its own wave), only where
   `wave_local` proves the allocation idiom.
3. `without skips, scalars followed` (deep; `VICTORY_WAVE_DEEP_SKIPS`).
4. `widen` (`:693`): `structure_wide` around the blocks lowering reports via
   `take_needs`. This needs one console wave per group (64 threads). It is on with
   `VICTORY_WAVE_WIDE=1`, by default for two hard-coded container digests
   (`:545-551`), and for the "driver loop" case (an ACCC container failing with
   `DIVERGENT_MASK_EXCHANGE`, a single `s_andn2_wrexec_b64` latch).
5. Approximate local waves (half-wave results), unless `VICTORY_WAVE_LOCAL=idiom`.
6. `with_local_waves_any` (approximate, every mask per host wave).

Separately, a CS failing with `LANE_MASK` tries `widen` unless
`VICTORY_WAVE_WIDE=0` (`:645-658`). If the result still wants wave 64 on a 32 host,
it becomes `Unsupported("no wave32 lowering for this wave64 program")` (`:659-669`).
Finally, any `Unsupported` result is retried once after `threading::constant_edges`
jump threading (`:670-683`).

**Surprising:** the two digests at `emit/mod.rs:548-549` are per-title special cases
added in `8cc31ab` ("archived kernels … validated wave32 PSOs on RTX").

### Other vendor-dependent behaviour

- `set_view_heap_limit(null_slot)`. NVIDIA caps the view heap (at 1,000,000 views per
  the doc comment). View indices at or past the limit read the null slot
  (`shader.rs:149-164`).
- `set_raw_views(first)`: raw "twin" views at `first + i` for picked buffers. This
  protects against wild raw reads on AMD (`shader.rs:166-195`).
- `rdna_float()` (`VICTORY_RDNA_FLOAT`) applies only when `set_host_rdna(true)`. It
  writes FMax/FMin, legacy mul, med3 and fract as single host ops (`emit/mod.rs:226-263`).
- `group_sync()` exists because NVIDIA's barrier waits for every thread and hangs the
  card when some never arrive (`emit/mod.rs:160-185`).
- **16-bit float rounding** (`VICTORY_F16_NEAREST=1`; off by default): the host's `f32tof16`
  rounds toward zero on NVIDIA and on WARP; the console rounds 16-bit results as
  `MODE.FP_ROUND[3:2]` says, to nearest even unless a program sets toward zero.
  `f16_round_toward_zero` (`lower.rs`) follows `s_setreg_imm32_b32
  hwreg(HW_REG_MODE, ...)` writes of those bits through the blocks (start: to
  nearest; a register value, the directed modes and joins that disagree: to
  nearest), and `Lower::console_half` rounds every 16-bit result (VOP1/2/3
  `_f16`, `v_cvt_f16_f32`, packed `v_pk_*_f16`, `v_fma_mixlo/hi_f16`): the f32
  is rounded in place to the 10 fraction bits a normal half keeps (bias and
  mask on its bits, about a dozen integer operations, a NaN kept, past 65504
  infinity) so the host's one conversion has nothing left to round
  (*shortcut*: subnormal halves keep the host's rounding of their last place).
  The exact `f16_nearest` (four conversions a result) made the title's frames
  four times as long on the card when used for every result; it stays for the
  16-bit wave sums. `v_cvt_pkrtz_f16_f32` is always toward zero. 6,301 of the title's ~25,700 programs bracket packing code
  with `hwreg(HW_REG_MODE, 2, 2)` writes of 3 and 0 (9,417 writes). Before
  2026-11-27 every result went through the host's conversion (toward zero):
  the title's upscaler (AMD FSR, `amd_fidelityfx_upscaler_xbox.dll`) computes
  its network's inputs in 16 bits and its history ran away on the hair (the
  hotspot). With the switch no launch showed the hotspot (4 of 4: twice
  everything to nearest, twice as `MODE` says), but 3 of the 4 were posterised
  for the whole session, which none of about 12 launches without it (ray
  tracing on) was; so it is off by default until that is understood
  (`docs/bug-journal.md`, 2026-11-27).

### Mesh shaders

- Output is `ms_6_6`. The mesh pipeline has no input layout, and the host calls
  `DispatchMesh(x,1,1)` (`emit/mod.rs:66-75`).
- The group id arrives multiplied by `max(threads, vertices, primitives)`. Threads
  come from PSV0 v2+, else the lane count (marked `shortcut` at `:847`).
- `mesh_prologue` (`lower.rs:4606`) lowers the maxima to the lane count when outputs
  exceed `MESH_OUTPUT_LIMIT` (32 KiB). With `VICTORY_MESH_OUTPUT_STRICT`, those
  programs are refused instead.
- Barriers are removed when there is no LDS, or when the block is dominated by the
  one-lane `s_sendmsg GS_ALLOC_REQ` (`lower.rs:1584-1627`).

### Pixel specifics

- Export patches are applied (`patched_code`).
- `ordered_section` (POPS, `lower.rs:150`).
- Helper lanes read their own value where the console reads the first lane's
  (`helper_own`, `VICTORY_HELPER_OWN`).
- The first-half interpolation retry (above).
- Comparison samplers set filter bit 0x80 (`emit/mod.rs:877-888`).
- **Early depth.** A pixel program whose `DB_SHADER_CONTROL` (native stage
  data `E+0x138`) has `DEPTH_BEFORE_SHADER` (bit 12, the console compiler's
  `[earlydepthstencil]`) declares `ForceEarlyDepthStencil` (DXIL shader flag
  bit 3, `flags::EARLY_DEPTH`; `emit::early_depth`, set in `Lower::new`),
  unless it writes depth, stencil or coverage. Without it the host tests
  depth after a program that writes unordered views, runs it for pixels
  the console never runs, and keeps its stores (and a killed pixel writes no
  depth where the console's did). In the title 3,009 of 12,056 pixel
  programs have the bit (`5200`/`0x1450` for the glass pre-pass
  `PS_AlphaPrepass`). Until 2026-11-26 the register was read and not used:
  the glass pre-pass stored the window's depth and motion over the hair in
  front of it, and the upscaler's history ran away there (the hotspot,
  `docs/bug-journal.md`). `VICTORY_EARLY_DEPTH=0` turns it off (key).

### Rectangle lists

`rect_list_gs` (`emit/rect.rs`, `gs_6_6`) is put between the translated VS and the PS
for topology type 19. It completes the fourth corner. Flat values come from the first
vertex (marked `shortcut`).

### Ray tracing

The title uses no DXR. See `docs/fork-ray-tracing.md`.

- `bvh.rs` is the bit-exact CPU reference for the node test.
- `emit/intersect.rs` emits the same float ops in the same order. It reads nodes
  through `Lower::memory` (title memory by address) or a host buffer
  (`BvhNodes::Resource`).
- The BVH builder programs are spilled-user-data compute programs (`spill.rs`). They
  get counted dispatch, fences around atomics (`builder_fences`), and bounded
  root-view access (`builder_bounds`, `USER_ROOT_ADDRESS`, `USER_VIEW_BIAS`).
- `ShaderKind` knows the DXR kinds (`dxil.rs:20-38`), but `translate` refuses any
  container without a VS/PS/CS/MS native program.

---

## 5. Unsupported stages

`translate` (and therefore the runtime) refuses LS/HS/ES/GS programs, i.e.
tessellation and geometry: `"a container without a vertex, pixel, compute or mesh
program"` (`emit/mod.rs:418-421`). `victory translate` builds IR, structure and
resources for every program in a container, but runs `io::raise` and `emit::emit`
only for VS, PS, CS and MS (`apps/victory/src/translate.rs:571-605`). Other stages
count as "translated" (IR built) and are never emitted.

---

## 6. Caching

The cache is in `victory-gpu/src/shader_cache.rs`. This crate only provides the
inputs to its key.

- **Location:** `<cache_dir>/shaders`, where `cache_dir` is `VICTORY_CACHE_DIR`, the
  config file, or `<user dir>/victory/cache` (`victory-core/src/config.rs:88,112`).
  It is set by `run.rs:65` and `translate.rs:178,399`. Entries go through
  `victory-pipeline`'s content-addressed `Cache` (converter id `"shader.dxil"`,
  `version()` fixed at 1) and are encoded as `VSH1` records.
- **Key:** `BLAKE3(fixed ‖ host_settings())`, prepended to the container bytes. The
  `fixed` part is:
  - `env!("VICTORY_TRANSLATOR")`: `victory-gpu/build.rs`, a BLAKE3 over every file in
    `crates/victory-shader/src` (tests included, CR stripped),
    `victory-shader/Cargo.toml`, `victory-gpu/src/shader_cache.rs`, `Cargo.lock`,
    `rust-toolchain.toml`.
  - `loop_limit()`, `group_sync()`.
  - A **hand-maintained list** of switch values (`shader_cache.rs:65-78`):
    WAVE_WIDE, WAVE_DEEP_SKIPS, WAVE_OWN_BITS, WAVE_LOCAL, F16_NEAREST, HELPER_OWN,
    FIRSTLANE_OWN, FIRST_HALF, STRICT_JOINS, MESH_OUTPUT_STRICT, BVH_FENCES,
    BVH_BOUNDS.
  - `shader_opt()`.

  `host_settings()` is `[host_max_wave, view_heap_limit, raw_views, host_rdna bits]`
  (`lower.rs:1127`). `GROUP_SYNC`, `SHADER_LOOP_LIMIT`, `SHADER_OPT` and `RDNA_FLOAT`
  enter by value.
- **Invalidation:** automatic on any edit to the translator's sources. There is no
  version number to bump. A new switch that changes output must be added to that list
  (or enter through a hashed getter). A new `HostShader` field must be added to
  `encode`/`decode` in `shader_cache.rs` (module doc `:8-10`). An unreadable entry is
  deleted and translated again.
- **Stale or unused in this crate:** `emit::translation_fingerprint()`
  (`emit/mod.rs:283`), together with `build.rs`'s `VICTORY_SHADER_SOURCE_HASH` and
  `VICTORY_SHADER_SWITCHES`, has **no callers** in the workspace. `build.rs`'s doc says
  it is the cache key, which is not what happens. If it were adopted, its automatic
  switch list would also include diagnostic and test-only switches (`WAVE_WHY`,
  `DESC_WHY`, `WARP`, `PC_DXC`...), so changing them would cause needless cache misses.

---

## 7. `VICTORY_*` switches read in this crate

Values are read once per process (`OnceLock`) unless noted. "Key" means the value is
part of the runtime cache key (§6).

| Switch | Values (default) | Effect | Where |
|---|---|---|---|
| `VICTORY_SHADER_OPT` | bitset, dec or `0x` hex (63) | optimiser and lowering pass bits (§3.6). `0` = none, `127` adds `WIDE` | `emit/mod.rs:223`; key |
| `VICTORY_SHADER_LOOP_LIMIT` | ≥1, dec/hex (2^20) | per-lane iteration cap of translated loops. **Only with the `diagnostics` feature** | `emit/mod.rs:145`; key |
| `VICTORY_GROUP_SYNC` | `0` / `1` / `2` (2) | which barriers wait: 2 all; 1 only in all-lane blocks of programs no lane can leave early; 0 none (wave64-split programs then compute wrong values) | `emit/mod.rs:180`; key |
| `VICTORY_HELPER_OWN` | `0` off (on) | pixel helper lanes read their own value at first-lane reads | `emit/mod.rs:157`; key |
| `VICTORY_BVH_FENCES` | `0` off (on) | fences around BVH builder atomics | `emit/mod.rs:192`; key |
| `VICTORY_BVH_BOUNDS` | `0` off (on) | bound builder accesses near root views; implausible header sizes count as none | `emit/mod.rs:201`; key |
| `VICTORY_EARLY_DEPTH` | `0` off (on) | pixel programs the console tests before they run declare `[earlydepthstencil]` | `emit/mod.rs` `early_depth_on`; key |
| `VICTORY_RDNA_FLOAT` | bitset of `rdna::{MAX_MIN 1, LEGACY 2, MED3 4, FRACT 8}` (15) | single host ops for those console float ops, on RDNA hosts only | `emit/mod.rs:243`; key via `host_settings` |
| `VICTORY_WAVE_LOCAL` | `0` off, `idiom` strict (on) | local-wave retries; `idiom` refuses approximate forms | `emit/mod.rs:470,476`; key |
| `VICTORY_WAVE_WIDE` | `1` on, `0` off (unset: off, except 2 digests and the driver-loop case) | `widen` retries. `0` also disables the `LANE_MASK` widen | `emit/mod.rs:542,649`; key |
| `VICTORY_WAVE_DEEP_SKIPS` | `0` off (on) | `without_skips(deep)` retry | `emit/mod.rs:569`; key |
| `VICTORY_WAVE_OWN_BITS` | `0` off (on) | `own_bits` proof for wave ORs in split wave64 | `lower.rs:1121`; key |
| `VICTORY_FIRST_HALF` | `0` off (on) | during the FIRST_HALF retry, evaluate first halves as pairs | `lower.rs:1673`; key |
| `VICTORY_FIRSTLANE_OWN` | `1` on (off) | *approximate*: first-lane read where not every lane runs takes the host wave's own first lane | `lower.rs:8841`; key |
| `VICTORY_F16_NEAREST` | `1` on (off) | 16-bit float results rounded as the console's `MODE` says (to nearest, or toward zero where a program sets it), not by the host's conversion (toward zero); cures the upscaler hotspot, posterised 3 of 4 launches | `lower.rs` `f16_nearest_on`; key |
| `VICTORY_STRICT_JOINS` | set (off) | refuse joins fed by values with no DXIL form instead of silently zero (40 programs stop with it) | `lower.rs:12622`; key |
| `VICTORY_MESH_OUTPUT_STRICT` | set (off) | refuse mesh shaders over 32 KiB of outputs even at lane-count maxima | `lower.rs:4630`; key |
| `VICTORY_WAVE_WHY` | set (off) | stderr `WAVE64-WHY`/`-RETRY`/`-FORM`/`-WIDE` diagnostics. Read uncached each time | `lower.rs:1502,1706`; `emit/mod.rs:458,651` |
| `VICTORY_DESC_WHY` | set (off) | stderr `DESC-WHY` when a descriptor is read only as data | `lower.rs:6469` |
| `VICTORY_SHADER_CENSUS` | any (off) | count emitted instructions by kind (`census::take`). The app also uses the value as an output file | `emit/bitcode/census.rs:63` |
| `VICTORY_DXIL` | path | `dxil.dll` used by `Validator` | `emit/validator.rs:122` |
| `VICTORY_WARP` | `1` | `Host::new` uses WARP | `emit/host.rs:68`, `diff_tests.rs:1189,1220` |
| `VICTORY_D3D12_DEBUG` | set | D3D12 debug layer in `Host` | `emit/host.rs:73` |
| `VICTORY_PC_DXC` | path (SDK `dxc.exe`) | PC compiler for tests (`dump_dxil`, diff tests) | `emit/mod.rs:1061`, `diff_tests.rs:656` |
| `VICTORY_CONSOLE_DXC` | path | console SDK `dxc.exe`; diff tests skip without it | `diff_tests.rs:655,682` |
| `VICTORY_DIFF_ONLY` | case name | run one differential case | `diff_tests.rs:778,900,1188` |
| `VICTORY_EMIT_DUMP` | dir | diff tests write console and our DXIL | `diff_tests.rs:793,914,1196,1207` |
| `VICTORY_REF_PS` / `VICTORY_REF_VS` | file | reference DXIL in `stub_pipeline_is_created` | `emit/mod.rs:1690-1691` |
| `VICTORY_PROBE` | dir | `probe_tests` tool input | `emit/probe_tests.rs:15` |

Read by callers but affecting this crate: `VICTORY_HOST_WAVE` (`apps/victory/src/main.rs:208`) and `VICTORY_HOST_MAX_WAVE` (`translate.rs:250`)
are **two names** for `set_host_max_wave` in the offline command. `--warm` only
honours the first. Also `VICTORY_HOST_RDNA`, `VICTORY_VIEW_LIMIT`,
`VICTORY_TRANSLATE_ONLY`, `VICTORY_PAIRS`, `VICTORY_FAIL_DUMP`, `VICTORY_DUMP_MATCH`,
`VICTORY_EMIT_DUMP` (with `--list`) in `apps/victory/src/translate.rs`.

---

## 8. Failure modes and debugging

**Where a failure comes from:**

| Stage | Error | Typical `what` / `reason` |
|---|---|---|
| parse | `NotDxbc`, `Malformed(..)` | `"unknown FSSM version"`, `"no DXIL part"` |
| decode / IR | `Isa{offset,reason}`, `Unsupported{what:"unknown opcode"}` or the opcode name | `"jump into a block"`, `"falls off the code"`, `"s_movrel_b32"` |
| structure | `Unsupported("irreducible control flow")`, `Isa("lane conditions too large")` | |
| resource | `Unsupported` with a tracing reason | `"an address not from a root parameter"`, waterfall refusals |
| io | `Unsupported` | stage data not read, unknown constructs |
| signature | `Unsupported` | `"a minimum-precision signature element"`, `"a system value in the signature"` |
| lower | `Unsupported{offset, what: Why}` raised when an effect needs a `Missing` value | `lower::{LANE_MASK, MASK_ELSEWHERE, DIVERGENT_MASK_EXCHANGE, FIRST_HALF}`, `"a phi that does not settle"`, `"no wave32 lowering for this wave64 program"`, `"flat memory"` |

At run time, `victory-gpu` gets the error string from `shader_cache::host_shader`.
What the pipeline then does is outside this crate (see `victory-gpu/src/pipeline.rs`).

**`victory translate <paths> [--validate] [--pipelines] [--list] [--jobs N]`**
(`apps/victory/src/translate.rs:236`) walks files, extracts containers (including
SGD5 records) and de-duplicates them. For every native program it runs
`spill::build`, `structure`, `resource::track`, `io::raise` and `emit::emit`
separately. It prints totals per stage (`structured`, `resources`, `I/O`, `DXIL`
emitted / validated / pipelines per VS+PS, CS, MS). It then prints a **tally of
failure reasons**, sorted by count, each with one example: `  <n>  <stage>: <what>
(e.g. <file> <stage> at <offset>)`. The stage prefixes are `unsupported:`,
`resources:`, `I/O:`, `structure:`, `emit:`, `validator:` and `pipeline:`.

Caveats:

- It calls `emit::emit`, not `emit::translate`, so the FIRST_HALF retry is **not**
  applied. `emit: the first half of an interpolation` counts there overstate runtime
  failures.
- The host facts are defaults (wave 0, no RDNA) unless you set
  `VICTORY_HOST_WAVE`/`VICTORY_HOST_MAX_WAVE=32`, `VICTORY_HOST_RDNA=1` and
  `VICTORY_VIEW_LIMIT` to match the card in the run's log.

**Debugging a mistranslated shader** (all dumps must be outside the repo: they are
game-derived, and `ensure_outside_repo` enforces this for `VICTORY_EMIT_DUMP`):

1. Isolate it. Use `VICTORY_TRANSLATE_ONLY=<hash prefix>` with `victory translate --list`.
   `--list` prints a 16-hex BLAKE3 of the `HostShader` per program, which you can diff
   before and after a change to see which shaders the change touched.
2. Dump it. `VICTORY_EMIT_DUMP=<dir>` with `--list` writes every emitted `.dxil`, and
   validator failures write `<reason>_<n>.dxil/.txt` (first 3 per reason).
   `VICTORY_FAIL_DUMP` saves failing containers. Read DXIL with `dxc -dumpbin`.
3. Read the source. `victory shader-info --disasm <file>` prints the GFX10 code in SP3
   syntax (`gfx10::sp3`).
4. Bisect the translator. `VICTORY_SHADER_OPT=0` rules the optimiser in or out (every
   pass is meant to be exact). `VICTORY_WAVE_WHY=1` and `VICTORY_DESC_WHY=1` explain
   choices. Toggle the wave switches and `VICTORY_HELPER_OWN`/`VICTORY_GROUP_SYNC` one
   at a time; the cache keys on them, so no cache clearing is needed.
5. Diff variants on a capture (`docs/replay.md`). `victory replay --retranslate <dir>`
   writes this build's translations. `--shaders A --compare --shaders-b B` runs two
   sets turn by turn. **Note:** `--retranslate` hard-codes `set_host_max_wave(64)` and
   `set_host_rdna(true)` (`apps/victory/src/replay.rs:468-469`), so it reproduces the
   AMD form, not the NVIDIA one.
6. Reduce it to a test. Hand-encode the offending sequence in a synthetic program
   (§9) and compare GPU output with expected values. If the console SDK compiler is
   available, write an HLSL case in `diff_tests.rs`.

---

## 9. How-to recipes

**Add an instruction:**

1. **Name.** Check the opcode is in `gfx10/opcodes.rs`. That table is generated from
   Mesa, so regenerate it rather than hand-patching if it is missing.
2. **Operands.** Make sure `gfx10/fields.rs` decodes the encoding and that the
   register widths for the name are right (`salu_dwords`, `valu_dwords`,
   `data_dwords`, `image_addr_dwords`, `:729-789`).
3. **Effects.** In `ir::effects` (`ir.rs:1336`), declare extra reads and writes (SCC,
   EXEC, M0, the old destination) and `writes_scc` (`:1615`). Scheduling-only
   instructions go in `DROPPED`.
4. **Masks / EXEC.** If it reads or writes lane masks or EXEC, extend the mask
   handling in `structure.rs` (`SopMasks`, `Lanes`).
5. **Descriptors / memory.** If it reads a descriptor or memory, make sure its
   `Fields` arm in `resource::track` (`:259-326`) covers it.
6. **Lowering.** Add a `match name` arm in the right family function (§3.6 table).
   Return `Err("reason")` for unsupported operand forms: this makes the result
   `Missing`, and only effects then fail.
7. **Cross-lane.** If it crosses lanes, add it to `console_lanes` (`lower.rs:1423`)
   and, if it crosses 32-lane halves, to `crosses` (`:1447`). Teach `emit/wave.rs`'s
   simulator if it appears in whole-wave sections.
8. **Test.** Add a VALU case to `valu_tests.rs` (GPU + WARP), or a `lowers_valu`
   smoke test in `lower.rs` tests.

**Add a resource pattern** (a new way code computes a descriptor or address):

1. Extend `Tracer` (`resource.rs:398`). Usually this means `sum`/`origin` for a new
   arithmetic form, `classify` (`:1151`) for a new `Source`, or `by_address`/`RAW`
   for a new raw-address fallback.
2. If you add a `Source` variant, handle it in `Source::leaves` and in lowering
   (`Lower::handle` `:7193`, `heap_index` `:7683`, `lane_heap_index` `:7579`,
   `descriptor_phi` `:7338`).
3. Add a `resource::tests` case using `track_words(words, layout, stage)`
   (`resource.rs:2489`). For per-lane cases, add a `waterfall_tests.rs` run.
4. If the host must supply something new, add a `RootType::Other` code or slot
   constant in `emit/shader.rs`. Wire it in `victory-gpu/src/draw.rs` and
   `pipeline/host.rs`.

**Bump the cache version:** not needed. Any change under `crates/victory-shader/src`
(or its `Cargo.toml`) changes `VICTORY_TRANSLATOR`. You must act in two cases:

- A new `VICTORY_*` switch that changes output: add it to the list in
  `victory-gpu/src/shader_cache.rs:65-78`.
- A new `HostShader` field: update `encode`/`decode` there. Changing `MAGIC`
  (`VSH1`) or `Translate::version()` would also invalidate the cache, but is not
  required.

**Add a test:**

- Pure IR/structure/resource: a `#[test]` in that module, using hand-encoded words
  (`ir.rs` tests, `structure.rs` tests, `resource::tests::track_words`).
- Lowering without a GPU: `lower.rs` tests `lowers(words, rsrc2)`,
  `lower_with(words, layout, used)` and `lower_wave(..)` (`lower.rs:12884-12966`).
- GPU execution: follow `valu_tests::build_cs` (`valu_tests.rs:102`) and
  `run_cases`. Take `valu_tests::ONE_AT_A_TIME` (concurrent GPU tests hang or
  crash WARP). Validate with `Validator` when present. Skip gracefully when
  `Host::new` fails.
- Test-only per-thread overrides exist: `TEST_HOST_WAVE`, `TEST_RDNA`,
  `TEST_F16_NEAREST` (`lower.rs`) and `RAW_VIEWS_HERE` (`shader.rs:182`). Use them
  instead of the process-wide setters.
- Gate GPU and D3D12 modules with `#[cfg(all(test, windows))]` and register them in
  `emit/mod.rs:83-113`.

---

## 10. Tests and how to run them

Per-file `#[test]` counts: bvh 16, dxbc 2, dxil 3, io 7, ir 10, native 12, resource 38,
spill 3, structure 13, gfx10 16, emit/mod 33, lower 16, bitcode 7, opt 11, wave 9,
own_bits 6, rect 3, sampler 4, container 2, threading 2, valu 38, size 8, wave64 9,
intersect 6, waterfall 6, diff 4, permlane 2, cube_lod 1, probe 1 (ignored).

```
cargo test -p victory-shader                       # everything; emit GPU tests need Windows
cargo test -p victory-shader -- --ignored probe    # with VICTORY_PROBE=<dir of .dxil>
cargo bench -p victory-shader --bench guard_runs   # structurisation timing
```

- GPU tests run on the default adapter and on WARP. Either is skipped with
  "no SM 6.6 … device; skipped" when it is missing.
- DXIL validation runs only when `dxil.dll` is found (`VICTORY_DXIL` or the newest
  Windows SDK). The tests also check that our container hash equals the validator's.
- Differential tests need `VICTORY_CONSOLE_DXC` and silently skip without it.
- Non-Windows builds compile without `emit::host`, `emit::validator` and the GPU test
  modules.

---

## 11. Known gaps, shortcuts, oddities (verified in code)

- `shortcut:` comments mark accepted approximations. Grep for them. Examples:
  - first-lane own read (`VICTORY_FIRSTLANE_OWN`)
  - local waves' per-half results (`lower.rs:1085-1088`)
  - rect-list flat vertex
  - mesh group stride for old PSV0
  - LDS two-writes assumption (`lower.rs:943`)
  - silent zero for joins of missing values unless `VICTORY_STRICT_JOINS`
    (`lower.rs:12612-12619`)
- Per-container special cases by digest exist (`emit/mod.rs:545-550`).
- Lowering state is partly thread-local (`NEEDS`, `LOCAL_WAVES`, `LOCAL_ANY`,
  `FIRST_HALF_PAIRS`) and partly process-global atomics (host facts). Host facts must
  be set before the first translation, and the cache key reads them at key time.
- Source comments cite "decision 0003" and "decision 0007" everywhere, but no
  decision documents exist in the repository.

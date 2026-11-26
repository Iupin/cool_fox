# victory-pe

Read-only analysis of PE32+ (x86-64) images and COFF import libraries
(`.lib`). Xbox Series titles are ordinary x86-64 PE32+ images (Zen 2), so one
parser serves titles, console system DLLs and host binaries. The crate
**never modifies bytes**; patching lives in `victory-loader`, which uses the
raw header offsets exported from `layout`.

## Dependencies and features

| Kind | What |
|------|------|
| deps | `object` (the actual PE/COFF parser), `thiserror` |
| features | none |
| platform | portable (no `cfg(windows)` code; tests run anywhere) |

Users: `victory-loader` (`prepare.rs`, `resolve.rs`, `tls.rs`, `windows.rs`),
`victory-hle` (`ImportSymbol` in `lib.rs`, `host.rs`), `apps/victory`
(`census.rs`, `inspect.rs`).

## Module map

| File | Role |
|------|------|
| `src/lib.rs` | Re-exports, `PeError` (`Unsupported` / `Malformed`), `MACHINE_AMD64 = 0x8664`, `From<object::read::Error>` → `Malformed`. |
| `src/image.rs` | `ImageInfo::parse`: one pass over a PE32+ image → sections, imports, delay imports, exports, TLS, CodeView, flags. |
| `src/implib.rs` | `ImportLibrary::parse`: COFF archive of short-format import objects → sorted, deduped `dll!export` list. |
| `src/layout.rs` | Raw byte offsets of header fields, data-directory indices, flag constants, bounds-checked `read_u32`/`read_u64`. |

## Public API

```text
ImageInfo::parse(&[u8]) -> Result<ImageInfo, PeError>      image.rs:126
  .is_dll()  .rva_to_file_offset(rva)  .import_count()  .subsystem_name()
ImageInfo { machine, characteristics, timestamp, subsystem, dll_characteristics,
            image_base, size_of_image, entry_rva, stack_reserve, stack_commit,
            sections: Vec<SectionInfo>, imports, delay_imports: Vec<ImportModule>,
            exports: Vec<ExportInfo>, tls: Option<TlsInfo>,
            has_relocations, has_exception_table, has_load_config,
            codeview: Option<CodeViewInfo> }
ImportModule { dll /* case preserved */, entries: Vec<ImportEntry> }
ImportEntry  { symbol: ImportSymbol, iat_slot_rva /* 8-byte slot to fill */ }
ImportSymbol::Name { hint, name } | ImportSymbol::Ordinal(u16)
   Display: the name, or "#<ordinal>"  <- the key victory-hle's registry uses
ExportInfo   { name: Option, ordinal, rva: Option /* None = forwarded */, forward }
TlsInfo      { directory_rva, callbacks_va /* VA at preferred base */, callback_rvas }
CodeViewInfo { pdb_path, guid, age }  .symbol_key() -> symbol-server key
implib::ImportLibrary::parse(&[u8]) -> { imports: Vec<LibImport>, other_members }
implib::LibImport { dll, export: ImportSymbol, kind: Code|Data|Const }
layout::{file_header_offset, optional_header_offset, data_directory_offset,
         read_u32, read_u64, consts, dir::*, file_flags::*, dll_flags::*, subsystem::*}
```

## Behaviour worth knowing

- **Rejects non-PE64 and non-AMD64** (`image.rs:128`, `:139`) with
  `Unsupported`.
- **Section raw data is bounds-checked** against the file before anything else
  (`image.rs:160-181`); a section with `file_size == 0` is skipped. A section
  ending exactly at EOF is accepted.
- **IAT slots are bounds-checked** against `size_of_image` (`entries`,
  `image.rs:329`): `object` does not validate the IAT RVA, so every
  `iat_slot_rva + 8 <= size_of_image` is enforced here. The loader writes
  those slots blindly, so this check is load-bearing.
- **Import name table**: `OriginalFirstThunk` is preferred; when it is 0 (old
  linkers) the IAT itself is read as the name table (`image.rs:290`).
- **Delay imports** are parsed into a separate list with the same shape
  (`parse_delay_imports`, `image.rs:304`); the loader binds them eagerly.
- **Exports** use `object`'s generic iterator, not the PE table, so **every
  name of a shared ordinal survives** (`image.rs:355` comment, test
  `every_name_of_a_shared_ordinal_survives`). Unused address-table slots are
  not reported. RVA = generic address minus image base.
- **TLS**: only `AddressOfCallBacks` (offset 24 of `IMAGE_TLS_DIRECTORY64`) is
  read; the callback array is walked until a null VA, capped at
  `MAX_TLS_CALLBACKS = 1024` (`image.rs:122`). VAs are converted with
  `va_to_rva` relative to the **preferred** base.
- `has_relocations` = base-reloc directory present **and** `RELOCS_STRIPPED`
  clear. The loader uses it only to word an error message.
- `rva_to_file_offset` (`image.rs:233`) ignores raw padding past
  `virtual_size` and uses `checked_add` (hostile offsets near 4 GiB).
- `implib`: `object` applies the name-type rules (`NAME_NO_PREFIX`,
  `NAME_UNDECORATE`, `NAME_EXPORTAS`), so names come out as the DLL exports
  them. Non-import members (regular objects, long-format descriptors) are
  counted in `other_members`, not errors. Output is `sort()` + `dedup()`; the
  same symbol with a different `kind` is kept.
- Lossy UTF-8 everywhere (`String::from_utf8_lossy`); non-UTF-8 names never
  fail a parse.

## layout constants (offsets relative to the named struct)

| Constant | Value | Used by |
|----------|-------|---------|
| `DOS_E_LFANEW` | 0x3C | header offset helpers |
| `FILE_HEADER_CHARACTERISTICS` | 18 | loader sets `IMAGE_FILE_DLL` |
| `OPT_ADDRESS_OF_ENTRY_POINT` | 16 | loader zeroes it |
| `OPT_CHECKSUM` | 64 | loader zeroes it |
| `OPT_DLL_CHARACTERISTICS` | 70 | loader clears `FORCE_INTEGRITY` |
| `OPT_NUMBER_OF_RVA_AND_SIZES` / `OPT_DATA_DIRECTORY` | 108 / 112 | `data_directory_offset` honours a shorter table |
| `TLS64_ADDRESS_OF_CALLBACKS` | 24 | loader detaches TLS callbacks |
| `dir::{IMPORT=1, EXCEPTION=3, BASERELOC=5, LOAD_CONFIG=10, BOUND_IMPORT=11, IAT=12}` | | |
| `subsystem::{WINDOWS_GUI=2, WINDOWS_CUI=3, XBOX=14, XBOX_CODE_CATALOG=17}` | | `subsystem_name()` |

`data_directory_offset` returns `None` when `index >= NumberOfRvaAndSizes`.
`optional_header_offset` passes a hostile `e_lfanew` through; the bounds-checked
readers then refuse it (test `a_hostile_e_lfanew_is_passed_on_but_the_readers_refuse_it`).

## Environment switches

None. The crate reads no `VICTORY_*` variable.

## Invariants and gotchas

- `ImageInfo` always describes the bytes it was parsed from. The loader keeps
  the info of the **original** image in `PreparedImage.info`; parsing the
  prepared bytes gives `is_dll() == true`, `entry_rva == 0`, no imports.
- `ImportSymbol` ordering/hash derive is relied on by `implib` sort/dedup.
- Do not add mutation here; add new header offsets to `layout` and patch in
  `victory-loader::prepare`.

## How to

- **Add a field to `ImageInfo`**: read it in `ImageInfo::parse` from
  `object`'s `nt_headers()` / `data_directory()`; validate every RVA/size
  (headers are untrusted); add a test that builds or mutates a buffer (see
  `rejects_iat_outside_image`, `rejects_section_extending_beyond_file`).
- **Inspect an image**: `victory inspect <exe|dll> --imports --exports`;
  `victory lib-exports <import.lib> [--list]` for import libraries.

## Tests

`cargo test -p victory-pe` (portable). 10 tests in `image.rs` (several parse
the test binary itself via `current_exe()`, a real MSVC-linked PE32+), 15 in
`implib.rs` (synthetic archives built in-test), 10 in `layout.rs`. No fixture
files, no game data.

//! Translated shaders kept on disk (decision 0003): under the configured
//! cache folder, keyed by the container's content and by the translator
//! (the BLAKE3 of its sources) and its settings, so a changed translator
//! never serves an old translation. Entries go through
//! `victory-pipeline`'s content-addressed cache, which writes them
//! atomically; an entry that does not read back is translated again.
//!
//! The entry is our own encoding of [`HostShader`]: a field added to it
//! must be added to `encode` and `decode` too (entries of older builds
//! are ignored anyway).

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use victory_pipeline::{Cache, Converter};
use victory_shader::emit::{HostShader, StaticSampler, UserData};
use victory_shader::resource::RootIndexedView;
use victory_shader::{HwStage, RootType};

static CACHE: OnceLock<Cache> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

/// Keeps translated shaders in `dir` from now on.
pub fn use_shader_cache(dir: PathBuf) {
    match Cache::open(dir) {
        Ok(cache) => {
            let _ = CACHE.set(cache);
        }
        Err(e) => tracing::warn!(error = %e, "no shader cache"),
    }
}

/// Shaders found in the cache and translated since the start.
pub(crate) fn counts() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}

/// What identifies a translation beside its container: the translator
/// (the BLAKE3 of its sources, `build.rs`), the settings it runs with, and
/// what it was told of the host. Not our executable: a rebuild of anything
/// else used to translate every shader again.
fn build() -> [u8; 32] {
    // The view heap's null slot differs by host GPU: a translation cached
    // without the clamp must not be served (`host_settings` below).
    crate::descriptor::host::probe_view_limit();
    static FIXED: OnceLock<Vec<u8>> = OnceLock::new();
    let fixed = FIXED.get_or_init(|| {
        let limit = victory_shader::emit::loop_limit();
        if limit != victory_shader::emit::DEFAULT_LOOP_LIMIT {
            tracing::warn!(
                limit,
                "VICTORY_SHADER_LOOP_LIMIT: translated loops stop sooner"
            );
        }
        let mut bytes = env!("VICTORY_TRANSLATOR").as_bytes().to_vec();
        bytes.extend_from_slice(&limit.to_le_bytes());
        // Which group barriers wait is part of the translation too.
        bytes.push(victory_shader::emit::group_sync());
        // So are the switches that pick the form of a wave64 program:
        // without them here a run with `VICTORY_WAVE_WIDE=0` was
        // served the translations an earlier run of the same
        // executable had cached with the switch unset.
        for name in [
            "VICTORY_WAVE_WIDE",
            "VICTORY_WAVE_DEEP_SKIPS",
            "VICTORY_WAVE_OWN_BITS",
            "VICTORY_WAVE_LOCAL",
            "VICTORY_F16_NEAREST",
            "VICTORY_HELPER_OWN",
            "VICTORY_FIRSTLANE_OWN",
            "VICTORY_FIRST_HALF",
            "VICTORY_STRICT_JOINS",
            "VICTORY_MESH_OUTPUT_STRICT",
            "VICTORY_BVH_FENCES",
            "VICTORY_BVH_BOUNDS",
            "VICTORY_EARLY_DEPTH",
        ] {
            bytes.extend_from_slice(name.as_bytes());
            bytes.push(b'=');
            if let Some(value) = std::env::var_os(name) {
                bytes.extend_from_slice(value.as_encoded_bytes());
            }
            bytes.push(0);
        }
        // And which clean-up passes ran over the translated code.
        bytes.extend_from_slice(&victory_shader::emit::shader_opt().to_le_bytes());
        bytes
    });
    let mut hash = blake3::Hasher::new();
    hash.update(fixed);
    for value in victory_shader::emit::host_settings() {
        hash.update(&value.to_le_bytes());
    }
    *hash.finalize().as_bytes()
}

/// What tells one set of translations from another ([`build`]).
pub(crate) fn translation() -> [u8; 32] {
    build()
}

/// Puts the translation of `container` into the cache before any title asks
/// for it (`victory shaders`): whether it was there already, or why it does
/// not translate. The host is opened first, as a run does: what the card
/// can do decides the form of some programs.
///
/// `image`: the file name of the image the container was found in (a
/// program the title passes from an image `VICTORY_F16_NEAREST` names is
/// translated with the console's 16-bit rounding, [`console_f16_for`]).
pub fn precompile(container: &[u8], image: Option<&str>) -> Result<bool, String> {
    let _ = crate::host::host();
    let misses = MISSES.load(Ordering::Relaxed);
    let scope = f16_scope(std::env::var("VICTORY_F16_NEAREST").ok().as_deref());
    let program = &blake3::hash(container).to_hex()[..16];
    host_shader(container, in_f16_scope(&scope, image, program))?;
    // (Counts other threads' misses too: the caller only adds them up.)
    Ok(MISSES.load(Ordering::Relaxed) == misses)
}

/// The translation of a console shader container, from the cache when
/// there is one. `console_f16`: its 16-bit float results rounded as the
/// console rounds them ([`console_f16_for`]); such translations are cached
/// apart from the others.
pub(crate) fn host_shader(container: &[u8], console_f16: bool) -> Result<HostShader, String> {
    let translate = |c: &[u8]| {
        victory_shader::emit::with_console_f16(console_f16, || victory_shader::emit::translate(c))
            .map_err(|e| e.to_string())
    };
    match CACHE.get() {
        Some(cache) => cached(cache, &scoped(build(), console_f16), container, translate),
        None => translate(container),
    }
}

/// The key of translations made with the console's 16-bit rounding for
/// their program alone: `build` hashed once more.
fn scoped(build: [u8; 32], console_f16: bool) -> [u8; 32] {
    if !console_f16 {
        return build;
    }
    let mut hash = blake3::Hasher::new();
    hash.update(&build);
    hash.update(b"console f16 rounding");
    *hash.finalize().as_bytes()
}

/// Which programs round their 16-bit float results as the console does
/// (`VICTORY_F16_NEAREST`; the translator's `with_console_f16`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum F16Scope {
    /// `1`: every program (the translator reads the switch itself).
    All,
    /// `0`: none, the host's conversion everywhere (toward zero on NVIDIA
    /// and WARP).
    None,
    /// The programs whose bytes the title passed from these images (file
    /// names, lower case), and the programs named by their hash as the log
    /// gives it (16 hex digits of the container's BLAKE3; for experiments).
    Images(Vec<String>),
}

/// `VICTORY_F16_NEAREST`: unset, empty or `0` none (the default: the host's
/// conversion everywhere), `1` every program, else a comma list of image
/// file names and program hashes. The candidate for a default is
/// `amd_fidelityfx_upscaler_xbox.dll` (AMD's FidelityFX upscaler for Xbox,
/// middleware a title ships beside its executable): its accumulate pass
/// computes its network's inputs in 16 bits and with the host's rounding
/// toward zero its history ran away (the hotspot). With that scope no launch
/// of 7 had the hotspot, but 2 of the 7 were posterised (with the host's
/// rounding: hotspot 4 of 5, posterised 5 of 5), so it is not the default
/// yet (`docs/bug-journal.md` 2026-11-27).
fn f16_scope(value: Option<&str>) -> F16Scope {
    match value.map(str::trim) {
        Some("1") => F16Scope::All,
        Some(list) if !list.is_empty() && list != "0" => F16Scope::Images(
            list.split(',')
                .map(|name| name.trim().to_ascii_lowercase())
                .filter(|name| !name.is_empty())
                .collect(),
        ),
        _ => F16Scope::None,
    }
}

/// Whether a program the title passed from the image named `image` (its
/// file name; `None`: not from an image, e.g. read from the title's archives
/// into its heap), whose container's hash is `program`, is translated inside
/// the translator's console-rounding scope. With `VICTORY_F16_NEAREST=1` the
/// translator rounds every program so already, and none needs the scope.
fn in_f16_scope(scope: &F16Scope, image: Option<&str>, program: &str) -> bool {
    match scope {
        F16Scope::Images(names) => names.iter().any(|name| {
            name.eq_ignore_ascii_case(program)
                || image.is_some_and(|image| name.eq_ignore_ascii_case(image))
        }),
        _ => false,
    }
}

/// Whether the program in a container the title passed at `address`, whose
/// hash is `program`, gets the console's 16-bit rounding: whether the image
/// holding `address` is one `VICTORY_F16_NEAREST` names ([`f16_scope`]), or
/// the program is. The first program of each image is logged.
#[cfg(windows)]
pub(crate) fn console_f16_for(address: usize, program: &str) -> bool {
    static SCOPE: OnceLock<F16Scope> = OnceLock::new();
    let scope = SCOPE.get_or_init(|| {
        let scope = f16_scope(std::env::var("VICTORY_F16_NEAREST").ok().as_deref());
        tracing::info!(?scope, "16-bit float results rounded as the console does for");
        scope
    });
    if !matches!(scope, F16Scope::Images(_)) {
        return false;
    }
    let image = image_name(address);
    let on = in_f16_scope(scope, image.as_deref(), program);
    if on {
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(image) = image.filter(|i| !seen.contains(i)) {
            tracing::info!(
                image = %image,
                program,
                at = format_args!("{address:#x}"),
                "first program of this image: its 16-bit float results rounded as the console does"
            );
            seen.push(image);
        }
    }
    on
}

/// The title's images by their base, with the name of the file each was
/// mapped from (the loader maps a prepared copy called
/// `victory-image-<hash>.dll`, so Windows does not know the title's name).
static TITLE_IMAGES: std::sync::Mutex<Vec<(usize, String)>> = std::sync::Mutex::new(Vec::new());

/// The title image at `base` was mapped from the file `name` (`None`: it
/// is unmapped again).
pub fn note_title_image(base: usize, name: Option<String>) {
    let mut images = TITLE_IMAGES.lock().unwrap_or_else(|e| e.into_inner());
    images.retain(|(b, _)| *b != base);
    images.extend(name.map(|name| (base, name)));
}

/// The name of the title file whose image `address` lies in, if it lies in
/// one; for an image that is not the title's, its own file name.
#[cfg(windows)]
fn image_name(address: usize) -> Option<String> {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    let mut module = std::ptr::null_mut();
    // SAFETY: looks the image up by an address in it; no reference is taken.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    } != 0;
    if !found {
        return None;
    }
    let base = module as usize;
    let images = TITLE_IMAGES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, name)) = images.iter().find(|(b, _)| *b == base) {
        return Some(name.clone());
    }
    drop(images);
    let mut path = [0u16; 260];
    // SAFETY: `module` is a mapped image; the buffer's length is passed.
    let len = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) };
    let path = String::from_utf16_lossy(&path[..len as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_owned)
}

/// The cache entry's converter: the build and the container in, the
/// encoded translation out. `ran` tells a miss from a hit.
struct Translate<F> {
    translate: F,
    ran: AtomicBool,
}

impl<F: Fn(&[u8]) -> Result<HostShader, String> + Send + Sync> Converter for Translate<F> {
    fn id(&self) -> &'static str {
        "shader.dxil"
    }
    fn version(&self) -> u32 {
        // The build fingerprint in the input does the versioning.
        1
    }
    fn convert(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.ran.store(true, Ordering::Relaxed);
        let shader = (self.translate)(&input[32..]).map_err(anyhow::Error::msg)?;
        Ok(encode(&shader))
    }
}

fn cached(
    cache: &Cache,
    build: &[u8; 32],
    container: &[u8],
    translate: impl Fn(&[u8]) -> Result<HostShader, String> + Send + Sync,
) -> Result<HostShader, String> {
    let input = [&build[..], container].concat();
    let converter = Translate {
        translate,
        ran: AtomicBool::new(false),
    };
    for _ in 0..2 {
        let path = cache
            .get_or_convert(&converter, &input)
            .map_err(|e| format!("{e:#}"))?;
        if let Some(shader) = std::fs::read(&path).ok().and_then(|b| decode(&b)) {
            let (counter, what) = if converter.ran.load(Ordering::Relaxed) {
                (&MISSES, "translated and cached")
            } else {
                (&HITS, "from the cache")
            };
            counter.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(path = %path.display(), "shader {what}");
            return Ok(shader);
        }
        tracing::debug!(path = %path.display(), "unreadable shader cache entry; translating again");
        let _ = std::fs::remove_file(&path);
    }
    MISSES.fetch_add(1, Ordering::Relaxed);
    (converter.translate)(container)
}

const MAGIC: &[u8; 4] = b"VSH1";
const STAGES: [HwStage; 8] = [
    HwStage::Ls,
    HwStage::Hs,
    HwStage::Es,
    HwStage::Gs,
    HwStage::Vs,
    HwStage::Ps,
    HwStage::Ms,
    HwStage::Cs,
];

fn root_code(r: RootType) -> u32 {
    match r {
        RootType::Constants => 0,
        RootType::Descriptor => 1,
        RootType::Table => 2,
        RootType::ImmediateSampler => 3,
        RootType::ResourceHeap => 4,
        RootType::SamplerHeap => 5,
        RootType::Other(x) => 6 + x,
    }
}

fn root_type(code: u32) -> RootType {
    match code {
        0 => RootType::Constants,
        1 => RootType::Descriptor,
        2 => RootType::Table,
        3 => RootType::ImmediateSampler,
        4 => RootType::ResourceHeap,
        5 => RootType::SamplerHeap,
        x => RootType::Other(x - 6),
    }
}

/// Little-endian words and length-prefixed lists.
fn encode(s: &HostShader) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    let word = |out: &mut Vec<u8>, v: u32| out.extend(v.to_le_bytes());
    let stage = STAGES.iter().position(|&x| x == s.stage).unwrap_or(0) as u32;
    word(&mut out, stage);
    word(&mut out, s.dxil.len() as u32);
    out.extend(&s.dxil);
    word(&mut out, s.user_data.len() as u32);
    for u in &s.user_data {
        for v in [u.index, u32::from(u.slot), root_code(u.root), u.dword] {
            word(&mut out, v);
        }
    }
    word(&mut out, s.samplers.len() as u32);
    for sampler in &s.samplers {
        for &v in sampler {
            word(&mut out, v);
        }
    }
    word(&mut out, s.static_samplers.len() as u32);
    for t in &s.static_samplers {
        let [u, v, w] = t.address;
        let words = [
            t.filter,
            u,
            v,
            w,
            t.mip_lod_bias.to_bits(),
            t.max_anisotropy,
            t.comparison,
            t.border,
            t.min_lod.to_bits(),
            t.max_lod.to_bits(),
        ];
        for v in words {
            word(&mut out, v);
        }
    }
    for slots in [&s.root_cbvs, &s.root_srvs, &s.root_uavs] {
        word(&mut out, slots.len() as u32);
        out.extend(slots);
    }
    word(&mut out, s.root_indexed_views.len() as u32);
    for view in &s.root_indexed_views {
        for value in [
            u32::from(view.table_slot),
            u32::from(view.constant_slot),
            view.constant_dword,
            view.byte_offset,
            u32::from(view.shift),
        ] {
            word(&mut out, value);
        }
    }
    out
}

fn decode(bytes: &[u8]) -> Option<HostShader> {
    struct Reader<'a>(&'a [u8]);
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let (head, rest) = self.0.split_at_checked(n)?;
            self.0 = rest;
            Some(head)
        }
        fn word(&mut self) -> Option<u32> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
        }
        fn list(&mut self) -> Option<Vec<u8>> {
            let n = self.word()? as usize;
            Some(self.take(n)?.to_vec())
        }
    }
    let mut r = Reader(bytes);
    if r.take(4)? != MAGIC {
        return None;
    }
    let stage = *STAGES.get(r.word()? as usize)?;
    let dxil = r.list()?;
    let n = r.word()? as usize;
    let mut user_data = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        user_data.push(UserData {
            index: r.word()?,
            slot: u8::try_from(r.word()?).ok()?,
            root: root_type(r.word()?),
            dword: r.word()?,
        });
    }
    let n = r.word()? as usize;
    let mut samplers = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        samplers.push([r.word()?, r.word()?, r.word()?, r.word()?]);
    }
    let n = r.word()? as usize;
    let mut static_samplers = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        static_samplers.push(StaticSampler {
            filter: r.word()?,
            address: [r.word()?, r.word()?, r.word()?],
            mip_lod_bias: f32::from_bits(r.word()?),
            max_anisotropy: r.word()?,
            comparison: r.word()?,
            border: r.word()?,
            min_lod: f32::from_bits(r.word()?),
            max_lod: f32::from_bits(r.word()?),
        });
    }
    let root_cbvs = r.list()?;
    let root_srvs = r.list()?;
    let root_uavs = r.list()?;
    let n = r.word()? as usize;
    let mut root_indexed_views = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        root_indexed_views.push(RootIndexedView {
            table_slot: u8::try_from(r.word()?).ok()?,
            constant_slot: u8::try_from(r.word()?).ok()?,
            constant_dword: r.word()?,
            byte_offset: r.word()?,
            shift: u8::try_from(r.word()?).ok().filter(|&s| s < 32)?,
        });
    }
    r.0.is_empty().then_some(HostShader {
        stage,
        dxil,
        user_data,
        samplers,
        static_samplers,
        root_cbvs,
        root_srvs,
        root_uavs,
        root_indexed_views,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shader() -> HostShader {
        HostShader {
            stage: HwStage::Ps,
            dxil: b"DXBC and the rest".to_vec(),
            user_data: vec![UserData {
                index: 8,
                slot: 1,
                root: RootType::Table,
                dword: 0,
            }],
            samplers: vec![[0x400, 0xc0_0000, 0x18f0_0000, 0]],
            static_samplers: vec![StaticSampler::from_words([
                0x400,
                0xc0_0000,
                0x18f0_0000,
                0,
            ])],
            root_cbvs: vec![2],
            root_srvs: vec![0, 3],
            root_uavs: vec![1],
            root_indexed_views: vec![RootIndexedView {
                table_slot: 1,
                constant_slot: 2,
                constant_dword: 3,
                byte_offset: 32,
                shift: 5,
            }],
        }
    }

    #[test]
    fn translations_come_back_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let build = [7u8; 32];
        let container = b"a console container".as_slice();
        let runs = AtomicU64::new(0);
        let translate = |_: &[u8]| {
            runs.fetch_add(1, Ordering::Relaxed);
            Ok(shader())
        };
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 1, "the second came from disk");

        // Another build translates again.
        assert_eq!(cached(&cache, &[8; 32], container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 2);

        // A damaged entry is a miss, translated and stored again.
        for e in walk(dir.path()) {
            std::fs::write(e, b"VSH1 broken").unwrap();
        }
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 3);
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 3);

        // Failures are not stored.
        let fail = |_: &[u8]| Err::<HostShader, _>("unsupported".to_owned());
        assert!(cached(&cache, &build, b"other", fail).is_err());
    }

    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                files.extend(walk(&e.path()));
            } else {
                files.push(e.path());
            }
        }
        files
    }

    #[test]
    fn the_console_f16_scope_is_the_images_and_programs_the_switch_names() {
        const P: &str = "0123456789abcdef";
        let upscaler = Some("AMD_FidelityFX_Upscaler_Xbox.dll");
        // Unset, empty or `0`: nowhere; `1`: everywhere, which the
        // translator does on its own.
        for value in [None, Some(""), Some("  "), Some("0"), Some("1")] {
            assert!(!in_f16_scope(&f16_scope(value), upscaler, P), "{value:?}");
        }
        let scope = f16_scope(Some("amd_fidelityfx_upscaler_xbox.dll"));
        assert!(in_f16_scope(&scope, upscaler, P));
        assert!(!in_f16_scope(&scope, Some("game_scarlett_master.exe"), P));
        assert!(!in_f16_scope(&scope, Some("XboxBvhBuilder.dll"), P));
        assert!(!in_f16_scope(&scope, None, P), "a program from the heap");
        assert_eq!(f16_scope(Some("1")), F16Scope::All);
        let listed = f16_scope(Some(" XboxBvhBuilder.dll ,game_scarlett_master.exe,"));
        assert!(in_f16_scope(&listed, Some("xboxbvhbuilder.dll"), P));
        assert!(in_f16_scope(&listed, Some("game_scarlett_master.exe"), P));
        assert!(!in_f16_scope(&listed, upscaler, P));
        // A program by its hash, wherever it came from; others from its image not.
        let one = f16_scope(Some("6A6C7B7FDE59134F"));
        assert!(in_f16_scope(&one, upscaler, "6a6c7b7fde59134f"));
        assert!(in_f16_scope(&one, None, "6a6c7b7fde59134f"));
        assert!(!in_f16_scope(&one, upscaler, P));
    }

    #[test]
    fn translations_in_the_console_f16_scope_are_cached_apart() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let build = [7u8; 32];
        assert_eq!(scoped(build, false), build);
        assert_ne!(scoped(build, true), build);
        let container = b"a console container".as_slice();
        let runs = AtomicU64::new(0);
        let translate = |_: &[u8]| {
            runs.fetch_add(1, Ordering::Relaxed);
            Ok(shader())
        };
        for console_f16 in [false, true, false, true] {
            cached(&cache, &scoped(build, console_f16), container, translate).unwrap();
        }
        assert_eq!(runs.load(Ordering::Relaxed), 2, "one translation of each kind");
    }

    #[test]
    fn entries_round_trip_and_reject_damage() {
        let bytes = encode(&shader());
        assert_eq!(decode(&bytes), Some(shader()));
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode(&[bytes.as_slice(), &[0]].concat()), None);
        assert_eq!(decode(b"VSH0"), None);
    }
}

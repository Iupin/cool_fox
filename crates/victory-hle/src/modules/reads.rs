//! What the title reads from files, said aloud: the reads the kernel does
//! straight into title memory (`files::ReadFile`, `fread`, `_read`, the
//! DirectStorage workers) fail on a block the GPU side guards, and a title
//! that ignores the failure runs on with whatever its buffer held. So:
//!
//! - every failed or unexplained short read is a warning (rate-limited per
//!   call site), with the file, the destination, what was asked and got,
//!   the host's error and how the destination was guarded;
//! - for the first minute of the run, one info line per file at its first
//!   read (`title file: first read`), and one per file when the minute is
//!   over (`title file: read in the first minute`) with its reads and bytes,
//!   so two runs can be compared by file name.
//!
//! Names come from the host (`GetFinalPathNameByHandleW`) once per handle,
//! stream or descriptor, and are forgotten when our wrappers open a new one
//! of the same value.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use super::files::host;

/// How long after the start the title's file reads are listed.
pub(crate) const EARLY: Duration = Duration::from_secs(60);

static START: OnceLock<Instant> = OnceLock::new();

/// The run starts (the first virtual drive is mounted); later calls do
/// nothing.
pub(crate) fn start() {
    START.get_or_init(Instant::now);
}

/// What a read went through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    /// A kernel file handle.
    Handle(usize),
    /// A CRT stream (`FILE*`).
    Stream(usize),
    /// A CRT file descriptor.
    Descriptor(i32),
}

fn names() -> MutexGuard<'static, HashMap<Source, String>> {
    static NAMES: OnceLock<Mutex<HashMap<Source, String>>> = OnceLock::new();
    NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// A new file was opened as `source`: a name known for the value is
/// another file's.
pub(crate) fn opened(source: Source) {
    names().remove(&source);
}

/// The kernel handle behind `source`, if there is one. The CRT calls its
/// invalid parameter handler for a null stream or a closed descriptor, and
/// the default one ends the process: a handler that returns is set for this
/// thread meanwhile.
fn handle_of(source: Source) -> Option<*mut c_void> {
    type Handler = Option<extern "C" fn(*const u16, *const u16, *const u16, u32, usize)>;
    extern "C" fn ignore(_: *const u16, _: *const u16, _: *const u16, _: u32, _: usize) {}
    let stream = match source {
        Source::Handle(h) => return Some(h as *mut c_void),
        Source::Stream(0) => return None,
        Source::Stream(f) => Some(f),
        Source::Descriptor(_) => None,
    };
    let set = host!("ucrtbase.dll", "_set_thread_local_invalid_parameter_handler": extern "C" fn(Handler) -> Handler);
    let before = set(Some(ignore));
    let descriptor = match (stream, source) {
        (Some(f), _) => {
            host!("ucrtbase.dll", "_fileno": extern "C" fn(*mut c_void) -> i32)(f as *mut c_void)
        }
        (None, Source::Descriptor(fd)) => fd,
        (None, _) => -1,
    };
    let handle = if descriptor < 0 {
        -1
    } else {
        host!("ucrtbase.dll", "_get_osfhandle": extern "C" fn(i32) -> isize)(descriptor)
    };
    set(before);
    (handle != -1 && handle != 0 && handle != -2).then_some(handle as *mut c_void)
}

/// The host path of `source`, or what it is when that is not known.
pub(crate) fn name(source: Source) -> String {
    if let Some(name) = names().get(&source) {
        return name.clone();
    }
    let name = handle_of(source)
        .and_then(final_path)
        .unwrap_or_else(|| format!("{source:x?}"));
    names().insert(source, name.clone());
    name
}

fn final_path(handle: *mut c_void) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut buffer = vec![0u16; 512];
    for _ in 0..2 {
        // SAFETY: a buffer of `len` units; the handle is the title's.
        let n = unsafe {
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0)
        } as usize;
        if n == 0 {
            return None;
        }
        if n < buffer.len() {
            let path = String::from_utf16_lossy(&buffer[..n]);
            return Some(
                path.strip_prefix(r"\\?\")
                    .map(str::to_owned)
                    .unwrap_or(path),
            );
        }
        buffer.resize(n + 1, 0);
    }
    None
}

/// The size of the file behind `source`, if it is a file on disk.
pub(crate) fn size(source: Source) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileSizeEx, GetFileType};
    let handle = handle_of(source)?;
    // SAFETY: plain queries of the title's handle.
    unsafe {
        if GetFileType(handle) != FILE_TYPE_DISK {
            return None;
        }
        let mut size = 0i64;
        (GetFileSizeEx(handle, &mut size) != 0).then_some(size as u64)
    }
}

/// Whether `source` is a file on disk (reads of which can be tried again).
pub(crate) fn on_disk(source: Source) -> bool {
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};
    // SAFETY: a plain query of the title's handle.
    handle_of(source).is_some_and(|h| unsafe { GetFileType(h) } == FILE_TYPE_DISK)
}

/// The file pointer of a handle.
pub(crate) fn position(handle: *mut c_void) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_CURRENT, SetFilePointerEx};
    let mut at = 0i64;
    // SAFETY: a plain query of the title's handle; the pointer is not moved.
    (unsafe { SetFilePointerEx(handle, 0, &mut at, FILE_CURRENT) } != 0).then_some(at as u64)
}

/// Whether the `n`th event of a kind is to be told: the first 16, then
/// every power of two.
pub(crate) fn tell(counter: &AtomicU64) -> Option<u64> {
    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
    (n <= 16 || n.is_power_of_two()).then_some(n)
}

/// How title memory at a failed read's destination is guarded, in words.
pub(crate) fn guarded(at: usize, len: usize) -> String {
    let g = victory_gpu::title_memory_guarded(at, len);
    format!(
        "{} of {} blocks read-only by the GPU side's guard, {} also shown and guarded at another view",
        g.read_only, g.blocks, g.other_views
    )
}

#[derive(Default)]
struct Tally {
    api: &'static str,
    size: Option<u64>,
    reads: u64,
    asked: u64,
    got: u64,
    failed: u64,
}

static OVER: AtomicBool = AtomicBool::new(false);

fn tallies() -> MutexGuard<'static, HashMap<String, Tally>> {
    static TALLIES: OnceLock<Mutex<HashMap<String, Tally>>> = OnceLock::new();
    TALLIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// A read of the title's from `source` by `api`: `asked` bytes, `got`
/// delivered, or failed. Listed in the first minute; nothing after it.
pub(crate) fn note(api: &'static str, source: Source, asked: u64, got: u64, failed: bool) {
    note_named(api, || name(source), || size(source), asked, got, failed);
}

/// [`note`] for a file known by name (DirectStorage's).
pub(crate) fn note_named(
    api: &'static str,
    name: impl FnOnce() -> String,
    size: impl FnOnce() -> Option<u64>,
    asked: u64,
    got: u64,
    failed: bool,
) {
    if OVER.load(Ordering::Relaxed) {
        return;
    }
    if START.get_or_init(Instant::now).elapsed() >= EARLY {
        summarise();
        return;
    }
    let name = name();
    let mut tallies = tallies();
    let tally = tallies.entry(name).or_insert_with_key(|name| {
        let size = size();
        tracing::info!(path = %name, api, size, asked, got, failed, "title file: first read");
        Tally {
            api,
            size,
            ..Tally::default()
        }
    });
    tally.reads += 1;
    tally.asked += asked;
    tally.got += got;
    tally.failed += u64::from(failed);
}

/// The first minute is over: one line per file read in it.
fn summarise() {
    if OVER.swap(true, Ordering::Relaxed) {
        return;
    }
    let tallies = std::mem::take(&mut *tallies());
    let mut files: Vec<_> = tallies.into_iter().collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    tracing::info!(
        files = files.len(),
        seconds = EARLY.as_secs(),
        "title files read in the first minute"
    );
    for (path, t) in files {
        tracing::info!(
            path = %path,
            api = t.api,
            size = t.size,
            reads = t.reads,
            asked = t.asked,
            got = t.got,
            failed = t.failed,
            "title file: read in the first minute"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed descriptor or a null stream has no name, and asking does
    /// not end the process (the CRT's default invalid parameter handler).
    #[test]
    fn bad_descriptors_and_streams_have_no_handle() {
        assert_eq!(handle_of(Source::Stream(0)), None);
        assert_eq!(handle_of(Source::Descriptor(4000)), None);
        assert_eq!(handle_of(Source::Descriptor(-1)), None);
        assert!(name(Source::Descriptor(4001)).contains("Descriptor"));
    }

    /// The first 16 of a kind are told, then every power of two.
    #[test]
    fn tell_thins_out() {
        let counter = AtomicU64::new(0);
        let told: Vec<u64> = (0..100).filter_map(|_| tell(&counter)).collect();
        assert_eq!(
            told,
            [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 32, 64
            ]
        );
    }

    /// A handle's name is the host's path of its file.
    #[test]
    fn handles_are_named_by_their_path() {
        use std::os::windows::io::AsRawHandle;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("named.bin");
        std::fs::write(&path, b"12345").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let source = Source::Handle(file.as_raw_handle() as usize);
        opened(source);
        assert!(name(source).ends_with("named.bin"), "{}", name(source));
        assert_eq!(size(source), Some(5));
        assert!(on_disk(source));
    }
}

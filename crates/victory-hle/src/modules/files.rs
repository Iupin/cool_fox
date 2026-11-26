//! Virtual drives over host folders: on the console, title storage is a
//! drive of its own, and titles build paths on that assumption.
//!
//! Every title import that takes a path is bound here. A path on a virtual
//! drive (`X:\..`, `X:/..`, `X:`, `\\?\X:\..`) is rewritten to the host
//! folder behind it and handed to the host's own function; any other path
//! goes to the host unchanged. A rewritten narrow path goes to the host's
//! wide twin, so host folders outside the ANSI code page work; never to our
//! wide wrapper, which would rewrite the host path a second time when its
//! drive letter is mounted too. Letters are chosen among those the host
//! does not use, so no host drive is hidden.
#![allow(non_snake_case)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::too_many_arguments)]

use std::ffi::{CStr, c_char, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::{Mutex, MutexGuard};

use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;

use super::reads;
use crate::{HleExport, HleModule, hle_export};

/// The host's own function: what the import would have been bound to.
macro_rules! host {
    ($dll:literal, $name:literal: $ty:ty) => {{
        static ADDRESS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let address = *ADDRESS.get_or_init(|| {
            crate::host::function($dll, $name)
                .unwrap_or_else(|| panic!("the host lacks {}!{}", $dll, $name))
        });
        // SAFETY: the host exports this function with this signature.
        unsafe { std::mem::transmute::<usize, $ty>(address) }
    }};
}
pub(crate) use host;

/// Mounted drives: upper-case letter and host root (UTF-16, no trailing
/// separator, no NUL).
static DRIVES: Mutex<Vec<(u16, Vec<u16>)>> = Mutex::new(Vec::new());

fn drives() -> MutexGuard<'static, Vec<(u16, Vec<u16>)>> {
    DRIVES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Gives the host folder `root` a drive letter (the same one if it already
/// has one): the highest letter neither the host nor another mount uses.
pub fn mount(root: &Path) -> Option<char> {
    reads::start();
    let wide = root_wide(root);
    let mut drives = drives();
    if let Some(&(letter, _)) = drives.iter().find(|(_, r)| *r == wide) {
        return char::from_u32(letter.into());
    }
    // SAFETY: no arguments.
    let host = unsafe { GetLogicalDrives() };
    let letter = (b'D'..=b'Z').rev().find(|&l| {
        host >> (l - b'A') & 1 == 0 && !drives.iter().any(|(m, _)| *m == u16::from(l))
    })?;
    tracing::info!(drive = %char::from(letter), root = %root.display(), "virtual drive");
    drives.push((letter.into(), wide));
    Some(letter.into())
}

/// Gives the host folder `root` a drive letter the console fixes (titles
/// open their package as `G:\..` and temporary files as `T:\..`). A host
/// drive of that letter is hidden from the title.
pub fn mount_at(letter: char, root: &Path) {
    reads::start();
    let letter = letter.to_ascii_uppercase();
    let Some(index) = u32::from(letter)
        .checked_sub(u32::from('A'))
        .filter(|i| *i < 26)
    else {
        return;
    };
    // SAFETY: no arguments.
    let hides_host_drive = unsafe { GetLogicalDrives() } >> index & 1 != 0;
    tracing::info!(drive = %letter, root = %root.display(), hides_host_drive, "virtual drive");
    let mut drives = drives();
    drives.retain(|(l, _)| u32::from(*l) != u32::from(letter));
    drives.push((letter as u16, root_wide(root)));
}

/// `root` as UTF-16, absolute, without trailing separators (a drive root
/// keeps its own). An empty root is the current directory, as it is to a
/// path joined to it: it is the title's folder when the title is run by its
/// file name alone (`victory run GTA6.exe`).
fn root_wide(root: &Path) -> Vec<u16> {
    let root = if root.as_os_str().is_empty() {
        Path::new(".")
    } else {
        root
    };
    let root = std::path::absolute(root).unwrap_or_else(|_| root.to_owned());
    let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    while wide.len() > 3 && wide.last().is_some_and(|c| SEPARATORS.contains(c)) {
        wide.pop();
    }
    wide
}

const SEPARATORS: [u16; 2] = [b'\\' as u16, b'/' as u16];
const VERBATIM: [u16; 4] = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];

/// Host paths from this length on take the `\\?\` form, which lifts the
/// host's MAX_PATH limit (248 characters for a directory, 260 for a file).
const LONG: usize = 248;

/// `root` and the title's `tail` as a `\\?\` path. That form turns off the
/// host's own clean-up, so `/` becomes `\` and `.` and `..` are resolved
/// here (`..` stops at the root).
fn verbatim(root: &[u16], tail: &[u16]) -> Vec<u16> {
    let backslash = u16::from(b'\\');
    let mut out = if root.starts_with(&VERBATIM) {
        root.to_vec()
    } else if let Some(unc) = root.strip_prefix(&[backslash; 2][..]) {
        let mut out = VERBATIM.to_vec();
        out.extend("UNC\\".encode_utf16());
        out.extend_from_slice(unc);
        out
    } else {
        [&VERBATIM[..], root].concat()
    };
    while out.last() == Some(&backslash) {
        out.pop();
    }
    let dot = u16::from(b'.');
    let mut parts: Vec<&[u16]> = Vec::new();
    for part in tail.split(|c| SEPARATORS.contains(c)) {
        match part {
            [] => {}
            [d] if *d == dot => {}
            [d, e] if [*d, *e] == [dot; 2] => drop(parts.pop()),
            _ => parts.push(part),
        }
    }
    for part in parts {
        out.push(backslash);
        out.extend_from_slice(part);
    }
    out
}

/// The host path, NUL-terminated, for `path` (without its NUL) if it is on
/// a virtual drive.
pub(crate) fn translate(path: &[u16]) -> Option<Vec<u16>> {
    let (prefix, rest) = match path.strip_prefix(&VERBATIM[..]) {
        Some(rest) => (&VERBATIM[..], rest),
        None => (&[][..], path),
    };
    let &[letter, colon, ..] = rest else {
        return None;
    };
    if colon != u16::from(b':') {
        return None;
    }
    let letter = u8::try_from(letter).ok().filter(u8::is_ascii_alphabetic)?;
    let letter = u16::from(letter.to_ascii_uppercase());
    let drives = drives();
    let (_, root) = drives.iter().find(|(l, _)| *l == letter)?;
    let tail = &rest[2..];
    if prefix.is_empty() && root.len() + tail.len() >= LONG {
        let mut out = verbatim(root, tail);
        out.push(0);
        return Some(out);
    }
    let mut out = prefix.to_vec();
    out.extend_from_slice(root);
    // `X:` and `X:name` are relative to the root.
    if !tail.first().is_some_and(|c| SEPARATORS.contains(c)) {
        out.push(b'\\'.into());
    }
    let verbatim = !prefix.is_empty();
    out.extend(tail.iter().map(|&c| {
        if verbatim && c == u16::from(b'/') {
            b'\\'.into()
        } else {
            c
        }
    }));
    out.push(0);
    Some(out)
}

/// A title's wide path: the host path when it is on a virtual drive, else
/// the title's own pointer.
struct Wide(*const u16, Option<Vec<u16>>);

impl Wide {
    fn ptr(&self) -> *const u16 {
        self.1.as_ref().map_or(self.0, |p| p.as_ptr())
    }
}

fn wide(path: *const u16) -> Wide {
    if path.is_null() {
        return Wide(path, None);
    }
    // SAFETY: the title passes a NUL-terminated path.
    let path_slice = unsafe {
        let len = (0..).take_while(|&i| *path.add(i) != 0).count();
        std::slice::from_raw_parts(path, len)
    };
    let host = translate(path_slice);
    trace(path_slice, host.as_deref());
    Wide(path, host)
}

/// Every title path at debug level, with the host path if rewritten.
fn trace(path: &[u16], host: Option<&[u16]>) {
    tracing::debug!(
        path = %String::from_utf16_lossy(path),
        host = host.map(|h| String::from_utf16_lossy(&h[..h.len() - 1])),
        "title path"
    );
}

/// A narrow path (ANSI code page, as the host decodes it) on a virtual
/// drive, as a wide host path.
fn narrow(path: *const c_char) -> Option<Vec<u16>> {
    if path.is_null() {
        return None;
    }
    // SAFETY: the title passes a NUL-terminated path.
    let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
    // The drive part is ASCII in every code page; decode only candidates.
    match bytes.strip_prefix(br"\\?\").unwrap_or(bytes) {
        [letter, b':', ..] if letter.is_ascii_alphabetic() => {}
        _ => {
            tracing::debug!(path = %String::from_utf8_lossy(bytes), "title path");
            return None;
        }
    }
    let len = i32::try_from(bytes.len()).ok()?;
    // SAFETY: sizing call, then a call with room for `n` units.
    let wide = unsafe {
        let n = MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
        let mut wide = vec![0u16; usize::try_from(n).ok()?];
        MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), n);
        wide
    };
    let host = translate(&wide);
    trace(&wide, host.as_deref());
    host
}

/// An ASCII mode string (`"rb"`, `"w, ccs=UTF-8"`) for a wide CRT call.
fn widen(mode: *const c_char) -> Vec<u16> {
    if mode.is_null() {
        return vec![0];
    }
    // SAFETY: a NUL-terminated mode string.
    let bytes = unsafe { CStr::from_ptr(mode) }.to_bytes();
    bytes.iter().map(|&b| u16::from(b)).chain([0]).collect()
}

type Handle = *mut c_void;
type File = *mut c_void;

// kernel32 and its file API sets.

pub extern "system" fn CreateFileW(
    path: *const u16,
    access: u32,
    share: u32,
    security: *const c_void,
    disposition: u32,
    flags: u32,
    template: Handle,
) -> Handle {
    let p = wide(path);
    handle_opened(
        host!("kernel32.dll", "CreateFileW": extern "system" fn(*const u16, u32, u32, *const c_void, u32, u32, Handle) -> Handle)(
            p.ptr(),
            access,
            share,
            security,
            disposition,
            flags,
            template,
        )
    )
}

pub extern "system" fn CreateFileA(
    path: *const c_char,
    access: u32,
    share: u32,
    security: *const c_void,
    disposition: u32,
    flags: u32,
    template: Handle,
) -> Handle {
    handle_opened(
        match narrow(path) {
            Some(p) => {
                host!("kernel32.dll", "CreateFileW": extern "system" fn(*const u16, u32, u32, *const c_void, u32, u32, Handle) -> Handle)(
                    p.as_ptr(),
                    access,
                    share,
                    security,
                    disposition,
                    flags,
                    template,
                )
            }
            None => {
                host!("kernel32.dll", "CreateFileA": extern "system" fn(*const c_char, u32, u32, *const c_void, u32, u32, Handle) -> Handle)(
                    path,
                    access,
                    share,
                    security,
                    disposition,
                    flags,
                    template,
                )
            }
        }
    )
}

/// Used by Bink.
pub extern "system" fn CreateFile2(
    path: *const u16,
    access: u32,
    share: u32,
    disposition: u32,
    params: *const c_void,
) -> Handle {
    let p = wide(path);
    handle_opened(
        host!("kernel32.dll", "CreateFile2": extern "system" fn(*const u16, u32, u32, u32, *const c_void) -> Handle)(
            p.ptr(),
            access,
            share,
            disposition,
            params,
        )
    )
}

pub extern "system" fn DeleteFileA(path: *const c_char) -> i32 {
    match narrow(path) {
        Some(p) => {
            host!("kernel32.dll", "DeleteFileW": extern "system" fn(*const u16) -> i32)(p.as_ptr())
        }
        None => {
            host!("kernel32.dll", "DeleteFileA": extern "system" fn(*const c_char) -> i32)(path)
        }
    }
}

pub extern "system" fn DeleteFileW(path: *const u16) -> i32 {
    let p = wide(path);
    host!("kernel32.dll", "DeleteFileW": extern "system" fn(*const u16) -> i32)(p.ptr())
}

pub extern "system" fn RemoveDirectoryW(path: *const u16) -> i32 {
    let p = wide(path);
    host!("kernel32.dll", "RemoveDirectoryW": extern "system" fn(*const u16) -> i32)(p.ptr())
}

pub extern "system" fn SetFileAttributesW(path: *const u16, attributes: u32) -> i32 {
    let p = wide(path);
    host!("kernel32.dll", "SetFileAttributesW": extern "system" fn(*const u16, u32) -> i32)(
        p.ptr(),
        attributes,
    )
}

pub extern "system" fn GetFileAttributesA(path: *const c_char) -> u32 {
    match narrow(path) {
        Some(p) => {
            host!("kernel32.dll", "GetFileAttributesW": extern "system" fn(*const u16) -> u32)(
                p.as_ptr(),
            )
        }
        None => {
            host!("kernel32.dll", "GetFileAttributesA": extern "system" fn(*const c_char) -> u32)(
                path,
            )
        }
    }
}

pub extern "system" fn CreateDirectoryW(path: *const u16, security: *const c_void) -> i32 {
    let p = wide(path);
    host!("kernel32.dll", "CreateDirectoryW": extern "system" fn(*const u16, *const c_void) -> i32)(
        p.ptr(),
        security,
    )
}

pub extern "system" fn GetFileAttributesExW(
    path: *const u16,
    level: i32,
    info: *mut c_void,
) -> i32 {
    let p = wide(path);
    host!("kernel32.dll", "GetFileAttributesExW": extern "system" fn(*const u16, i32, *mut c_void) -> i32)(
        p.ptr(),
        level,
        info,
    )
}

pub extern "system" fn GetFileAttributesW(path: *const u16) -> u32 {
    let p = wide(path);
    host!("kernel32.dll", "GetFileAttributesW": extern "system" fn(*const u16) -> u32)(p.ptr())
}

pub extern "system" fn GetDiskFreeSpaceExW(
    path: *const u16,
    available: *mut u64,
    total: *mut u64,
    free: *mut u64,
) -> i32 {
    let p = wide(path);
    host!("kernel32.dll", "GetDiskFreeSpaceExW": extern "system" fn(*const u16, *mut u64, *mut u64, *mut u64) -> i32)(
        p.ptr(),
        available,
        total,
        free,
    )
}

/// Wants a volume root: a virtual drive answers for the host volume its
/// folder is on.
pub extern "system" fn GetDiskFreeSpaceW(
    root: *const u16,
    sectors_per_cluster: *mut u32,
    bytes_per_sector: *mut u32,
    free_clusters: *mut u32,
    clusters: *mut u32,
) -> i32 {
    let mut p = wide(root);
    if let Some(host) = &mut p.1
        && let [letter, colon, sep, ..] = host[..]
        && colon == u16::from(b':')
        && SEPARATORS.contains(&sep)
    {
        *host = vec![letter, colon, sep, 0];
    }
    host!("kernel32.dll", "GetDiskFreeSpaceW": extern "system" fn(*const u16, *mut u32, *mut u32, *mut u32, *mut u32) -> i32)(
        p.ptr(),
        sectors_per_cluster,
        bytes_per_sector,
        free_clusters,
        clusters,
    )
}

pub extern "system" fn FindFirstFileW(path: *const u16, data: *mut c_void) -> Handle {
    let p = wide(path);
    host!("kernel32.dll", "FindFirstFileW": extern "system" fn(*const u16, *mut c_void) -> Handle)(
        p.ptr(),
        data,
    )
}

pub extern "system" fn FindFirstFileExW(
    path: *const u16,
    level: i32,
    data: *mut c_void,
    search: i32,
    filter: *mut c_void,
    flags: u32,
) -> Handle {
    let p = wide(path);
    host!("kernel32.dll", "FindFirstFileExW": extern "system" fn(*const u16, i32, *mut c_void, i32, *mut c_void, u32) -> Handle)(
        p.ptr(),
        level,
        data,
        search,
        filter,
        flags,
    )
}

pub extern "system" fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32 {
    let (from, to) = (wide(from), wide(to));
    host!("kernel32.dll", "MoveFileExW": extern "system" fn(*const u16, *const u16, u32) -> i32)(
        from.ptr(),
        to.ptr(),
        flags,
    )
}

pub extern "system" fn SetCurrentDirectoryA(path: *const c_char) -> i32 {
    match narrow(path) {
        Some(p) => {
            host!("kernel32.dll", "SetCurrentDirectoryW": extern "system" fn(*const u16) -> i32)(
                p.as_ptr(),
            )
        }
        None => {
            host!("kernel32.dll", "SetCurrentDirectoryA": extern "system" fn(*const c_char) -> i32)(
                path,
            )
        }
    }
}

// The C runtime (ucrtbase, which the title's CRT imports bind to).

pub extern "C" fn _wfopen(path: *const u16, mode: *const u16) -> File {
    let p = wide(path);
    stream_opened(
        host!("ucrtbase.dll", "_wfopen": extern "C" fn(*const u16, *const u16) -> File)(p.ptr(), mode)
    )
}

pub extern "C" fn _wfopen_s(out: *mut File, path: *const u16, mode: *const u16) -> i32 {
    let p = wide(path);
    host!("ucrtbase.dll", "_wfopen_s": extern "C" fn(*mut File, *const u16, *const u16) -> i32)(
        out,
        p.ptr(),
        mode,
    )
}

pub extern "C" fn _wfsopen(path: *const u16, mode: *const u16, share: i32) -> File {
    let p = wide(path);
    stream_opened(
        host!("ucrtbase.dll", "_wfsopen": extern "C" fn(*const u16, *const u16, i32) -> File)(
            p.ptr(),
            mode,
            share,
        )
    )
}

// Kernel reads into title memory. The GPU side leaves blocks of title GPU
// buffers read-only to see the title's next write, and the kernel's writes
// fail there instead of faulting into its handler; the title's
// synchronous reads (title+0x313fc0) read straight into 4 KiB-aligned
// destinations and retry a failed `ReadFile` forever. So the destination
// is opened first, and not guarded again before the call is done (or, for
// a read that goes on after the call, for `PENDING_KEEP` more). A read the
// host still refuses for the buffer is tried again; every failure and every
// short read the file's end does not explain is a warning (`reads`).

/// How long the destination of a read that goes on after its call stays
/// open: the kernel completes it in that time or the title waits for it.
const PENDING_KEEP: std::time::Duration = std::time::Duration::from_secs(2);

/// How often a read refused for its buffer is tried again (about 0.25 s
/// in all), as DirectStorage's are.
const READ_RETRIES: u32 = 8;

/// Errors with which the host refuses a read for its buffer: what a
/// read-only destination gives.
fn refused_for_the_buffer(error: u32) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_INVALID_USER_BUFFER, ERROR_NOACCESS};
    matches!(error, ERROR_NOACCESS | ERROR_INVALID_USER_BUFFER)
}

pub extern "system" fn ReadFile(
    file: *mut c_void,
    buffer: *mut u8,
    len: u32,
    read: *mut u32,
    overlapped: *mut c_void,
) -> i32 {
    use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, GetLastError, SetLastError};
    static FAILED: AtomicU64 = AtomicU64::new(0);
    static RETRIED: AtomicU64 = AtomicU64::new(0);
    static SHORT: AtomicU64 = AtomicU64::new(0);
    let real = host!("kernel32.dll", "ReadFile": extern "system" fn(*mut c_void, *mut u8, u32, *mut u32, *mut c_void) -> i32);
    let source = reads::Source::Handle(file as usize);
    // An overlapped read names its offset; another reads at the file
    // pointer, which a refused read leaves where it was (it is put back
    // there all the same before a second try).
    // SAFETY: the title's OVERLAPPED: `Internal`, `InternalHigh`, then the
    // offset's low and high words.
    let offset = (!overlapped.is_null()).then(|| unsafe {
        let words = overlapped.cast::<u32>();
        u64::from(words.add(4).read_unaligned()) | (u64::from(words.add(5).read_unaligned()) << 32)
    });
    let before = if overlapped.is_null() { reads::position(file) } else { None };
    let mut tries = 0;
    loop {
        let writing = victory_gpu::kernel_writing(buffer as usize, len as usize);
        let done = real(file, buffer, len, read, overlapped);
        // The call's error is the title's to ask for.
        // SAFETY: plain calls.
        let error = unsafe { GetLastError() };
        if done == 0 && error == ERROR_IO_PENDING {
            // The kernel has locked the destination's pages and writes them
            // after the call: kept open until it is surely done.
            writing.keep_for(PENDING_KEEP);
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::info!(len, "ReadFile: a read of the title's goes on after the call"));
            reads::note("ReadFile (pending)", source, len.into(), 0, false);
            // SAFETY: as above.
            unsafe { SetLastError(error) };
            return done;
        }
        drop(writing);
        if done != 0 {
            // SAFETY: the title's out parameter, or its OVERLAPPED's count.
            let got = unsafe {
                if !read.is_null() {
                    read.read_unaligned()
                } else if !overlapped.is_null() {
                    overlapped.cast::<usize>().add(1).read_unaligned() as u32
                } else {
                    len
                }
            };
            if tries > 0 {
                tracing::info!(path = %reads::name(source), tries, len, "ReadFile: read after its buffer was opened again");
            }
            if got < len {
                // Short of the file's end is something else than its end.
                let end = match offset {
                    Some(at) => Some(at + u64::from(got)),
                    None => reads::position(file),
                };
                if let (Some(end), Some(size)) = (end, reads::size(source))
                    && end < size
                    && let Some(n) = reads::tell(&SHORT)
                {
                    tracing::warn!(
                        n,
                        path = %reads::name(source),
                        buffer = format_args!("{buffer:p}"),
                        asked = len,
                        got,
                        end,
                        size,
                        guarded = %reads::guarded(buffer as usize, len as usize),
                        "ReadFile: fewer bytes than asked, short of the file's end"
                    );
                }
            }
            reads::note("ReadFile", source, len.into(), got.into(), false);
            // SAFETY: as above.
            unsafe { SetLastError(error) };
            return done;
        }
        let again = refused_for_the_buffer(error)
            && tries < READ_RETRIES
            && reads::on_disk(source)
            && (offset.is_some() || before.is_some_and(|at| put_back(file, at)));
        if let Some(n) = reads::tell(if again { &RETRIED } else { &FAILED }) {
            tracing::warn!(
                n,
                path = %reads::name(source),
                buffer = format_args!("{buffer:p}"),
                len,
                offset = offset.or(before),
                error,
                tries,
                guarded = %reads::guarded(buffer as usize, len as usize),
                "{}",
                if again {
                    "ReadFile: the host refused the title's buffer; opened again and tried again"
                } else {
                    "ReadFile: a read of the title's failed"
                }
            );
        }
        if !again {
            reads::note("ReadFile", source, len.into(), 0, true);
            // SAFETY: as above.
            unsafe { SetLastError(error) };
            return done;
        }
        std::thread::sleep(std::time::Duration::from_millis(1 << tries));
        tries += 1;
    }
}

/// Moves a handle's file pointer back to `at`; whether it is there.
fn put_back(file: *mut c_void, at: u64) -> bool {
    use windows_sys::Win32::Storage::FileSystem::{FILE_BEGIN, SetFilePointerEx};
    // SAFETY: the title's handle, whose pointer was at `at` before its read.
    unsafe { SetFilePointerEx(file, at as i64, std::ptr::null_mut(), FILE_BEGIN) != 0 }
}

/// The host CRT reads large requests straight into the caller's buffer
/// through the kernel, whose write fails on a guarded block instead of
/// faulting into the guard: the destination is opened for the call, as for
/// `ReadFile`. (Smaller ones are copied in from the stream's buffer in user
/// mode, where a guarded block faults into the guard like the title's own
/// writes; `setvbuf` keeps that buffer the CRT's own.) A failed read cannot
/// be tried again (the stream's position is the CRT's), so it is a warning.
/// Returns the items read, as the CRT does.
pub extern "C" fn fread(buffer: *mut u8, size: usize, count: usize, stream: File) -> usize {
    static FAILED: AtomicU64 = AtomicU64::new(0);
    let asked = size.saturating_mul(count);
    let writing = victory_gpu::kernel_writing(buffer as usize, asked);
    let items = host!("ucrtbase.dll", "fread": extern "C" fn(*mut u8, usize, usize, File) -> usize)(
        buffer, size, count, stream,
    );
    // SAFETY: a plain call.
    let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    drop(writing);
    let source = reads::Source::Stream(stream as usize);
    let failed =
        items < count && !stream.is_null() && host!("ucrtbase.dll", "ferror": extern "C" fn(File) -> i32)(stream) != 0;
    if failed && let Some(n) = reads::tell(&FAILED) {
        let doserrno = host!("ucrtbase.dll", "__doserrno": extern "C" fn() -> *mut u32)();
        // SAFETY: the CRT's per-thread error word.
        let doserrno = unsafe { doserrno.as_ref() }.copied();
        tracing::warn!(
            n,
            path = %reads::name(source),
            buffer = format_args!("{buffer:p}"),
            asked,
            items,
            size,
            doserrno,
            error,
            guarded = %reads::guarded(buffer as usize, asked),
            "fread: a read of the title's failed"
        );
    }
    reads::note("fread", source, asked as u64, items.saturating_mul(size) as u64, failed);
    // SAFETY: as above.
    unsafe { windows_sys::Win32::Foundation::SetLastError(error) };
    items
}

/// `_read(fd, buffer, count)`: a kernel read into the caller's buffer, as
/// `fread`'s large ones (libHttpClient's).
pub extern "C" fn _read(descriptor: i32, buffer: *mut c_void, count: u32) -> i32 {
    static FAILED: AtomicU64 = AtomicU64::new(0);
    let writing = victory_gpu::kernel_writing(buffer as usize, count as usize);
    let n = host!("ucrtbase.dll", "_read": extern "C" fn(i32, *mut c_void, u32) -> i32)(descriptor, buffer, count);
    // SAFETY: a plain call.
    let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    drop(writing);
    let source = reads::Source::Descriptor(descriptor);
    if n < 0
        && let Some(told) = reads::tell(&FAILED)
    {
        tracing::warn!(
            n = told,
            path = %reads::name(source),
            buffer = format_args!("{buffer:p}"),
            count,
            error,
            guarded = %reads::guarded(buffer as usize, count as usize),
            "_read: a read of the title's failed"
        );
    }
    reads::note("_read", source, count.into(), n.max(0) as u64, n < 0);
    // SAFETY: as above.
    unsafe { windows_sys::Win32::Foundation::SetLastError(error) };
    n
}

/// `setvbuf(stream, buffer, mode, size)`: a buffer of the title's would be
/// filled by the kernel at every refill of the stream (`fgetc`, small
/// `fread`s), where no wrapper of ours opens it. The CRT is given none and
/// makes its own of that size; the title never reads a stream's buffer
/// itself (its contents are the CRT's, and `std::basic_filebuf` asks for
/// the pointers again after `setvbuf`).
pub extern "C" fn setvbuf(stream: File, buffer: *mut c_char, mode: i32, size: usize) -> i32 {
    const _IONBF: i32 = 4;
    let buffer = if buffer.is_null() || mode == _IONBF {
        buffer
    } else {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::info!(size, "setvbuf: a stream buffer of the title's left out; the CRT makes its own"));
        std::ptr::null_mut()
    };
    host!("ucrtbase.dll", "setvbuf": extern "C" fn(File, *mut c_char, i32, usize) -> i32)(stream, buffer, mode, size)
}

/// A file opened by one of our wrappers: a name kept for an earlier one
/// with the same handle is another file's.
fn handle_opened(handle: Handle) -> Handle {
    if handle as isize != -1 {
        reads::opened(reads::Source::Handle(handle as usize));
    }
    handle
}

fn stream_opened(stream: File) -> File {
    if !stream.is_null() {
        reads::opened(reads::Source::Stream(stream as usize));
    }
    stream
}

fn descriptor_opened(descriptor: i32) -> i32 {
    if descriptor >= 0 {
        reads::opened(reads::Source::Descriptor(descriptor));
    }
    descriptor
}

pub extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> File {
    stream_opened(
        match narrow(path) {
            Some(p) => host!("ucrtbase.dll", "_wfopen": extern "C" fn(*const u16, *const u16) -> File)(
                p.as_ptr(),
                widen(mode).as_ptr(),
            ),
            None => {
                host!("ucrtbase.dll", "fopen": extern "C" fn(*const c_char, *const c_char) -> File)(
                    path, mode,
                )
            }
        }
    )
}

pub extern "C" fn fopen_s(out: *mut File, path: *const c_char, mode: *const c_char) -> i32 {
    match narrow(path) {
        Some(p) => {
            host!("ucrtbase.dll", "_wfopen_s": extern "C" fn(*mut File, *const u16, *const u16) -> i32)(
                out,
                p.as_ptr(),
                widen(mode).as_ptr(),
            )
        }
        None => {
            host!("ucrtbase.dll", "fopen_s": extern "C" fn(*mut File, *const c_char, *const c_char) -> i32)(
                out, path, mode,
            )
        }
    }
}

/// `_open(path, flags[, mode])` is variadic; on x64 the optional integer
/// travels in the same register either way, so it is passed on as is.
pub extern "C" fn _open(path: *const c_char, flags: i32, mode: i32) -> i32 {
    descriptor_opened(
        match narrow(path) {
            Some(p) => host!("ucrtbase.dll", "_wopen": extern "C" fn(*const u16, i32, i32) -> i32)(
                p.as_ptr(),
                flags,
                mode,
            ),
            None => host!("ucrtbase.dll", "_open": extern "C" fn(*const c_char, i32, i32) -> i32)(
                path, flags, mode,
            ),
        }
    )
}

/// See [`_open`]; used by libHttpClient.
pub extern "C" fn _wopen(path: *const u16, flags: i32, mode: i32) -> i32 {
    let p = wide(path);
    descriptor_opened(
        host!("ucrtbase.dll", "_wopen": extern "C" fn(*const u16, i32, i32) -> i32)(
            p.ptr(),
            flags,
            mode,
        )
    )
}

pub extern "C" fn _wstat64(path: *const u16, stat: *mut c_void) -> i32 {
    let p = wide(path);
    host!("ucrtbase.dll", "_wstat64": extern "C" fn(*const u16, *mut c_void) -> i32)(p.ptr(), stat)
}

pub extern "C" fn _stat64i32(path: *const c_char, stat: *mut c_void) -> i32 {
    match narrow(path) {
        Some(p) => {
            host!("ucrtbase.dll", "_wstat64i32": extern "C" fn(*const u16, *mut c_void) -> i32)(
                p.as_ptr(),
                stat,
            )
        }
        None => {
            host!("ucrtbase.dll", "_stat64i32": extern "C" fn(*const c_char, *mut c_void) -> i32)(
                path, stat,
            )
        }
    }
}

pub extern "C" fn remove(path: *const c_char) -> i32 {
    match narrow(path) {
        Some(p) => host!("ucrtbase.dll", "_wremove": extern "C" fn(*const u16) -> i32)(p.as_ptr()),
        None => host!("ucrtbase.dll", "remove": extern "C" fn(*const c_char) -> i32)(path),
    }
}

pub extern "C" fn _wchmod(path: *const u16, mode: i32) -> i32 {
    let p = wide(path);
    host!("ucrtbase.dll", "_wchmod": extern "C" fn(*const u16, i32) -> i32)(p.ptr(), mode)
}

pub extern "C" fn _wutime64(path: *const u16, times: *mut c_void) -> i32 {
    let p = wide(path);
    host!("ucrtbase.dll", "_wutime64": extern "C" fn(*const u16, *mut c_void) -> i32)(
        p.ptr(),
        times,
    )
}

// The C++ library: `std::basic_filebuf::open` goes through `_Fiopen`.

const FIOPEN: &str = "?_Fiopen@std@@YAPEAU_iobuf@@PEBDHH@Z";
const FIOPEN_WIDE: &str = "?_Fiopen@std@@YAPEAU_iobuf@@PEB_WHH@Z";

/// `_Fiopen(const wchar_t* path, ios_base::openmode, int protection)`.
extern "C" fn fiopen_wide(path: *const u16, mode: i32, protection: i32) -> File {
    let p = wide(path);
    stream_opened(
        host!("msvcp140.dll", "?_Fiopen@std@@YAPEAU_iobuf@@PEB_WHH@Z": extern "C" fn(*const u16, i32, i32) -> File)(
            p.ptr(),
            mode,
            protection,
        )
    )
}

/// `_Fiopen(const char* path, ios_base::openmode, int protection)`.
extern "C" fn fiopen(path: *const c_char, mode: i32, protection: i32) -> File {
    stream_opened(
        match narrow(path) {
            Some(p) => {
                host!("msvcp140.dll", "?_Fiopen@std@@YAPEAU_iobuf@@PEB_WHH@Z": extern "C" fn(*const u16, i32, i32) -> File)(
                    p.as_ptr(),
                    mode,
                    protection,
                )
            }
            None => {
                host!("msvcp140.dll", "?_Fiopen@std@@YAPEAU_iobuf@@PEBDHH@Z": extern "C" fn(*const c_char, i32, i32) -> File)(
                    path, mode, protection,
                )
            }
        }
    )
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "files",
        dlls: &[
            "kernel32.dll",
            "kernelbase.dll",
            "api-ms-win-core-file-*",
            "api-ms-win-core-processenvironment-*",
            "ucrtbase.dll",
            "api-ms-win-crt-stdio-*",
            "api-ms-win-crt-filesystem-*",
            "api-ms-win-crt-time-*",
            "msvcp140.dll",
        ],
        exports: vec![
            hle_export!(ReadFile),
            hle_export!(fread),
            hle_export!(_read),
            hle_export!(setvbuf),
            hle_export!(CreateFileW),
            hle_export!(CreateFileA),
            hle_export!(CreateFile2),
            hle_export!(DeleteFileA),
            hle_export!(DeleteFileW),
            hle_export!(RemoveDirectoryW),
            hle_export!(SetFileAttributesW),
            hle_export!(GetFileAttributesA),
            hle_export!(CreateDirectoryW),
            hle_export!(GetFileAttributesW),
            hle_export!(GetFileAttributesExW),
            hle_export!(GetDiskFreeSpaceExW),
            hle_export!(GetDiskFreeSpaceW),
            hle_export!(FindFirstFileW),
            hle_export!(FindFirstFileExW),
            hle_export!(MoveFileExW),
            hle_export!(SetCurrentDirectoryA),
            hle_export!(_wfopen),
            hle_export!(_wfopen_s),
            hle_export!(_wfsopen),
            hle_export!(fopen),
            hle_export!(fopen_s),
            hle_export!(_open),
            hle_export!(_wopen),
            hle_export!(_wstat64),
            hle_export!(_stat64i32),
            hle_export!(remove),
            hle_export!(_wchmod),
            hle_export!(_wutime64),
            HleExport {
                name: FIOPEN,
                address: fiopen as *const () as usize,
            },
            HleExport {
                name: FIOPEN_WIDE,
                address: fiopen_wide as *const () as usize,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// A fresh allocation of `pages` 4 KiB pages; `guard(page, true)` makes
    /// one read-only, as a guard of the GPU side's would be.
    fn pages(pages: usize) -> (usize, impl Fn(usize, bool) -> i32 + Send + Copy + 'static) {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RESERVE, PAGE_READONLY, PAGE_READWRITE, VirtualAlloc, VirtualProtect,
        };
        // SAFETY: a fresh allocation of the test's own.
        let at = unsafe { VirtualAlloc(std::ptr::null(), pages * 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) } as usize;
        assert_ne!(at, 0);
        let guard = move |page: usize, on: bool| {
            let mut old = 0;
            // SAFETY: as above, one of its pages.
            unsafe { VirtualProtect((at + page * 4096) as *const _, 4096, if on { PAGE_READONLY } else { PAGE_READWRITE }, &mut old) }
        };
        (at, guard)
    }

    /// The host refuses a read into a page that is read-only for a moment
    /// (guarded again between our opening it and the kernel's write): the
    /// read is tried again from where it began, and lands whole.
    #[test]
    fn a_read_file_into_memory_guarded_for_a_moment_is_tried_again() {
        use std::io::{Seek, SeekFrom};
        use std::os::windows::io::AsRawHandle;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        let bytes: Vec<u8> = (0..5 * 4096).map(|i| (i * 7 + i / 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        file.seek(SeekFrom::Start(4096)).unwrap();
        let (at, guard) = pages(3);
        assert_ne!(guard(1, true), 0);
        let opener = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            guard(1, false)
        });
        let mut n = 0;
        let done = ReadFile(file.as_raw_handle().cast(), at as *mut u8, 3 * 4096, &mut n, std::ptr::null_mut());
        assert_ne!(opener.join().unwrap(), 0);
        assert_ne!(done, 0, "tried again once the page was open");
        assert_eq!(n, 3 * 4096);
        // SAFETY: the allocation, all of it readable.
        assert_eq!(unsafe { std::slice::from_raw_parts(at as *const u8, 3 * 4096) }, &bytes[4096..4 * 4096]);
        assert_eq!(file.stream_position().unwrap(), 4 * 4096, "read once, from where it began");

        // An overlapped read names its own offset; tried again there.
        assert_ne!(guard(0, true), 0);
        let opener = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            guard(0, false)
        });
        // SAFETY: plain data, the offset's words set below.
        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.Anonymous.Anonymous.Offset = 2 * 4096;
        let done = ReadFile(file.as_raw_handle().cast(), at as *mut u8, 4096, &mut n, (&raw mut overlapped).cast());
        assert_ne!(opener.join().unwrap(), 0);
        assert_ne!(done, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { std::slice::from_raw_parts(at as *const u8, 4096) }, &bytes[2 * 4096..3 * 4096]);
    }

    /// A read the host keeps refusing for its buffer fails as it did, with
    /// the title's error, after the tries.
    #[test]
    fn a_read_file_into_memory_that_stays_read_only_fails() {
        use std::os::windows::io::AsRawHandle;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, vec![1u8; 8192]).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let (at, guard) = pages(1);
        assert_ne!(guard(0, true), 0);
        let mut n = 0;
        let done = ReadFile(file.as_raw_handle().cast(), at as *mut u8, 4096, &mut n, std::ptr::null_mut());
        // SAFETY: a plain call.
        let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        assert_eq!(done, 0);
        assert!(refused_for_the_buffer(error), "error {error}");
        // SAFETY: the allocation, readable.
        assert_eq!(unsafe { (at as *const u8).read() }, 0, "nothing landed");
    }

    /// An overlapped read that goes on after the call (a pipe with nothing
    /// in it yet) lands when the data comes, and its call says so.
    #[test]
    fn an_overlapped_read_left_pending_lands() {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, GetLastError, WAIT_OBJECT_0};
        use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_INBOUND, WriteFile};
        use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
        use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const PIPE_TYPE_BYTE: u32 = 0;
        let pipe = host!("kernel32.dll", "CreateNamedPipeW": extern "system" fn(*const u16, u32, u32, u32, u32, u32, u32, *const c_void) -> Handle);
        let name = w(&format!(r"\\.\pipe\victory-read-test-{}", std::process::id()));
        // SAFETY: plain calls on handles of the test's own.
        unsafe {
            let server = pipe(name.as_ptr(), PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED, PIPE_TYPE_BYTE, 1, 0, 4096, 0, std::ptr::null());
            assert_ne!(server as isize, -1);
            let client = CreateFileW(name.as_ptr(), GENERIC_WRITE, 0, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
            assert_ne!(client as isize, -1);
            let (at, _) = pages(1);
            let mut overlapped: OVERLAPPED = std::mem::zeroed();
            overlapped.hEvent = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
            let done = ReadFile(server, at as *mut u8, 13, std::ptr::null_mut(), (&raw mut overlapped).cast());
            assert_eq!((done, GetLastError()), (0, ERROR_IO_PENDING));
            let mut n = 0;
            assert_ne!(WriteFile(client, b"from the pipe".as_ptr(), 13, &mut n, std::ptr::null_mut()), 0);
            assert_eq!(WaitForSingleObject(overlapped.hEvent, 5000), WAIT_OBJECT_0);
            assert_ne!(GetOverlappedResult(server, &overlapped, &mut n, 0), 0);
            assert_eq!(n, 13);
            assert_eq!(std::slice::from_raw_parts(at as *const u8, 13), b"from the pipe");
            CloseHandle(overlapped.hEvent);
            CloseHandle(client);
            CloseHandle(server);
        }
    }

    /// `_read` reads into the caller's buffer; a stream buffer the title
    /// names for `setvbuf` is left alone (the CRT fills one of its own).
    #[test]
    fn descriptor_reads_and_title_stream_buffers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let path = format!("{}\0", path.display());
        const O_RDONLY: i32 = 0;
        const O_BINARY: i32 = 0x8000;
        let fd = _open(path.as_ptr().cast(), O_RDONLY | O_BINARY, 0);
        assert!(fd >= 0);
        let mut out = [0u8; 16];
        assert_eq!(_read(fd, out.as_mut_ptr().cast(), 16), 10);
        assert_eq!(&out[..10], b"0123456789");
        host!("ucrtbase.dll", "_close": extern "C" fn(i32) -> i32)(fd);

        let f = fopen(path.as_ptr().cast(), c"rb".as_ptr());
        assert!(!f.is_null());
        let mut buffer = vec![0xaau8; 64];
        const _IOFBF: i32 = 0;
        assert_eq!(setvbuf(f, buffer.as_mut_ptr().cast(), _IOFBF, buffer.len()), 0);
        let mut got = [0u8; 4];
        assert_eq!(fread(got.as_mut_ptr(), 1, 4, f), 4);
        assert_eq!(&got, b"0123");
        assert!(buffer.iter().all(|&b| b == 0xaa), "the CRT read into a buffer of its own");
        assert_eq!(fread(out.as_mut_ptr(), 1, 16, f), 6, "short at the file's end, no failure");
        host!("ucrtbase.dll", "fclose": extern "C" fn(File) -> i32)(f);
    }

    #[test]
    fn kernel_reads_reach_the_buffer() {
        use std::os::windows::io::AsRawHandle;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let (mut out, mut n) = ([0u8; 10], 0);
        let handle = file.as_raw_handle().cast();
        assert_ne!(ReadFile(handle, out.as_mut_ptr(), 10, &mut n, std::ptr::null_mut()), 0);
        assert_eq!((&out, n), (b"0123456789", 10));
    }

    #[test]
    fn fread_reads_beside_in_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        // Over two chunks, and not a whole number of 3-byte items.
        let bytes: Vec<u8> = (0..(5 << 19) + 1).map(|i| (i * 7 + i / 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let fclose = host!("ucrtbase.dll", "fclose": extern "C" fn(File) -> i32);
        let open = || {
            let f = _wfopen(w(&path.to_string_lossy()).as_ptr(), w("rb").as_ptr());
            assert!(!f.is_null());
            f
        };

        // Asked for far more than the file holds: the whole file, in items.
        let f = open();
        let mut out = vec![0xaau8; bytes.len() + 64];
        let items = fread(out.as_mut_ptr(), 3, out.len() / 3, f);
        assert_eq!(items, bytes.len() / 3);
        assert_eq!(&out[..bytes.len()], &bytes[..], "the partial last item's bytes too");
        assert!(out[bytes.len()..].iter().all(|&b| b == 0xaa), "nothing past what was read");
        fclose(f);

        // Exact reads continue where the last one stopped.
        let f = open();
        let mut a = vec![0u8; 1000];
        let mut b = vec![0u8; (1 << 20) + 5];
        assert_eq!(fread(a.as_mut_ptr(), 1000, 1, f), 1);
        assert_eq!(fread(b.as_mut_ptr(), 1, b.len(), f), b.len());
        assert_eq!(a, bytes[..1000]);
        assert_eq!(b, bytes[1000..1000 + b.len()]);
        fclose(f);
    }

    fn text(v: &[u16]) -> String {
        String::from_utf16_lossy(&v[..v.len() - 1])
    }

    #[test]
    fn rewrites_only_paths_on_mounted_drives() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let letter = mount(dir.path()).unwrap();
        assert_eq!(
            mount(dir.path()),
            Some(letter),
            "mounting twice reuses the letter"
        );
        let tr = |s: String| translate(&w(&s)[..s.encode_utf16().count()]).map(|v| text(&v));
        assert_eq!(
            tr(format!("{letter}:/a/b.txt")),
            Some(format!("{root}/a/b.txt"))
        );
        let lower = letter.to_ascii_lowercase();
        assert_eq!(tr(format!("{lower}:\\a")), Some(format!("{root}\\a")));
        assert_eq!(tr(format!("{letter}:")), Some(format!("{root}\\")));
        assert_eq!(
            tr(format!(r"\\?\{letter}:/a/b")),
            Some(format!(r"\\?\{root}\a\b"))
        );
        assert_eq!(tr(r"C:\Windows".into()), None);
        assert_eq!(tr("relative/path".into()), None);
        assert_eq!(tr(String::new()), None);

        // Fixed letters, as the console's package drive; a later mount of
        // the same letter replaces the earlier one.
        let package = tempfile::tempdir().unwrap();
        mount_at('g', dir.path());
        mount_at('G', package.path());
        assert_eq!(
            tr(r"G:\appdata0.rpf".into()),
            Some(format!(r"{}\appdata0.rpf", package.path().display()))
        );
        assert_ne!(mount(dir.path()), Some('G'), "free letters skip fixed ones");

        // Long ones take the `\\?\` form, cleaned up as the host would.
        let long = "d".repeat(250);
        assert_eq!(
            tr(format!(r"G:/a/./b/../{long}//f")),
            Some(format!(r"\\?\{}\a\{long}\f", package.path().display()))
        );
        assert_eq!(
            verbatim(&w(r"\\server\share\")[..15], &w("/x/../../y")[..10]),
            w(r"\\?\UNC\server\share\y")[..22].to_vec()
        );
    }

    /// A title run by its file name alone has an empty folder: the drive
    /// over it is the current directory, not the current drive's root.
    #[test]
    fn an_empty_root_is_the_current_directory() {
        let letter = mount(Path::new("")).unwrap();
        let path = format!(r"{letter}:\a.rpf");
        let host = translate(&w(&path)[..path.len()]).map(|v| text(&v));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(host, Some(cwd.join("a.rpf").display().to_string()));
    }

    #[test]
    fn long_title_paths_work_on_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let letter = mount(dir.path()).unwrap();
        let deep = format!("{letter}:/{}/{}", "a".repeat(200), "b".repeat(100));
        let null = std::ptr::null_mut();
        let first = w(&deep[..203]);
        assert_ne!(CreateDirectoryW(first.as_ptr(), null), 0);
        assert_ne!(CreateDirectoryW(w(&deep).as_ptr(), null), 0);
        let file = format!("{deep}/../{}/f.txt", "b".repeat(100));
        let f = _wfopen(w(&file).as_ptr(), w("wb").as_ptr());
        assert!(!f.is_null(), "{file}");
        host!("ucrtbase.dll", "fclose": extern "C" fn(File) -> i32)(f);
        assert_ne!(DeleteFileW(w(&format!("{deep}/f.txt")).as_ptr()), 0);
        assert_ne!(RemoveDirectoryW(w(&deep).as_ptr()), 0);
    }

    #[test]
    fn title_file_calls_land_in_the_host_folder() {
        let dir = tempfile::tempdir().unwrap();
        let letter = mount(dir.path()).unwrap();
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const CREATE_ALWAYS: u32 = 2;
        let close = host!("kernel32.dll", "CloseHandle": extern "system" fn(Handle) -> i32);
        let fclose = host!("ucrtbase.dll", "fclose": extern "C" fn(File) -> i32);

        let path = w(&format!(r"{letter}:\created.bin"));
        let null = std::ptr::null_mut();
        let h = CreateFileW(
            path.as_ptr(),
            GENERIC_WRITE,
            0,
            null,
            CREATE_ALWAYS,
            0,
            null,
        );
        assert_ne!(h as isize, -1);
        close(h);
        assert!(dir.path().join("created.bin").is_file());

        let path = format!("{letter}:/opened.txt\0");
        let f = fopen(path.as_ptr().cast(), c"wb".as_ptr());
        assert!(!f.is_null());
        fclose(f);
        assert!(dir.path().join("opened.txt").is_file());

        let path = format!("{letter}:/stream.txt\0");
        const OUT: i32 = 2;
        let f = fiopen(path.as_ptr().cast(), OUT, 0x40);
        assert!(!f.is_null());
        fclose(f);
        assert!(dir.path().join("stream.txt").is_file());

        assert_eq!(remove(path.as_ptr().cast()), 0);
        assert!(!dir.path().join("stream.txt").exists());
        let f = fiopen_wide(w(&format!("{letter}:/wide.txt")).as_ptr(), OUT, 0x40);
        assert!(!f.is_null());
        fclose(f);
        assert!(dir.path().join("wide.txt").is_file());

        const OPEN_EXISTING: u32 = 3;
        let path = w(&format!(r"{letter}:\created.bin"));
        let h = CreateFile2(path.as_ptr(), GENERIC_WRITE, 0, OPEN_EXISTING, null);
        assert_ne!(h as isize, -1);
        close(h);
        const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
        assert_ne!(GetFileAttributesW(path.as_ptr()), INVALID_FILE_ATTRIBUTES);

        const O_WRONLY: i32 = 1;
        const O_CREAT: i32 = 0x100;
        const S_IWRITE: i32 = 0x80;
        let path = format!("{letter}:/low.bin\0");
        let fd = _open(path.as_ptr().cast(), O_WRONLY | O_CREAT, S_IWRITE);
        assert!(fd >= 0);
        host!("ucrtbase.dll", "_close": extern "C" fn(i32) -> i32)(fd);
        assert!(dir.path().join("low.bin").is_file());

        // Other paths pass through untouched.
        let host_path = format!("{}\0", dir.path().join("plain.txt").display());
        let f = fopen(host_path.as_ptr().cast(), c"wb".as_ptr());
        assert!(!f.is_null());
        fclose(f);
        assert!(dir.path().join("plain.txt").is_file());
    }

    /// A narrow path is rewritten once, as its wide twin is. The inner
    /// drive's folder lies on the outer virtual drive, as the title folder
    /// does when it sits on a host `G:` drive and is mounted as `G:`. The
    /// outer letter is no host drive, so the host finds nothing there; a
    /// narrow call that reaches the outer folder rewrote the path twice.
    #[test]
    fn narrow_paths_are_rewritten_once() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("inner");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("present.txt"), b"").unwrap();
        let outer = mount(dir.path()).unwrap();
        let inner = mount(Path::new(&format!(r"{outer}:\inner"))).unwrap();
        let path = |name: &str| format!("{inner}:/{name}\0");

        const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
        let wide = GetFileAttributesW(w(&format!("{inner}:/present.txt")).as_ptr());
        assert_eq!(wide, INVALID_FILE_ATTRIBUTES);
        let narrow = GetFileAttributesA(path("present.txt").as_ptr().cast());
        assert_eq!(narrow, wide, "GetFileAttributesA agrees with its wide twin");

        const GENERIC_WRITE: u32 = 0x4000_0000;
        const CREATE_ALWAYS: u32 = 2;
        let null = std::ptr::null_mut();
        let h = CreateFileA(
            path("a.bin").as_ptr().cast(),
            GENERIC_WRITE,
            0,
            null,
            CREATE_ALWAYS,
            0,
            null,
        );
        assert_eq!(h as isize, -1);
        assert!(fopen(path("b.txt").as_ptr().cast(), c"wb".as_ptr()).is_null());
        let mut f: File = std::ptr::null_mut();
        let error = fopen_s(&mut f, path("c.txt").as_ptr().cast(), c"wb".as_ptr());
        assert_ne!(error, 0);
        const OUT: i32 = 2;
        assert!(fiopen(path("d.txt").as_ptr().cast(), OUT, 0x40).is_null());

        // Nothing was made behind a path rewritten twice.
        let names: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["present.txt"]);
    }
}

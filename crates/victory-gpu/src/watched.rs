//! Host memory handed to the title that the kernel write-watches
//! (`VICTORY_WATCHED_HEAP`, on by default; `0`: the process heap, as before).
//!
//! The title's general allocator (`XMemAllocDefault` in victory-hle, behind
//! the title's own allocation hooks) and the heaps the title makes through the
//! device (`resource::Memory`) took their memory from the process heap. That
//! memory is not write-watched, so a host copy of a buffer there is
//! `NOT_WATCHED` and copied whole at every refresh (`Shadow::update_free`):
//! the title's upscaler scratch buffer, 21,045,760 bytes, was copied twice a
//! frame (41.6 MB, 6-8 ms) and sent up whole to its card copy every frame,
//! although the title's CPU never writes it.
//!
//! Blocks of at least [`LEAST`] bytes come from a reservation of their own made
//! with `MEM_WRITE_WATCH`, so `watch::writes` answers for them as for the
//! runtime's own title memory: a page nothing wrote since a copy took it is not
//! copied again. What writes it (the title, the kernel, our landings) sets the
//! kernel's bits as in any write-watched reservation.
//!
//! Layout of a block: a reservation of whole 64 KiB, so that every 64 KiB page
//! ([`crate::watch::PAGE`]) of the caller's bytes lies in it and an asking about
//! them is never refused for reaching into the next reservation. The caller's
//! bytes start at a multiple of their alignment (at least 4 KiB) with at least
//! one committed 4 KiB page below them: its first word holds the reservation's
//! base, for [`free`]; the rest of that page is the caller's (victory-hle keeps
//! its block header just below the pointer). Only the caller's pages and that
//! one are committed.

use std::ptr::NonNull;

use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};

/// Blocks this large or larger are write-watched; smaller ones stay with the
/// process heap (a reservation is 64 KiB of address space at least).
pub const LEAST: usize = 64 << 10;

const MEM_WRITE_WATCH: u32 = 0x20_0000;
const KERNEL_PAGE: usize = 4096;
const GRAIN: usize = 64 << 10;

/// Whether big blocks for the title are write-watched (`VICTORY_WATCHED_HEAP=0`:
/// they come from the process heap, as before 2026-11-27).
pub fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_WATCHED_HEAP").is_none_or(|v| v != "0"))
}

/// Whether a block of `size` bytes is one for [`alloc`].
pub fn wanted(size: usize) -> bool {
    size >= LEAST && on()
}

/// `size` zeroed bytes aligned to `align` (a power of two), write-watched,
/// with the 4 KiB below them committed and the first word of that page ours.
/// `None` if the kernel refuses (no address space or commit).
pub fn alloc(size: usize, align: usize) -> Option<NonNull<u8>> {
    let align = align.max(KERNEL_PAGE);
    if size == 0 || !align.is_power_of_two() {
        return None;
    }
    // The base is a multiple of 64 KiB, so for any alignment the caller's
    // bytes start at most `align` past it, with a page below them.
    let len = align.checked_add(size)?.checked_next_multiple_of(GRAIN)?;
    // SAFETY: a fresh reservation of our own.
    let base = unsafe {
        VirtualAlloc(
            std::ptr::null(),
            len,
            MEM_RESERVE | MEM_WRITE_WATCH,
            PAGE_READWRITE,
        )
    } as usize;
    if base == 0 {
        return None;
    }
    let user = (base + KERNEL_PAGE).next_multiple_of(align);
    let commit = user - KERNEL_PAGE;
    let committed = (user + size).next_multiple_of(KERNEL_PAGE) - commit;
    debug_assert!(user + size <= base + len);
    // SAFETY: inside the reservation just made.
    let at =
        unsafe { VirtualAlloc(commit as *const _, committed, MEM_COMMIT, PAGE_READWRITE) } as usize;
    if at == 0 {
        // SAFETY: the reservation made above, nothing in it handed out.
        unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) };
        return None;
    }
    // SAFETY: the first word of the committed page below the caller's bytes.
    unsafe { (commit as *mut usize).write(base) };
    NonNull::new(user as *mut u8)
}

/// Gives back a block from [`alloc`].
///
/// # Safety
/// `user` came from [`alloc`] and is not used after this.
pub unsafe fn free(user: *mut u8) {
    // SAFETY: the caller's: the page below the block holds its base.
    let base = unsafe { ((user as usize - KERNEL_PAGE) as *const usize).read() };
    // SAFETY: the whole reservation, ours.
    unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::PAGE;

    /// The pages of `[from, to)` (64 KiB pages, rounded out) the kernel says
    /// were written since it was last asked, or `None` if it refuses.
    fn asked(from: usize, to: usize) -> Option<usize> {
        use windows_sys::Win32::System::Memory::GetWriteWatch;
        let (first, last) = (from / PAGE * PAGE, to.next_multiple_of(PAGE));
        let mut found = vec![0usize; (last - first) / KERNEL_PAGE];
        let (mut count, mut granularity) = (found.len(), 0u32);
        // SAFETY: the answer buffer holds `count` entries.
        let failed = unsafe {
            GetWriteWatch(
                1,
                first as *const _,
                last - first,
                found.as_mut_ptr().cast(),
                &mut count,
                &mut granularity,
            )
        } != 0;
        (!failed).then_some(count)
    }

    #[test]
    fn a_block_is_zeroed_aligned_and_asked_about_whole_pages_without_refusal() {
        for (size, align) in [
            (LEAST, 16),
            (21_045_760, 1 << 16),
            (100_000, 4096),
            (3 * PAGE + 5, 1 << 21),
            (LEAST + 1, 256),
        ] {
            let p = alloc(size, align).expect("memory").as_ptr() as usize;
            assert_eq!(p % align.max(KERNEL_PAGE), 0, "{size} {align}");
            // SAFETY: the block's bytes.
            let bytes = unsafe { std::slice::from_raw_parts_mut(p as *mut u8, size) };
            assert!(bytes.iter().all(|&b| b == 0));
            // The 64 KiB pages around the block are one reservation, its
            // uncommitted pages too: the kernel answers.
            assert!(asked(p, p + size).is_some(), "{size} {align}");
            assert_eq!(asked(p, p + size), Some(0));
            bytes[size - 1] = 1;
            bytes[0] = 2;
            assert_eq!(asked(p, p + size), Some(2));
            assert_eq!(asked(p, p + size), Some(0));
            // SAFETY: from `alloc`, not used again.
            unsafe { free(p as *mut u8) };
        }
    }

    #[test]
    fn the_journal_tells_a_block_s_writes_to_every_asker() {
        let size = 4 * PAGE;
        let p = alloc(size, PAGE).expect("memory").as_ptr() as usize;
        let (none, _) = crate::watch::writes(p, p + size, false).expect("write-watched");
        // SAFETY: the block's bytes.
        unsafe { ((p + 2 * PAGE + 100) as *mut u32).write(7) };
        let (now, written) = crate::watch::writes(p, p + size, false).expect("write-watched");
        assert!(now > none);
        assert!(
            written
                .iter()
                .any(|&(page, w)| page == p / PAGE + 2 && w == now),
            "{written:?}"
        );
        // SAFETY: from `alloc`, not used again.
        unsafe { free(p as *mut u8) };
    }

    #[test]
    fn a_kernel_write_into_a_block_is_seen() {
        use std::io::{Read, Seek, Write};
        let size = 2 * PAGE;
        let p = alloc(size, PAGE).expect("memory").as_ptr() as usize;
        let _ = crate::watch::writes(p, p + size, false).expect("write-watched");
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&[9u8; 5000]).unwrap();
        file.rewind().unwrap();
        // SAFETY: bytes of the block, nothing else refers to them.
        let into = unsafe { std::slice::from_raw_parts_mut((p + PAGE + 3000) as *mut u8, 5000) };
        assert_eq!(file.read(into).unwrap(), 5000);
        let (now, written) = crate::watch::writes(p, p + size, false).expect("write-watched");
        assert!(
            written
                .iter()
                .any(|&(page, w)| page == p / PAGE + 1 && w == now),
            "{written:?}"
        );
        // SAFETY: from `alloc`, not used again.
        unsafe { free(p as *mut u8) };
    }
}

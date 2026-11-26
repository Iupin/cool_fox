//! Title memory by address, for translated shaders that read and write
//! through raw 40-bit addresses (`victory_shader::emit::SLOT_MEMORY`: the
//! console's ray-tracing structure builder and the programs that walk its
//! structures; the structures themselves lie in plain title memory, in no
//! buffer object).
//!
//! A 64 KiB page in a title buffer that holds all of it is in that buffer's
//! copy ([`crate::shadow`]), which every other access uses. A page outside
//! buffers has a page of its own in an **arena**: memory of ours with a copy
//! on the card ([`crate::shadow::by_address`]). The streamed structures lie
//! anywhere between other data (6.6 GB of title memory in 4 MiB windows, run
//! light-1; 8 GB in windows of 256 KiB, light-2), so a page is what is kept.
//!
//! This module keeps what a shader needs to find the pages:
//!
//! - the **directory**: per 64 KiB page of title address space the number
//!   of the copy that holds the page (0: none) and the page's byte offset
//!   in it; one for all lists, entries only ever go from empty to filled.
//!   The CPU writes it in memory of its own and shaders read a copy of that
//!   on the card, brought up to date as title memory is (over the bus, every
//!   step of every ray waited for it);
//! - per view heap a run of slots with a raw unordered access view of each
//!   numbered copy (slot 0 of the run: a null view, for pages without a
//!   copy);
//! - the **faults**: the page numbers shaders found empty, read back here
//!   once a frame, so the pages are there from the next frame on. A pointer a
//!   shader takes out of memory cannot be known before it runs. Pages made
//!   for faults are bounded a frame and held at a time, and the one made
//!   longest ago is given back for a new one ([`FaultPages`]), so a run of
//!   wild rays cannot use them up for good.
//!
//! **A page that is not there when a shader touches it is a lost access**:
//! the read gives 0, the store is dropped, and the fault note only helps
//! the next frame. For the structure builder that is the same as a dropped
//! stage, so before one of its dispatches the host makes ready what it can
//! see the dispatch name:
//!
//! - 256 KiB from each root view outside buffers, and where title memory has a
//!   packed ("dehydrated") structure there, all of it: the title ships its
//!   structures packed, and the builder's rehydration stages unpack each in
//!   place once it is streamed in;
//! - 4 MiB from each unordered access view outside buffers (structures built);
//! - the page of each pair of root constants that is a title address and of
//!   each root constant that is one in 256-byte units;
//! - the first page of every structure a list's upload memory names
//!   (a top-level build's instances).
//!
//! **What comes back to title memory**: what the builder stores in title
//! buffers of at most [`BACK`] bytes it names (the sizes the title reads to
//! make a structure smaller). Structures and scratch memory stay on the card
//! (the first whole run of the builder wrote garbage over the title's
//! objects). So an arena's page is the only place a structure is whole, and
//! title memory must not go over it: of a page the title wrote only the
//! dwords that changed since they were last taken go up. A packed structure
//! goes up whole when the builder comes to it, whatever is known of it: the
//! same bytes streamed in again do not differ from those last taken, and the
//! card holds them unpacked (run light-3: a quarter of the structures
//! unpacked a second time, into garbage all over their neighbours).
//!
//! **Stores are bounded**: a structure the builder misreads makes it store
//! anywhere. Pages of title buffers take its stores only once one of its
//! dispatches was given that buffer as an unordered access view (its scratch
//! memory, its results); what else a store reaches is an arena's.
//!
//! A packed structure is gone when title memory has no header of it any more
//! (looked at over [`SWEEP`] frames); the pages it alone has are given back.
//!
//! shortcut: a page held for anything else (named by a list, a packed
//! structure's, or made for a fault and named since) is kept for the rest of
//! the run.
//! shortcut: pages a list does not name are looked at again in turn, over
//! [`SWEEP`] frames.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use victory_shader::emit::{MEMORY_FAULTS, MEMORY_PAGES, MEMORY_READ_ONLY};
use windows::Win32::Graphics::Direct3D12::*;

use crate::descriptor::host::HostHeap;
use crate::host::{HostList, host};
use crate::shadow::Shadow;

/// Copies that can be numbered (the run of heap slots is one longer).
const COPIES: u32 = (1 << 12) - 1;
/// A directory page.
const PAGE: u64 = 1 << 16;
/// An arena's bytes.
const ARENA: u64 = 64 << 20;
/// What is compared at a time.
const PIECE: usize = 4096;
/// Pages held, at most (3 GiB).
const MOST: usize = 3 << 14;
/// Pages of title memory with small title buffers in them, at most.
const WINDOWS: usize = 512;
/// The biggest packed structure taken for one.
const PACKED: u64 = 64 << 20;
/// For so many frames what a page holds may still be on its way.
const YOUNG: u64 = 4;
/// Free pages there are, at least, when a list is recorded.
const SPARE: usize = 1024;
/// A packed structure not named for so many frames is another one.
const AGAIN: u64 = 8;
/// From a root view; from one that is written (a structure being built).
const VIEW: u64 = 256 << 10;
const BUILT: u64 = 4 << 20;
/// The biggest title buffer whose builder results come back.
const BACK: u64 = 256 << 10;
/// Pages taken from the faults of one frame, at most, and the title pages
/// looked at for them: a ray in a structure that is not right goes
/// anywhere, and 2,048 a frame took all the pages there are in the first
/// seconds (run light-4). How many are held at a time is bounded too
/// ([`fault_limits`], [`FaultPages`]); until 2026-11-26 it was 2,048 for the
/// whole run, used up some 45 frames after the world appeared.
const FAULTS: usize = 128;
const FAULT_TRIES: usize = 1024;
/// A page made from a fault is not given back for another before it was
/// held so many frames.
const FAULT_FRESH: u64 = 8;
/// For so long every frame's faults were not all given a copy before it is
/// warned.
const STARVING: std::time::Duration = std::time::Duration::from_secs(5);
/// The lowest title page a fault may name (4 GiB; lower ones are pointers
/// read as zero and the like).
const LOWEST: u64 = 0x1_0000;
/// Frames over which every page is looked at once.
const SWEEP: usize = 128;

/// What a draw or dispatch does with title memory by address.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Use {
    Reads,
    /// Stores by address, as the structure builder; `back`: its results in
    /// small title buffers come back.
    Builds { back: bool },
}

/// A page of title memory in an arena.
struct Held {
    /// Where in the arena's memory, and the arena's place in `copies`.
    at: usize,
    copy: usize,
    /// The write-watch asking after which it was last taken (0: never).
    taken: u64,
    /// The frame it was last looked at in, and the one it came in.
    looked: u64,
    born: u64,
    /// Packed structures that have bytes in it, and whether it is held for
    /// them alone.
    owners: u32,
    theirs: bool,
    /// Made from a fault and not named since: its number among those
    /// ([`FaultPages`]); such a page can be given back for another.
    fault: Option<u64>,
}

/// A packed structure the builder came to.
struct Packed {
    /// The frame it last did.
    frame: u64,
    len: u64,
    /// Its first two dwords in title memory.
    header: u64,
}

struct Memory {
    /// The CPU's entries (number, offset), and the copy shaders read.
    directory: (*mut u32, Arc<Shadow>),
    faults: (ID3D12Resource, *mut u32, u64),
    /// Copy `n` is `copies[n - 1]`.
    copies: Vec<Arc<Shadow>>,
    /// Places of title buffers' copies among `copies`.
    numbers: HashMap<usize, usize>,
    /// Arenas: place in `copies`, address.
    arenas: Vec<(usize, usize)>,
    /// By title page.
    pages: HashMap<u64, Held>,
    /// Their numbers in the order they came, and where the sweep is.
    order: Vec<u64>,
    sweep: usize,
    /// Windows of title memory itself ([`Memory::map`]).
    windows: usize,
    /// Packed structures the builder came to: the frame it last did.
    packed: HashMap<u64, Packed>,
    /// Their addresses in the order they came, and where the sweep is.
    packed_order: Vec<u64>,
    packed_sweep: usize,
    /// Places of pages that are free: the copy, the address in the arena.
    free: Vec<(usize, usize)>,
    /// Copies of title buffers the builder may store in.
    open: std::collections::HashSet<usize>,
    /// Pages taken from faults.
    faulted: FaultPages,
    /// Per host view heap (by address): the first slot of its run, and how
    /// many copies have their view there.
    heaps: HashMap<usize, (u32, u32)>,
    /// The frame the faults were last read in.
    frame: u64,
    /// Pages asked for this frame that are not the title's memory.
    refused: std::collections::HashSet<u64>,
    stats: Stats,
    #[cfg(feature = "diagnostics")]
    dump: dump::Dump,
    #[cfg(feature = "diagnostics")]
    trace: trace::Trace,
}

/// Since the last report.
#[derive(Default)]
struct Stats {
    from: Option<(std::time::Instant, u64)>,
    uses: [u64; 2],
    faults: u64,
    unknown: u64,
    back: u64,
    /// Pages looked at, bytes that changed, packed structures sent.
    looked: u64,
    changed: u64,
    packed: u64,
    packed_bytes: u64,
    gone: u64,
    /// Pages that came for an address in constants, a written view, another
    /// view, a structure named in upload memory.
    came: [u64; 4],
    micros: u64,
}

// SAFETY: the mappings are of memory the struct owns; all access is under
// the mutex.
unsafe impl Send for Memory {}

static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// What a draw binds.
pub(crate) struct Ready {
    pub(crate) directory: u64,
    pub(crate) faults: u64,
    /// The view heap slot of the null view; copy `n`'s view is `n` further.
    pub(crate) base: u32,
}

impl Memory {
    fn new() -> Option<Memory> {
        let host = host()?;
        let bytes = u64::from(MEMORY_PAGES) * 8;
        // Zeroed, and watched for writes as the title's memory is.
        let entries = crate::upload::reserve(bytes as usize)?;
        let directory = crate::shadow::by_address(entries as u64, bytes)?;
        crate::host::name(&directory.buffer, || "title memory by address: directory".into());
        let (faults, data) = host.shared_buffer(u64::from(MEMORY_FAULTS) * 4)?;
        crate::host::name(&faults, || "title memory by address: faults".into());
        // SAFETY: a fresh mapping of at least that many bytes.
        unsafe { std::ptr::write_bytes(data, 0, MEMORY_FAULTS as usize * 4) };
        // SAFETY: a live buffer.
        let gpu = unsafe { faults.GetGPUVirtualAddress() };
        tracing::info!("title memory by address: a shader reads or writes through raw addresses");
        let limits = fault_limits();
        tracing::info!(
            "title memory by address: pages for faults: at most {} made a frame ({} title pages tried), {} held at a time ({} MiB), \
             the one made longest ago given back for a new one when that many are held or the card has no room",
            limits.per_frame,
            limits.tries,
            limits.most,
            (limits.most as u64 * PAGE) >> 20
        );
        Some(Memory {
            directory: (entries as *mut u32, directory),
            faults: (faults, data.cast::<u32>(), gpu),
            copies: Vec::new(),
            numbers: HashMap::new(),
            arenas: Vec::new(),
            pages: HashMap::new(),
            order: Vec::new(),
            sweep: 0,
            windows: 0,
            packed: HashMap::new(),
            packed_order: Vec::new(),
            packed_sweep: 0,
            free: Vec::new(),
            open: Default::default(),
            faulted: FaultPages::default(),
            heaps: HashMap::new(),
            frame: u64::MAX,
            refused: Default::default(),
            stats: Stats::default(),
            #[cfg(feature = "diagnostics")]
            dump: Default::default(),
            #[cfg(feature = "diagnostics")]
            trace: Default::default(),
        })
    }

    /// The directory's entry of `page`.
    fn entry(&self, page: u64) -> *mut u32 {
        debug_assert!(page < u64::from(MEMORY_PAGES));
        // SAFETY: inside the directory.
        unsafe { self.directory.0.add(page as usize * 2) }
    }

    /// The place in `copies` of the copy that holds `page`, if one does.
    fn copy_of(&self, page: u64) -> Option<usize> {
        if page >= u64::from(MEMORY_PAGES) {
            return None;
        }
        // SAFETY: an entry of the directory.
        ((unsafe { self.entry(page).read() } & !MEMORY_READ_ONLY) as usize).checked_sub(1)
    }

    /// Whether `page` can be given a place: in the directory, not turned down
    /// this frame, and the title's GPU memory. (A shader's pointer, or a
    /// constant taken for an address, may be anything.)
    fn possible(&mut self, page: u64) -> bool {
        // Not the last entry: addresses past the directory read it, and it
        // stays empty.
        if page < LOWEST || page >= u64::from(MEMORY_PAGES) - 1 || self.refused.contains(&page) {
            return false;
        }
        let at = (page * PAGE) as usize;
        let ours = crate::texture::committed(at as *const u8, PAGE as usize)
            && crate::watch::writes(at, at + PAGE as usize, true).is_some();
        if !ours {
            self.refused.insert(page);
            self.stats.unknown += 1;
        }
        ours
    }

    /// Gives a copy a number, if one is left.
    fn number(&mut self, shadow: Arc<Shadow>) -> Option<usize> {
        let key = Arc::as_ptr(&shadow) as usize;
        if let Some(&n) = self.numbers.get(&key) {
            return Some(n);
        }
        if self.copies.len() >= COPIES as usize {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!(COPIES, "title memory by address: no number left for another copy; its pages read zeros"));
            return None;
        }
        // SAFETY: a live buffer.
        let flags = unsafe { shadow.buffer.GetDesc() }.Flags;
        if flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
            return None;
        }
        self.copies.push(shadow);
        let n = self.copies.len() - 1;
        self.numbers.insert(key, n);
        Some(n)
    }

    /// Gives `page` a place, if it has none and the memory is the title's;
    /// whether it has one then.
    fn map(&mut self, page: u64) -> bool {
        if self.copy_of(page).is_some() {
            // Named: no longer one of the pages for faults.
            if let Some(held) = self.pages.get_mut(&page)
                && held.fault.take().is_some()
            {
                self.faulted.named();
            }
            return true;
        }
        if !self.possible(page) {
            return false;
        }
        if let Some(made) = self.in_buffers(page) {
            return made;
        }
        if self.pages.len() >= MOST {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!(MOST, "title memory by address: no more pages are taken; others read zeros"));
            return false;
        }
        if self.free.is_empty() && !self.another() {
            return false;
        }
        let Some((copy, at)) = self.free.pop() else {
            return false;
        };
        self.hold(page, at, copy);
        true
    }

    /// [`Memory::map`] of a page that a title buffer's copy holds, or that
    /// is a window of title memory with small title buffers in it: whether
    /// it has a copy then. None for a page that needs an arena's.
    fn in_buffers(&mut self, page: u64) -> Option<bool> {
        let start = page * PAGE;
        if crate::shadow::in_buffer(start, PAGE).is_some() {
            let Some(shadow) = crate::shadow::find(start, PAGE) else {
                return Some(false);
            };
            let fresh = !self.numbers.contains_key(&(Arc::as_ptr(&shadow) as usize));
            let Some(n) = self.number(shadow.clone()) else {
                return Some(false);
            };
            // Every page the copy holds whole, not only the one asked for.
            // shortcut: a page of a title buffer inside this one that has a
            // copy of its own is given this copy too.
            if fresh {
                let (first, len) = shadow.span();
                let mut at = first.next_multiple_of(PAGE);
                while at + PAGE <= first + len && at / PAGE < u64::from(MEMORY_PAGES) - 1 {
                    let entry = self.entry(at / PAGE);
                    // SAFETY: an entry of the directory.
                    if unsafe { entry.read() } == 0 {
                        close(entry, n, at - first);
                    }
                    at += PAGE;
                }
            }
            // SAFETY: as above.
            if unsafe { self.entry(page).read() } == 0 {
                close(self.entry(page), n, start - shadow.span().0);
            }
            return Some(true);
        }
        // Small title buffers in it: what the builder stores in them comes
        // back, so title memory itself.
        if self.windows < WINDOWS && crate::shadow::small_buffer_in(start, PAGE, BACK) {
            if let Some(shadow) = crate::shadow::by_address(start, PAGE)
                && let Some(n) = self.number(shadow)
            {
                self.windows += 1;
                fill(self.entry(page), n, 0);
                return Some(true);
            }
            return Some(false);
        }
        None
    }

    /// Another arena's pages.
    fn another(&mut self) -> bool {
        let Some(at) = crate::upload::reserve(ARENA as usize) else {
            return false;
        };
        let Some(n) = crate::shadow::by_address(at as u64, ARENA).and_then(|s| self.number(s)) else {
            return false;
        };
        self.arenas.push((n, at));
        // The lowest first.
        self.free.extend((0..(ARENA / PAGE) as usize).rev().map(|i| (n, at + i * PAGE as usize)));
        true
    }

    /// `page` is held at `at` in the arena that is `copy`.
    fn hold(&mut self, page: u64, at: usize, copy: usize) {
        self.pages.insert(page, Held { at, copy, taken: 0, looked: u64::MAX, born: self.frame, owners: 0, theirs: false, fault: None });
        self.order.push(page);
        fill(self.entry(page), copy, at as u64 - self.copies[copy].span().0);
    }

    /// `page`'s place in its arena is given back: shaders find no copy of it
    /// from the next submission on (lists already sent read what the place
    /// holds, as when a packed structure goes).
    fn let_go(&mut self, page: u64) {
        if let Some(held) = self.pages.remove(&page) {
            self.free.push((held.copy, held.at));
            // SAFETY: an entry of the directory, under the mutex.
            unsafe { self.entry(page).write_volatile(0) };
        }
    }

    /// The packed structure at `address` is gone: the pages it alone had are
    /// given back.
    fn gone(&mut self, address: u64) {
        let Some(packed) = self.packed.remove(&address) else { return };
        for page in address / PAGE..=(address + packed.len - 1) / PAGE {
            let Some(held) = self.pages.get_mut(&page) else { continue };
            held.owners = held.owners.saturating_sub(1);
            if held.owners == 0 && held.theirs {
                self.let_go(page);
            }
        }
        self.stats.gone += 1;
    }

    /// Whether the packed structure at `address` is still in title memory.
    fn there(&self, address: u64) -> bool {
        let mut header = [0u8; 8];
        crate::texture::committed(address as *const u8, 8) && {
            crate::host::copy_mapped(header.as_mut_ptr(), address as *const u8, 8, false);
            self.packed.get(&address).is_some_and(|p| p.header == u64::from_le_bytes(header))
        }
    }

    /// Looks at some of the pages and packed structures again.
    fn look_again(&mut self) {
        for _ in 0..self.order.len().div_ceil(SWEEP) {
            self.sweep = (self.sweep + 1) % self.order.len();
            let page = self.order[self.sweep];
            if self.pages.contains_key(&page) {
                self.bring(page, page + 1);
            } else {
                self.order.swap_remove(self.sweep);
                if self.order.is_empty() {
                    break;
                }
            }
        }
        for _ in 0..self.packed_order.len().div_ceil(SWEEP) {
            self.packed_sweep = (self.packed_sweep + 1) % self.packed_order.len();
            let address = self.packed_order[self.packed_sweep];
            if !self.packed.contains_key(&address) {
                self.packed_order.swap_remove(self.packed_sweep);
                if self.packed_order.is_empty() {
                    break;
                }
            } else if !self.there(address) {
                self.gone(address);
            }
        }
    }

    /// A dispatch of the builder was given `address` to write: if it is in a
    /// title buffer's copy, stores by address reach that from here on.
    fn opens(&mut self, address: u64) {
        let Some(n) = self.map(address / PAGE).then(|| self.copy_of(address / PAGE)).flatten() else {
            return;
        };
        if self.pages.contains_key(&(address / PAGE)) || !self.open.insert(n) {
            return;
        }
        // (Its pages hold what the CPU writes beside the builder's scratch.)
        self.copies[n].keep_stores();
        let (first, len) = self.copies[n].span();
        tracing::info!(
            start = format_args!("{first:#x}"),
            len,
            "title memory by address: the structure builder may store in a title buffer"
        );
        for page in first.div_ceil(PAGE)..(first + len) / PAGE {
            if self.copy_of(page) == Some(n) {
                fill(self.entry(page), n, page * PAGE - first);
            }
        }
    }

    /// [`Memory::map`] of the pages of `[address, address + len)`, up to the
    /// first that is not the title's, and what the title wrote there since
    /// they were taken goes into the arenas.
    fn ready(&mut self, address: u64, len: u64) {
        let first = address / PAGE;
        let mut end = first;
        while end * PAGE < address + len && self.map(end) {
            end += 1;
        }
        self.bring(first, end);
    }

    /// Takes the pages `[first, end)` that are in arenas from title memory
    /// where it wrote them.
    fn bring(&mut self, first: u64, end: u64) {
        // Once a frame: the builder names the same pages list after list.
        let frame = self.frame;
        if !(first..end).any(|page| self.pages.get(&page).is_some_and(|h| h.looked != frame)) {
            return;
        }
        let started = std::time::Instant::now();
        let answer = crate::watch::writes((first * PAGE) as usize, (end * PAGE) as usize, true);
        for page in first..end {
            let Some(held) = self.pages.get_mut(&page) else {
                continue;
            };
            if held.looked == frame {
                continue;
            }
            held.looked = frame;
            self.stats.looked += 1;
            let wrote = answer
                .as_ref()
                .map(|(_, w)| w.binary_search_by_key(&(page as usize), |w| w.0).map_or(0, |i| w[i].1));
            let now = answer.as_ref().map_or(1, |a| a.0);
            let stale = held.taken == 0 || wrote.is_none_or(|w| w > held.taken);
            if !stale {
                continue;
            }
            let never = std::mem::replace(&mut held.taken, now.max(1)) == 0;
            let shadow = &self.copies[held.copy];
            let base = held.at - shadow.span().0 as usize;
            let mut title = [0u8; PIECE];
            for piece in (0..PAGE as usize).step_by(PIECE) {
                let from = (page * PAGE) as usize + piece;
                // (Pages that are not there read as zeros.)
                crate::host::copy_mapped(title.as_mut_ptr(), from as *const u8, PIECE, false);
                // SAFETY: a page of an arena of ours, under the mutex.
                let ours = unsafe { std::slice::from_raw_parts_mut((held.at + piece) as *mut u8, PIECE) };
                self.stats.changed += merge(&title, ours, never) as u64 * 4;
            }
            if never {
                shadow.resend(base, base + PAGE as usize);
            }
        }
        self.stats.micros += started.elapsed().as_micros() as u64;
    }

    /// The bytes of a packed structure title memory has at `address` by now,
    /// and its first two dwords.
    fn packed_at(&self, address: u64) -> Option<(u64, u64)> {
        if address & 0xFF != 0 {
            return None;
        }
        let mut header = [0u8; 8];
        crate::host::copy_mapped(header.as_mut_ptr(), address as *const u8, 8, false);
        let header = u64::from_le_bytes(header);
        Some((packed_len(header)?, header))
    }

    /// At the submission of a list whose builder dispatch was given `address`
    /// outside buffers, in the order of its copies: what title memory has
    /// there now goes up. When the dispatch is recorded it may not be there
    /// yet: the title's queue waits for its streaming. (Nothing is asked of
    /// the write watch here: the submission's asking stands.)
    fn land(&mut self, address: u64) {
        let page = address / PAGE;
        if !crate::texture::committed((page * PAGE) as *const u8, PAGE as usize) {
            return;
        }
        let Some((len, header)) = self.packed_at(address) else {
            // Anything else: what came since its page was first taken.
            for page in page..=(address + VIEW - 1) / PAGE {
                if self.pages.get(&page).is_some_and(|h| h.born + YOUNG >= self.frame) {
                    self.take(page, 0, PAGE as usize, false);
                }
            }
            return;
        };
        if let Some(known) = self.packed.get_mut(&address) {
            let soon = known.frame + AGAIN >= self.frame;
            known.frame = self.frame;
            if known.header == header && soon {
                return;
            }
            if known.header != header {
                self.gone(address);
            }
        }
        let last = (address + len - 1) / PAGE;
        if !self.packed.contains_key(&address) {
            self.packed.insert(address, Packed { frame: self.frame, len, header });
            self.packed_order.push(address);
            for page in page..=last {
                let new = self.copy_of(page).is_none();
                if new && !self.place(page) {
                    continue;
                }
                if let Some(held) = self.pages.get_mut(&page) {
                    held.owners += 1;
                    held.theirs |= new;
                    if held.fault.take().is_some() {
                        self.faulted.named();
                    }
                }
            }
            self.stats.packed_bytes += len;
        }
        for page in page..=last {
            let from = address.max(page * PAGE) - page * PAGE;
            let to = (address + len).min((page + 1) * PAGE) - page * PAGE;
            self.take(page, from as usize, to as usize, true);
        }
        self.stats.packed += 1;
    }

    /// A place in an arena for a page of committed title memory, at a
    /// submission: only one there is already (the list brings up the arenas
    /// it knew when it was recorded).
    fn place(&mut self, page: u64) -> bool {
        if page < 0x1_0000
            || page >= u64::from(MEMORY_PAGES) - 1
            || !crate::texture::committed((page * PAGE) as *const u8, PAGE as usize)
            || crate::shadow::in_buffer(page * PAGE, PAGE).is_some()
        {
            return false;
        }
        let Some((copy, at)) = self.free.pop() else {
            return false;
        };
        self.hold(page, at, copy);
        crate::watch::wrote(self.entry(page) as usize, 8);
        self.take(page, 0, PAGE as usize, true);
        true
    }

    /// Bytes `from..to` of `page` from title memory into its arena's page, at
    /// a submission; `whole`: sent whether they differ from those last taken
    /// or not.
    fn take(&mut self, page: u64, from: usize, to: usize, whole: bool) {
        let Some(held) = self.pages.get(&page) else { return };
        let shadow = &self.copies[held.copy];
        let base = held.at - shadow.span().0 as usize;
        let mut title = [0u8; PIECE];
        let mut at = from;
        while at < to {
            let n = (PIECE - at % PIECE).min(to - at);
            crate::host::copy_mapped(title.as_mut_ptr(), (page as usize * PAGE as usize + at) as *const u8, n, false);
            // SAFETY: a page of an arena of ours, under the mutex.
            let ours = unsafe { std::slice::from_raw_parts_mut((held.at + at) as *mut u8, n) };
            if *ours != title[..n] {
                ours.copy_from_slice(&title[..n]);
                crate::watch::wrote(held.at + at, n);
            }
            at += n;
        }
        if whole {
            shadow.resend(base + from, base + to);
        }
    }

    /// The pages shaders found empty since the last frame.
    fn take_faults(&mut self) {
        let mut pages = Vec::new();
        for i in 1..MEMORY_FAULTS as usize {
            // SAFETY: inside the faults' mapping; the GPU may be writing,
            // a number missed now is written again next frame.
            let at = unsafe { self.faults.1.add(i) };
            // SAFETY: as above.
            let page = unsafe { at.read_volatile() };
            if page != 0 {
                pages.push(u64::from(page));
                // SAFETY: as above.
                unsafe { at.write_volatile(0) };
            }
        }
        pages.sort_unstable();
        let limits = fault_limits();
        // (Out for the time: it asks this memory for pages.)
        let mut faulted = std::mem::take(&mut self.faulted);
        let t = faulted.take(&pages, self.frame, limits, self);
        let starving = faulted.watch(&t, std::time::Instant::now());
        let from_faults = faulted.held;
        self.faulted = faulted;
        if let Some(since) = starving {
            tracing::warn!(
                "title memory by address: for {:.0} s every frame's shaders touched title pages that could not all be given a copy \
                 (those reads give 0 and those stores are dropped until they have one); in the last frame {} past the limit of {} a frame, \
                 {} at the limit of {} held, {} for room on the card; binding: {}",
                since.as_secs_f64(),
                t.capped,
                limits.per_frame,
                t.held_limit,
                limits.most,
                t.no_room,
                t.binding()
            );
        }
        if pages.is_empty() {
            return;
        }
        self.stats.faults += pages.len() as u64;
        // Each is an access that read 0 or was dropped.
        static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let said = SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if said < 40 || said.is_multiple_of(100) {
            let some: Vec<String> = pages
                .iter()
                .step_by(pages.len().div_ceil(12))
                .map(|&p| format!("{:#x}", p * PAGE))
                .collect();
            tracing::warn!(
                frame = self.frame,
                pages = pages.len(),
                made = t.made,
                there = t.there,
                wild = t.wild,
                not_title = t.not_title,
                capped = t.capped,
                held_limit = t.held_limit,
                no_room = t.no_room,
                failed = t.failed,
                given_back = t.given_back,
                held = self.pages.len(),
                from_faults,
                ?some,
                "title memory by address: shaders touched pages that had no copy (those accesses read 0 or were dropped); \
                 `made` of them have one now, the others not the title's (`wild`, `not_title`) or tried again when touched again"
            );
        }
    }

    /// The structures named in a list's upload memory around `address`
    /// (instances of a top-level build: a structure's address is their last
    /// 8 bytes): the first page of each.
    fn named_in_upload(&mut self, address: u64) {
        let Some((from, len)) = crate::upload::block(address as usize) else {
            return;
        };
        // SAFETY: the block is handed out and stays mapped; the title wrote
        // it before it recorded the dispatch.
        let words = unsafe { std::slice::from_raw_parts(from as *const u64, len / 8) };
        let mut seen = std::collections::HashSet::new();
        for &word in words {
            if word & 0xFF == 0 && (1 << 32..1 << 37).contains(&word) && seen.insert(word / PAGE) && self.map(word / PAGE) {
                let page = word / PAGE;
                self.bring(page, page + 1);
            }
        }
    }

    /// A small title buffer whose results come back, if its pages are
    /// windows of title memory itself (or a copy of the buffer's own).
    fn small(&mut self, list: &HostList, start: u64, len: u64) {
        let mut at = start;
        while at < start + len {
            let page = at / PAGE;
            let n = ((page + 1) * PAGE).min(start + len) - at;
            self.map(page);
            if let Some(k) = self.copy_of(page)
                && !self.pages.contains_key(&page)
            {
                self.copies[k].refresh(list, at, n);
                self.copies[k].gpu_writes(list, at, n);
                self.stats.back += 1;
            }
            at += n;
        }
    }

    /// Every copy is brought up to date when `list` is submitted, after what
    /// it deferred so far (once until a copy of the title's intervenes).
    fn bring_up(&self, list: &HostList) {
        let (start, len) = self.directory.1.span();
        self.directory.1.refresh(list, start, len);
        for shadow in &self.copies {
            let (start, len) = shadow.span();
            shadow.refresh(list, start, len);
        }
    }

    /// Every ten seconds or so, what was used.
    fn report(&mut self) {
        let now = std::time::Instant::now();
        let (since, frame) = *self.stats.from.get_or_insert((now, self.frame));
        if now.duration_since(since).as_secs_f64() < 10.0 {
            return;
        }
        let frames = self.frame.wrapping_sub(frame).max(1) as f64;
        let s = std::mem::take(&mut self.stats);
        tracing::info!(
            "title memory by address: {} pages ({} MB) in {} arenas, {} copies in all; per frame {:.0} dispatches that read, {:.0} that build, \
             {:.1} pages that had no copy, {:.1} pages turned down, {:.1} results brought back; {:.0} pages looked at, {:.1} KB changed, \
             {:.1} packed structures ({} there), {:.2} ms; {} windows; since the last line {} KB of packed structures came and {} of them went, \
             and pages for constants {}, views written {}, others {}, upload memory {}, made from faults in all {}",
            self.pages.len(),
            (self.pages.len() as u64 * PAGE) >> 20,
            self.arenas.len(),
            self.copies.len(),
            s.uses[0] as f64 / frames,
            s.uses[1] as f64 / frames,
            s.faults as f64 / frames,
            s.unknown as f64 / frames,
            s.back as f64 / frames,
            s.looked as f64 / frames,
            s.changed as f64 / 1024.0 / frames,
            s.packed as f64 / frames,
            self.packed.len(),
            s.micros as f64 / 1000.0 / frames,
            self.windows,
            s.packed_bytes >> 10,
            s.gone,
            s.came[0],
            s.came[1],
            s.came[2],
            s.came[3],
            self.faulted.all
        );
        let f = std::mem::take(&mut self.faulted.tally);
        if f.asked > 0 {
            let limits = fault_limits();
            tracing::info!(
                "title memory by address: pages for faults since the last line, over {} frames: {} asked, {} made, {} given back \
                 (made longest ago first); not made: {} past the limit of {} a frame, {} at the limit of {} held, {} for room on the card, \
                 {} failed, {} not the title's memory, {} outside it (wild); {} already had one; {} held for faults now; \
                 {} frames short; binding: {}",
                f.frames,
                f.asked,
                f.made,
                f.given_back,
                f.capped,
                limits.per_frame,
                f.held_limit,
                limits.most,
                f.no_room,
                f.failed,
                f.not_title,
                f.wild,
                f.there,
                self.faulted.held,
                f.starved,
                f.binding()
            );
        }
        self.stats.from = Some((now, self.frame));
    }
}

/// The limits of [`FaultPages`].
#[derive(Clone, Copy)]
struct FaultLimits {
    /// Pages made a frame, title pages tried a frame.
    per_frame: usize,
    tries: usize,
    /// Pages held for faults at a time.
    most: usize,
    /// Frames a page is held at least before it can be given back.
    fresh: u64,
}

/// [`FaultLimits`] of this run: [`FAULTS`] and [`FAULT_TRIES`] a frame, and
/// `VICTORY_MEMORY_FAULT_MB` of pages held at a time (unset: an eighth of
/// `VICTORY_MEMORY_CARD_MB`, 512 MiB, 8,192 pages, by default; at least
/// one arena's, 64 MiB, and at most what leaves [`SPARE`] of [`MOST`]).
fn fault_limits() -> FaultLimits {
    static MOST_HELD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let most = *MOST_HELD.get_or_init(|| {
        let mb = std::env::var("VICTORY_MEMORY_FAULT_MB").ok().and_then(|v| v.trim().parse::<u64>().ok());
        let mb = mb.unwrap_or((crate::shadow::by_address_card_most() >> 20) / 8);
        ((mb << 20) / PAGE).clamp(ARENA / PAGE, (MOST - SPARE) as u64) as usize
    });
    FaultLimits { per_frame: FAULTS, tries: FAULT_TRIES, most, fresh: FAULT_FRESH }
}

/// What became of a frame's faults (or of the frames since the last report):
/// `asked` is `there + made + wild + not_title + capped + held_limit +
/// no_room + failed`.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct FaultTally {
    frames: u64,
    /// Pages shaders found without a copy.
    asked: u64,
    /// Had one by the time they were looked at.
    there: u64,
    made: u64,
    /// Outside title memory's range (below 4 GiB, past the directory): no
    /// pointer of the title's, nothing spent on them.
    wild: u64,
    /// In the range but not the title's memory (not committed, not watched).
    not_title: u64,
    /// Past the limit of a frame (made or tried): tried again next frame.
    capped: u64,
    /// At the limit of pages held, with none old enough to give back.
    held_limit: u64,
    /// No free place, the card without room for another arena and none to
    /// give back.
    no_room: u64,
    /// A copy that could not be made (memory of the PC, numbers).
    failed: u64,
    /// Pages for faults given back for new ones.
    given_back: u64,
    /// Frames in which title pages were left without a copy for a limit.
    starved: u64,
}

impl FaultTally {
    fn add(&mut self, t: &FaultTally) {
        self.frames += t.frames;
        self.asked += t.asked;
        self.there += t.there;
        self.made += t.made;
        self.wild += t.wild;
        self.not_title += t.not_title;
        self.capped += t.capped;
        self.held_limit += t.held_limit;
        self.no_room += t.no_room;
        self.failed += t.failed;
        self.given_back += t.given_back;
        self.starved += t.starved;
    }

    /// Whether title pages were left without a copy for a limit.
    fn short(&self) -> bool {
        self.capped + self.held_limit + self.no_room > 0
    }

    /// The limit that left most pages without a copy.
    fn binding(&self) -> &'static str {
        let most = self.capped.max(self.held_limit).max(self.no_room);
        if most == 0 {
            "none"
        } else if most == self.capped {
            "the limit of a frame"
        } else if most == self.held_limit {
            "the limit of pages held"
        } else {
            "room on the card"
        }
    }
}

/// What [`Keeper::grow`] did.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Grown {
    Yes,
    /// The card has no room for another arena (or the cap is reached).
    NoRoom,
    /// All the pages there may be are held or free ([`MOST`]).
    Full,
    Failed,
}

/// What [`FaultPages::take`] asks of the memory that holds the pages
/// ([`Memory`]; a plain one in the tests).
trait Keeper {
    /// Whether `page` has a copy.
    fn has(&self, page: u64) -> bool;
    /// Whether `page` is the title's memory.
    fn title(&mut self, page: u64) -> bool;
    /// A copy of `page` that is not an arena's page (a title buffer's, a
    /// window): whether it was made; None if it needs an arena's.
    fn other_copy(&mut self, page: u64) -> Option<bool>;
    /// Whether as many pages are held as there may be.
    fn full(&self) -> bool;
    /// Whether a free place is left beyond those kept for the builder.
    fn spare(&self) -> bool;
    /// Another arena's free places, only on the card.
    fn grow(&mut self) -> Grown;
    /// `page` in a free place, numbered `number` among the pages for faults.
    fn make(&mut self, page: u64, number: u64) -> bool;
    /// The number of `page` if it is held for faults.
    fn number(&self, page: u64) -> Option<u64>;
    fn give_back(&mut self, page: u64);
}

/// Pages given a copy because shaders found them without one (the faults).
/// At most [`FaultLimits::per_frame`] are made a frame, and at most
/// [`FaultLimits::most`] are held: past that, or when a new one would need
/// another arena and the card has no room for one, the page made longest
/// ago is given back for it (none held fewer than [`FaultLimits::fresh`]
/// frames). Shaders do not say which pages that have a copy they touch, so
/// "longest ago" is the order they were made in: a page given back that
/// shaders still use faults again and comes back. A page named since (by a
/// view, a constant, a packed structure) is held for that and no longer
/// counted here.
#[derive(Default)]
struct FaultPages {
    /// In the order they were made: the page, its number, the frame.
    made: std::collections::VecDeque<(u64, u64, u64)>,
    /// How many of them are held for faults now.
    held: usize,
    /// The last number given.
    number: u64,
    /// Where the next frame's tries start among its faults.
    turn: usize,
    /// Made in all.
    all: u64,
    /// Since the last report.
    tally: FaultTally,
    /// Since when every frame was short ([`FaultTally::short`]), and
    /// whether it was said.
    starving: Option<std::time::Instant>,
    warned: bool,
}

impl FaultPages {
    /// A page held for faults was named.
    fn named(&mut self) {
        self.held = self.held.saturating_sub(1);
    }

    /// Gives back the page made longest ago if it was held long enough.
    fn give_back_oldest(&mut self, frame: u64, fresh: u64, keeper: &mut impl Keeper) -> bool {
        while let Some(&(page, number, made)) = self.made.front() {
            if keeper.number(page) != Some(number) {
                self.made.pop_front();
                continue;
            }
            if made + fresh > frame {
                return false;
            }
            self.made.pop_front();
            keeper.give_back(page);
            self.held = self.held.saturating_sub(1);
            return true;
        }
        false
    }

    /// The faults of frame `frame` (sorted): pages made for those that are
    /// the title's, within the limits.
    fn take(&mut self, pages: &[u64], frame: u64, limits: FaultLimits, keeper: &mut impl Keeper) -> FaultTally {
        let mut t = FaultTally { frames: 1, asked: pages.len() as u64, ..FaultTally::default() };
        // Entries of pages given back or named since.
        if self.made.len() > 2 * self.held + 4096 {
            self.made.retain(|&(page, number, _)| keeper.number(page) == Some(number));
        }
        let start = self.turn % pages.len().max(1);
        let mut tried = 0;
        // Once an arena could not be made, not asked again this frame.
        let mut not_grown = None;
        for k in 0..pages.len() {
            let page = pages[(start + k) % pages.len()];
            if !(LOWEST..u64::from(MEMORY_PAGES) - 1).contains(&page) {
                t.wild += 1;
                continue;
            }
            if keeper.has(page) {
                t.there += 1;
                continue;
            }
            if t.made as usize >= limits.per_frame || tried >= limits.tries {
                t.capped += 1;
                continue;
            }
            tried += 1;
            if !keeper.title(page) {
                t.not_title += 1;
                continue;
            }
            match keeper.other_copy(page) {
                Some(true) => {
                    t.made += 1;
                    continue;
                }
                Some(false) => {
                    t.failed += 1;
                    continue;
                }
                None => {}
            }
            let mut freed = false;
            if self.held >= limits.most || keeper.full() {
                if !self.give_back_oldest(frame, limits.fresh, keeper) {
                    t.held_limit += 1;
                    continue;
                }
                t.given_back += 1;
                freed = true;
            }
            if !freed && !keeper.spare() {
                let grown = not_grown.unwrap_or_else(|| keeper.grow());
                if grown != Grown::Yes {
                    not_grown = Some(grown);
                    if self.give_back_oldest(frame, limits.fresh, keeper) {
                        t.given_back += 1;
                    } else {
                        match grown {
                            Grown::NoRoom => t.no_room += 1,
                            Grown::Full => t.held_limit += 1,
                            _ => t.failed += 1,
                        }
                        continue;
                    }
                }
            }
            self.number += 1;
            if keeper.make(page, self.number) {
                self.made.push_back((page, self.number, frame));
                self.held += 1;
                self.all += 1;
                t.made += 1;
            } else {
                t.failed += 1;
            }
        }
        // Next frame the tries start further on: pages left out now go first.
        self.turn = self.turn.wrapping_add(tried);
        t.starved = u64::from(t.short());
        self.tally.add(&t);
        t
    }

    /// After a frame's [`FaultPages::take`]: how long every frame has been
    /// short, once it has been for [`STARVING`] (once until a frame is not).
    fn watch(&mut self, t: &FaultTally, now: std::time::Instant) -> Option<std::time::Duration> {
        if !t.short() {
            self.starving = None;
            self.warned = false;
            return None;
        }
        let since = now.duration_since(*self.starving.get_or_insert(now));
        if self.warned || since < STARVING {
            return None;
        }
        self.warned = true;
        Some(since)
    }
}

impl Keeper for Memory {
    fn has(&self, page: u64) -> bool {
        self.copy_of(page).is_some()
    }

    fn title(&mut self, page: u64) -> bool {
        self.possible(page)
    }

    fn other_copy(&mut self, page: u64) -> Option<bool> {
        self.in_buffers(page)
    }

    fn full(&self) -> bool {
        self.pages.len() >= MOST
    }

    fn spare(&self) -> bool {
        self.free.len() > SPARE
    }

    fn grow(&mut self) -> Grown {
        if self.pages.len() + self.free.len() + (ARENA / PAGE) as usize > MOST {
            Grown::Full
        } else if !crate::shadow::by_address_on_card(ARENA) {
            Grown::NoRoom
        } else if self.another() {
            Grown::Yes
        } else {
            Grown::Failed
        }
    }

    fn make(&mut self, page: u64, number: u64) -> bool {
        let Some((copy, at)) = self.free.pop() else {
            return false;
        };
        self.hold(page, at, copy);
        if let Some(held) = self.pages.get_mut(&page) {
            held.fault = Some(number);
        }
        self.bring(page, page + 1);
        true
    }

    fn number(&self, page: u64) -> Option<u64> {
        self.pages.get(&page).and_then(|h| h.fault)
    }

    fn give_back(&mut self, page: u64) {
        self.let_go(page);
    }
}

/// A list with this key ([`HostList::used`]) asks the builder for sizes.
const SIZES: usize = usize::MAX - (9 << 33);

/// Whether a submission that asks for the sizes of structures is waited for,
/// and landed, before the title goes on (`VICTORY_BVH_SIZES_WAIT=1`; off).
///
/// Off: it does not help. In run light-15, with it, 33 of 154 copies had the
/// slot's old size still (all of them recorded one frame after the size was
/// asked for) against 53 of 230 in light-11, 48 of 275 dynamic structures
/// were good, and the title asks in every frame, so every frame waits (109 ms
/// against 90). The title reads the slot before our submission of the asking
/// list is through, or reads it for another purpose; not known.
///
/// The title asks for a new structure's compacted size (a program of one
/// thread stores it into a small buffer), reads it a frame or two later
/// without looking whether it is there, takes memory of that size and copies
/// the structure into it. With submissions that land up to two frames late it
/// read what the slot held before, some other structure's size, and the copy
/// wrote over its neighbours: of 230 such copies of run light-11, 99 were
/// asked for two frames earlier and landed in that frame, none of those with
/// the right size.
fn sizes_wait() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_SIZES_WAIT").is_some_and(|v| v == "1"))
}

/// Whether a dispatch of the builder is its program of a structure's size:
/// one thread, the structure's address and what to store (1: the compacted
/// size) as constants, a small title buffer to store into.
fn asks_size(written: &[u64], constants: &[u32], threads: Option<[u32; 3]>) -> bool {
    threads == Some([1, 1, 1])
        && matches!(*constants, [_, hi, 1..=3, ..] if (1..0x20).contains(&hi))
        && written.first().is_some_and(|&at| crate::shadow::in_buffer(at, 1).is_some_and(|b| b.1 <= BACK))
}

/// Whether one of `lists` asks for sizes.
pub(crate) fn asked_sizes<'a>(lists: impl Iterator<Item = &'a HostList>) -> bool {
    let mut lists = lists;
    let asked = sizes_wait() && lists.any(|l| l.used(SIZES));
    if asked {
        static TIMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = TIMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n.is_power_of_two() || n.is_multiple_of(100) {
            tracing::info!(n, frame = crate::frame::current(), "submissions that ask for structures' sizes, waited for");
        }
    }
    asked
}

/// [`fill`] of a page stores by address do not reach.
fn close(entry: *mut u32, n: usize, offset: u64) {
    fill(entry, n | MEMORY_READ_ONLY as usize, offset);
}

/// The offset first: a shader that sees the number sees it too.
fn fill(entry: *mut u32, n: usize, offset: u64) {
    // SAFETY: an entry of the directory, under the mutex.
    unsafe {
        entry.add(1).write_volatile(offset as u32);
        entry.write_volatile(n as u32 + 1);
    }
}

/// The bytes of a packed structure whose first two dwords these are: the
/// version (6) with the marks of a bottom level and of one that is packed,
/// and its size unpacked in 64-byte nodes.
fn packed_len(header: u64) -> Option<u64> {
    let (version, nodes) = (header as u32, header >> 32);
    let marks = version >> 16;
    // The header counts its nodes of 64 bytes without the last one: the
    // builder's own program of a structure's size stores one more.
    let len = (nodes + 1) * 64;
    (version & 0xFFFF == 6 && marks & 0x8100 == 0x8100 && nodes > 0 && len <= PACKED).then_some(len)
}

/// Puts what `title` holds into `ours` (what was last taken): the dwords that
/// differ. How many.
fn merge(title: &[u8; PIECE], ours: &mut [u8], never: bool) -> usize {
    if never {
        ours.copy_from_slice(title);
        return PIECE / 4;
    }
    if ours == title {
        return 0;
    }
    let mut changed = 0;
    for (t, o) in title.chunks_exact(4).zip(ours.chunks_exact_mut(4)) {
        if t != o {
            o.copy_from_slice(t);
            changed += 1;
        }
    }
    changed
}

/// Before a draw whose shader reads title memory by address: what its
/// root arguments name is made ready (`addresses`: its root views, of them
/// `written` those it writes; `constants`: its root constants, see the
/// module), every numbered copy
/// has its view in `heap`'s run and is brought up to date when `list` is
/// submitted. None if the host cannot hold it; the draw is then dropped.
pub(crate) fn ready(
    list: &HostList,
    heap: &HostHeap,
    addresses: &[u64],
    written: &[u64],
    constants: &[u32],
    use_: Use,
    threads: Option<[u32; 3]>,
) -> Option<Ready> {
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Memory::new();
    }
    let m = guard.as_mut()?;
    let frame = crate::frame::current();
    if m.frame != frame {
        m.frame = frame;
        m.refused.clear();
        m.take_faults();
        m.look_again();
        m.report();
        #[cfg(feature = "diagnostics")]
        m.trace.look(frame);
    }
    #[cfg(feature = "diagnostics")]
    if use_ != Use::Reads {
        m.trace.dispatch(frame, list, addresses, written, constants, threads);
    }
    let builds = use_ != Use::Reads;
    if builds && sizes_wait() && asks_size(written, constants, threads) {
        list.first_use(SIZES);
    }
    while builds && m.free.len() < SPARE && m.pages.len() + m.free.len() < MOST && m.another() {}
    m.stats.uses[usize::from(builds)] += 1;
    // Once per list and page named: the builder dispatches hundreds of times
    // a frame with the same arguments.
    const READ: usize = usize::MAX - 0x6d;
    let named = |kind: usize, at: u64| list.first_use(usize::MAX - (kind << 26) - (at / PAGE) as usize);
    // A title address in two constants (low dword, then bits 32 to 39), or
    // in one in 256-byte units (the builder's alone). A constant that only
    // looks like one names memory nothing reads; one that is not the title's
    // memory is refused.
    let held = |m: &Memory| m.pages.len() as u64;
    let before = held(m);
    for (i, &c) in constants.iter().enumerate() {
        if let Some(&high) = constants.get(i + 1)
            && (1..=0x1F).contains(&high)
            && c >= 0x1_0000
        {
            let at = u64::from(c) | u64::from(high) << 32;
            if named(1, at) {
                m.ready(at, 1);
            }
        }
        if builds && (0x0100_0000..0x2000_0000).contains(&c) {
            let at = u64::from(c) << 8;
            if named(1, at) {
                m.ready(at, 1);
            }
        }
    }
    let mut came = [held(m) - before, 0, 0, 0];
    for &at in addresses.iter().filter(|&&a| a >= 1 << 32) {
        // (Several structures to a page.)
        let mut packed = false;
        if builds
            && list.first_use(usize::MAX - (3 << 33) - (at >> 8) as usize)
            && crate::shadow::in_buffer(at, 1).is_none()
            && m.possible(at / PAGE)
        {
            // What it needs, as far as it is known now; the rest when the
            // list is submitted ([`Memory::land`]).
            if let Some((len, _)) = m.packed_at(at) {
                packed = true;
                for page in at / PAGE..=(at + len - 1) / PAGE {
                    let new = m.copy_of(page).is_none();
                    if m.map(page)
                        && new
                        && let Some(held) = m.pages.get_mut(&page)
                    {
                        held.theirs = true;
                    }
                }
                m.bring(at / PAGE, (at + len - 1) / PAGE + 1);
            }
            list.defer_host(move || {
                if let Some(m) = MEMORY.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    m.land(at);
                }
            });
            m.bring_up(list);
        }
        if builds && written.contains(&at) && list.first_use(usize::MAX - (4 << 33) - (at >> 2 & 0x7FFF_FFFF) as usize) {
            if let Some((start, len)) = crate::shadow::in_buffer(at, 1)
                && len <= BACK
                && use_ == (Use::Builds { back: true })
                && list.first_use(usize::MAX - (5 << 33) - (start >> 2 & 0x7FFF_FFFF) as usize)
            {
                m.small(list, start, len);
            }
            m.opens(at);
            if !packed && crate::shadow::in_buffer(at, 1).is_none() {
                #[cfg(feature = "diagnostics")]
                {
                    let built = m.dump.built.entry(at).or_insert((frame, frame, 0));
                    (built.1, built.2) = (frame, built.2 + 1);
                }
                let before = held(m);
                m.ready(at, BUILT);
                came[1] += held(m).saturating_sub(before);
            }
        }
        if packed || !named(2, at) {
            continue;
        }
        let before = held(m);
        match crate::shadow::in_buffer(at, 1) {
            Some(_) => {
                m.ready(at, 1);
                m.named_in_upload(at);
                came[3] += held(m).saturating_sub(before);
            }
            None => {
                m.ready(at, if builds { VIEW } else { PAGE });
                came[2] += held(m).saturating_sub(before);
            }
        }
    }
    for (sum, n) in m.stats.came.iter_mut().zip(came) {
        *sum += n;
    }
    let key = std::ptr::from_ref(heap) as usize;
    let (base, made) = match m.heaps.get(&key) {
        Some(&run) => run,
        None => {
            // Held for the heap's life: not given back with a list's slots.
            let base = heap.dynamic(COPIES + 1)?;
            // Every slot of the run: a number shows no view of another kind.
            for n in 0..=COPIES {
                heap.null_raw_uav_at(base + n);
            }
            (base, 0)
        }
    };
    for (n, shadow) in m.copies.iter().enumerate().skip(made as usize) {
        let (_, len) = shadow.span();
        heap.raw_uav_at(base + 1 + n as u32, &shadow.buffer, 0, len);
    }
    m.heaps.insert(key, (base, m.copies.len() as u32));
    // Every copy by every list that has come to by-address memory, whoever
    // numbered it: an arena another list made while this one was being
    // recorded was brought up to date by that list's submission alone, which
    // may run after this one's (structures unpacked from zeros, and then
    // written over by the arena's first upload: in frames that stream much in
    // half of them, run light-8).
    let first_read = list.first_use(READ);
    if list.first_use(usize::MAX - (6 << 33) - m.copies.len()) {
        m.bring_up(list);
    }
    if first_read {
        list.keep(&m.faults.0);
        #[cfg(feature = "diagnostics")]
        m.dump(list, addresses);
    }
    #[cfg(feature = "diagnostics")]
    if use_ == Use::Reads {
        m.dump.walked(addresses);
    }
    let directory = &m.directory.1;
    Some(Ready {
        directory: directory.gpu(directory.span().0),
        faults: m.faults.2,
        base,
    })
}

/// `VICTORY_BVH_TRACE=1`: the sizes the builder is asked for (its program of
/// one thread that stores a structure's compacted size) and when title memory
/// has them, and the copies to a place of that size.
#[cfg(feature = "diagnostics")]
mod trace {
    use super::*;

    #[derive(Default)]
    pub(super) struct Trace {
        /// Frame, where the size goes, the structure, what title memory had.
        asked: Vec<(u64, u64, u64, u64)>,
        /// By structure, what its size was when last seen to change.
        sizes: HashMap<u64, (u64, u64)>,
        said: u32,
    }

    fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_TRACE").is_some_and(|v| v == "1"))
    }

    fn value(at: u64) -> u64 {
        let mut b = [0u8; 8];
        crate::host::copy_mapped(b.as_mut_ptr(), at as *const u8, 8, false);
        u64::from_le_bytes(b)
    }

    impl Trace {
        fn say(&mut self, text: String) {
            self.said += 1;
            if self.said < 6000 {
                tracing::info!("bvh trace: {text}");
            }
        }

        pub(super) fn look(&mut self, frame: u64) {
            if !on() {
                return;
            }
            let mut gone = Vec::new();
            for i in 0..self.asked.len() {
                let (then, at, structure, was) = self.asked[i];
                let now = value(at);
                if now != was {
                    self.sizes.insert(structure, (now, frame));
                    gone.push(i);
                    self.say(format!("frame {frame}: size of {structure:#x} at {at:#x} asked in {then}: {was:#x} -> {now:#x}"));
                } else if frame > then + 60 {
                    gone.push(i);
                    self.say(format!("frame {frame}: size of {structure:#x} at {at:#x} asked in {then}: still {was:#x}"));
                }
            }
            for i in gone.into_iter().rev() {
                self.asked.swap_remove(i);
            }
        }

        pub(super) fn dispatch(
            &mut self,
            frame: u64,
            list: &HostList,
            addresses: &[u64],
            written: &[u64],
            constants: &[u32],
            threads: Option<[u32; 3]>,
        ) {
            if !on() {
                return;
            }
            let _ = addresses;
            let list = std::ptr::from_ref(list) as usize & 0xFFFF_FFFF;
            if threads == Some([1, 1, 1])
                && let [lo, hi, mode, ..] = *constants
                && (1..=3).contains(&mode)
                && hi < 0x20
                && let Some(&at) = written.first()
                && crate::shadow::in_buffer(at, 1).is_some_and(|b| b.1 <= BACK)
            {
                let structure = u64::from(lo) | u64::from(hi) << 32;
                let was = value(at);
                self.say(format!("frame {frame} list {list:x}: asks mode {mode} of {structure:#x} to {at:#x}, there {was:#x}"));
                if mode == 1 {
                    self.asked.push((frame, at, structure, was));
                }
            } else if threads == Some([32, 1, 1])
                && let [from, to] = *written
                && [from, to].iter().all(|&a| a >= 1 << 32 && crate::shadow::in_buffer(a, 1).is_none())
            {
                let known = self.sizes.get(&from).copied();
                self.say(format!("frame {frame} list {list:x}: copies {from:#x} to {to:#x} constants {:x?}, its size was {known:x?}", &constants[..constants.len().min(4)]));
            }
        }
    }
}

/// `VICTORY_MEMORY_DUMP=<folder>`: when a file `go` appears there, the arenas
/// as the card holds them are copied out and written (`arena-<n>.bin`), with
/// `pages.txt` (title page, arena, offset) and the structures the walking
/// programs were given: to look at the structures offline.
#[cfg(feature = "diagnostics")]
mod dump {
    use super::*;

    #[derive(Default)]
    pub(super) struct Dump {
        walked: std::collections::BTreeSet<u64>,
        /// Views the builder was given to write outside buffers: the first
        /// and the last frame, how often.
        pub(super) built: std::collections::BTreeMap<u64, (u64, u64, u64)>,
        /// The frame it is written at, and the copies.
        due: Option<(u64, Vec<(ID3D12Resource, usize)>)>,
        done: bool,
    }

    impl Dump {
        pub(super) fn walked(&mut self, addresses: &[u64]) {
            if !self.done && self.walked.len() < 64 {
                self.walked.extend(addresses);
            }
        }
    }

    fn folder() -> Option<&'static std::path::PathBuf> {
        static FOLDER: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
        FOLDER.get_or_init(|| std::env::var_os("VICTORY_MEMORY_DUMP").map(Into::into)).as_ref()
    }

    fn turn(buffer: &ID3D12Resource, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    // SAFETY: the same pointer, not counted; the barrier is
                    // used while the buffer lives.
                    pResource: unsafe { std::mem::transmute_copy(buffer) },
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }

    impl Memory {
        /// At a list's first use: the copies are recorded into it.
        pub(super) fn dump(&mut self, list: &HostList, _: &[u64]) {
            let Some(folder) = folder() else { return };
            if let Some((due, _)) = &self.dump.due {
                if self.frame < *due {
                    return;
                }
                let copies = self.dump.due.take().map(|d| d.1).unwrap_or_default();
                let mut text = String::new();
                for a in &self.dump.walked {
                    text += &format!("walked {a:#x}\n");
                }
                for (a, (first, last, n)) in &self.dump.built {
                    text += &format!("built {a:#x} {first} {last} {n}\n");
                }
                for (a, p) in &self.packed {
                    text += &format!("packed {a:#x} {} {}\n", p.len, p.frame);
                }
                text += &format!("frame {}\n", self.frame);
                for (page, held) in &self.pages {
                    let arena = self.arenas.iter().position(|a| a.0 == held.copy).unwrap_or(usize::MAX);
                    let at = held.at - self.arenas[arena].1;
                    text += &format!("{:#x} {arena} {at:#x}\n", page * PAGE);
                }
                let _ = std::fs::write(folder.join("pages.txt"), text);
                for (n, (_, data)) in copies.iter().enumerate() {
                    // SAFETY: a mapping of an arena's bytes; the copies ran
                    // frames ago.
                    let bytes = unsafe { std::slice::from_raw_parts(*data as *const u8, ARENA as usize) };
                    let _ = std::fs::write(folder.join(format!("arena-{n}.bin")), bytes);
                }
                tracing::info!(arenas = copies.len(), "title memory by address: the arenas are written out");
                return;
            }
            if self.dump.done || !folder.join("go").exists() {
                return;
            }
            self.dump.done = true;
            let Some(host) = host() else { return };
            let mut copies = Vec::new();
            for &(n, _) in &self.arenas {
                let buffer = &self.copies[n].buffer;
                let Some((to, data)) = host.shared_buffer(ARENA) else { break };
                // SAFETY: an open list; live buffers of those sizes. The list
                // has not used the arena yet.
                unsafe {
                    list.list.ResourceBarrier(&[turn(buffer, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                    list.list.CopyBufferRegion(&to, 0, buffer, 0, ARENA);
                    list.list.ResourceBarrier(&[turn(buffer, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON)]);
                }
                list.keep(&to);
                copies.push((to, data as usize));
            }
            self.dump.due = Some((self.frame + 12, copies));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_takes_what_changed() {
        let mut title = [7u8; PIECE];
        let mut ours = vec![0u8; PIECE];
        assert_eq!(merge(&title, &mut ours, true), PIECE / 4);
        assert_eq!(ours, title);
        assert_eq!(merge(&title, &mut ours, false), 0);
        // A dword of it.
        title[41] = 8;
        assert_eq!(merge(&title, &mut ours, false), 1);
        assert_eq!(ours, title);
    }

    #[test]
    fn a_packed_structure_is_known_by_its_header() {
        let header = |version: u32, nodes: u32| u64::from(version) | u64::from(nodes) << 32;
        assert_eq!(packed_len(header(0x8102_0006, 0x9a1a)), Some(0x9a1b * 64));
        // Unpacked, a top level, another version, no size, too big.
        assert_eq!(packed_len(header(0x8002_0006, 0x9a1a)), None);
        assert_eq!(packed_len(header(0x0000_0006, 5)), None);
        assert_eq!(packed_len(header(0x8102_0007, 5)), None);
        assert_eq!(packed_len(header(0x8102_0006, 0)), None);
        assert_eq!(packed_len(header(0x8102_0006, 1 << 21)), None);
    }

    /// Memory of the test's own that holds pages for [`FaultPages`]: pages
    /// are the title's unless listed in `not_title`; places come by arenas of
    /// 1,024 while `room`.
    #[derive(Default)]
    struct Plain {
        held: HashMap<u64, Option<u64>>,
        not_title: std::collections::HashSet<u64>,
        /// Pages a title buffer's copy holds.
        buffers: std::collections::HashSet<u64>,
        free: usize,
        room: bool,
        most: usize,
        given_back: Vec<u64>,
        tried: Vec<u64>,
    }

    impl Keeper for Plain {
        fn has(&self, page: u64) -> bool {
            self.held.contains_key(&page)
        }
        fn title(&mut self, page: u64) -> bool {
            self.tried.push(page);
            !self.not_title.contains(&page)
        }
        fn other_copy(&mut self, page: u64) -> Option<bool> {
            self.buffers.contains(&page).then(|| {
                self.held.insert(page, None);
                true
            })
        }
        fn full(&self) -> bool {
            self.held.len() >= self.most
        }
        fn spare(&self) -> bool {
            self.free > 0
        }
        fn grow(&mut self) -> Grown {
            if !self.room {
                return Grown::NoRoom;
            }
            self.free += 1024;
            Grown::Yes
        }
        fn make(&mut self, page: u64, number: u64) -> bool {
            if self.free == 0 {
                return false;
            }
            self.free -= 1;
            self.held.insert(page, Some(number));
            true
        }
        fn number(&self, page: u64) -> Option<u64> {
            self.held.get(&page).copied().flatten()
        }
        fn give_back(&mut self, page: u64) {
            self.held.remove(&page);
            self.free += 1;
            self.given_back.push(page);
        }
    }

    fn plain() -> Plain {
        Plain { room: true, most: MOST, ..Plain::default() }
    }

    fn limits(per_frame: usize, most: usize) -> FaultLimits {
        FaultLimits { per_frame, tries: FAULT_TRIES, most, fresh: FAULT_FRESH }
    }

    /// Title pages from 4 GiB on.
    const T: u64 = LOWEST + 0x100;

    fn sums(t: &FaultTally) -> u64 {
        t.there + t.made + t.wild + t.not_title + t.capped + t.held_limit + t.no_room + t.failed
    }

    /// Far more than the 2,048 pages the run used to be given in all: a page
    /// touched after them still gets a copy, a frame's limit and the limit
    /// held hold throughout.
    #[test]
    fn faults_get_pages_for_the_whole_run() {
        let (mut f, mut m, l) = (FaultPages::default(), plain(), limits(128, 4096));
        for frame in 0..400u64 {
            // 300 pages a frame never touched before: 120,000 in all.
            let pages: Vec<u64> = (0..300).map(|i| T + frame * 300 + i).collect();
            let t = f.take(&pages, frame, l, &mut m);
            assert_eq!(t.made, 128, "frame {frame}");
            assert_eq!(t.capped, 300 - 128);
            assert_eq!(sums(&t), t.asked);
            assert!(f.held <= l.most && m.held.len() <= l.most);
        }
        assert_eq!(f.all, 400 * 128);
        assert_eq!(f.held, l.most);
        // Once full, a page for each new one, the oldest first.
        assert_eq!(m.given_back.len(), 400 * 128 - l.most);
        assert_eq!(m.given_back[0], T);
        // A page touched now is there from the next frame on.
        let late = T + 500_000;
        let t = f.take(&[late], 400, l, &mut m);
        assert_eq!((t.made, t.given_back), (1, 1));
        assert!(m.has(late));
        // A frame's faults left out by its limit come first in the next.
        let pages: Vec<u64> = (0..300).map(|i| T + 600_000 + i).collect();
        f.take(&pages, 401, l, &mut m);
        let t = f.take(&pages, 402, l, &mut m);
        assert_eq!((t.there, t.made), (128, 128));
        let t = f.take(&pages, 403, l, &mut m);
        assert_eq!((t.there, t.made), (256, 300 - 256));
        assert!(pages.iter().all(|&p| m.has(p)));
        assert_eq!(f.tally.frames, 404);
    }

    /// At the limit held, the page made longest ago goes first; none goes
    /// before it was held [`FAULT_FRESH`] frames, nor a page named since.
    #[test]
    fn the_oldest_pages_for_faults_go_first() {
        let (mut f, mut m, l) = (FaultPages::default(), plain(), limits(128, 3));
        for frame in 0..3 {
            f.take(&[T + frame], frame, l, &mut m);
        }
        // Too soon: nothing given back, the page is left for later.
        let t = f.take(&[T + 10], 3, l, &mut m);
        assert_eq!((t.made, t.held_limit, t.given_back), (0, 1, 0));
        assert_eq!(t.binding(), "the limit of pages held");
        // The first page is named by a list: held for that, not for faults.
        m.held.insert(T, None);
        f.named();
        assert_eq!(f.held, 2);
        let t = f.take(&[T + 10], 3 + FAULT_FRESH, l, &mut m);
        assert_eq!((t.made, t.given_back), (1, 0));
        let t = f.take(&[T + 11, T + 12], 3 + FAULT_FRESH, l, &mut m);
        assert_eq!((t.made, t.given_back), (2, 2));
        assert_eq!(m.given_back, [T + 1, T + 2]);
        assert!(m.has(T) && m.has(T + 10) && m.has(T + 11) && m.has(T + 12));
        assert_eq!(f.held, 3);
    }

    /// Faults outside title memory spend neither a frame's pages nor its
    /// tries; those inside it that are not the title's spend tries alone.
    #[test]
    fn wild_faults_spend_nothing() {
        let (mut f, mut m, l) = (FaultPages::default(), plain(), limits(4, 64));
        // Below 4 GiB, past the directory, and some in range.
        let mut pages: Vec<u64> = (1..5000).chain([u64::from(MEMORY_PAGES) - 1, u64::from(MEMORY_PAGES) + 7]).collect();
        m.not_title.extend((0..10).map(|i| T + i));
        pages.extend((0..14).map(|i| T + i));
        pages.sort_unstable();
        let t = f.take(&pages, 0, l, &mut m);
        assert_eq!((t.wild, t.not_title, t.made, t.capped), (5001, 10, 4, 0));
        assert_eq!(sums(&t), t.asked);
        assert_eq!(m.tried.len(), 14);
        assert!((10..14).all(|i| m.has(T + i)));
        assert_eq!(t.binding(), "none");
        assert!(!t.short());
        // Tries are bounded too.
        let mut m = plain();
        m.not_title.extend((0..5000).map(|i| T + i));
        let pages: Vec<u64> = (0..5000).map(|i| T + i).collect();
        let t = f.take(&pages, 1, l, &mut m);
        assert_eq!((t.not_title, t.capped, t.made), (FAULT_TRIES as u64, 5000 - FAULT_TRIES as u64, 0));
    }

    /// No room on the card: free places and given-back ones are used, else
    /// the page is refused for room and made when there is room again.
    #[test]
    fn no_room_is_tried_again() {
        let (mut f, mut m, l) = (FaultPages::default(), plain(), limits(128, 1024));
        m.room = false;
        let t = f.take(&[T], 0, l, &mut m);
        assert_eq!((t.made, t.no_room), (0, 1));
        assert_eq!(t.binding(), "room on the card");
        m.room = true;
        let t = f.take(&[T], 1, l, &mut m);
        assert_eq!((t.made, t.no_room), (1, 0));
        // The arena's other places, then the oldest pages for faults.
        m.room = false;
        let pages: Vec<u64> = (1..1024).map(|i| T + i).collect();
        let t = f.take(&pages[..127], 2, l, &mut m);
        assert_eq!(t.made, 127);
        for (frame, chunk) in (3..).zip(pages[127..].chunks(128)) {
            f.take(chunk, frame, l, &mut m);
        }
        assert_eq!((m.free, f.held), (0, 1024));
        let t = f.take(&[T + 5000], 100, l, &mut m);
        assert_eq!((t.made, t.given_back, t.no_room), (1, 1, 0));
        assert_eq!(m.given_back, [T]);
        // Pages in title buffers take no place.
        m.buffers.insert(T + 6000);
        let t = f.take(&[T + 6000], 101, l, &mut m);
        assert_eq!((t.made, t.given_back), (1, 0));
        assert_eq!(f.held, 1024);
    }

    /// Frames short of pages for [`STARVING`] are warned once, again only
    /// after a frame that was not.
    #[test]
    fn being_short_for_long_is_said_once() {
        let mut f = FaultPages::default();
        let short = FaultTally { capped: 3, ..FaultTally::default() };
        let start = std::time::Instant::now();
        let at = |s: f64| start + std::time::Duration::from_secs_f64(s);
        assert_eq!(f.watch(&short, at(0.0)), None);
        assert_eq!(f.watch(&short, at(4.9)), None);
        assert!(f.watch(&short, at(5.1)).is_some_and(|d| d.as_secs_f64() > 5.0));
        assert_eq!(f.watch(&short, at(9.0)), None);
        assert_eq!(f.watch(&FaultTally::default(), at(9.5)), None);
        assert_eq!(f.watch(&short, at(10.0)), None);
        assert!(f.watch(&short, at(15.5)).is_some());
        assert_eq!(short.binding(), "the limit of a frame");
    }
}

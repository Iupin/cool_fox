//! Which code writes a range of title memory, and what it writes: the
//! writers watch (`VICTORY_WRITERS`, `VICTORY_WRITERS_OF`; read in
//! diagnostics builds only, through `victory_hle::watch_writers`).
//!
//! The 4 KiB pages holding each watched range are kept read-only for the
//! watch's time window. A write into one faults into the guards' vectored
//! handler (`crate::watch::install`), which hands it here first
//! ([`on_exception`]):
//!
//! 1. What the guards do with a write to a block they guard is done first
//!    (`crate::watch::writer_opens`: the 64 KiB block opened at all its
//!    views, noted written and resting), so the GPU side's copies take the
//!    write as they would have; then, under the same lock, the 4 KiB page is
//!    made writable.
//! 2. The write is noted: the faulting instruction, the offset in the range
//!    (or a count for writes elsewhere in the page), its size (decoded by
//!    `crate::titlewrite`), the thread, the frame and, for the first
//!    `VICTORY_WRITERS_DETAIL` writes of each (instruction, offset), a stack.
//! 3. The thread goes on with the trap flag set. The instruction runs as it
//!    would have; the single-step exception that follows reads the bytes it
//!    wrote, makes the page read-only again and clears the flag. A string
//!    store (`rep movs`, `rep stos`) is stepped until it is done.
//!
//! Coexistence with the guards (`crate::watch`): the guards make whole
//! 64 KiB blocks writable (`unguard`); right after, [`reguard`] makes the
//! watched pages in them read-only again, except those a thread is stepping
//! or the kernel is writing. A kernel write into a watched page would fail
//! (the kernel does not fault into a handler), so `kernel_writing` opens the
//! watched pages too, until its `KernelWrite` is dropped, and the write is
//! noted as the kernel's, with the caller's stack. Writes the guards are told
//! of after the fact (`kernel_will_write`, DirectStorage's page writes,
//! landings beside the guards) did not fault and are noted as they are told.
//! A page shown at several addresses is watched at each of them, as the
//! guards do ([`arm`], from `crate::inplace::shown_elsewhere`), at the time
//! the range is armed.
//!
//! Nothing here runs unless the watch was started: [`USED`] is 0 and every
//! hook returns at once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::SeqCst};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, EXCEPTION_SINGLE_STEP};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_RECORD,
};

/// The unit watched: the kernel's page.
const SMALL: usize = 4096;
/// The guards' unit.
const BLOCK: usize = crate::watch::PAGE;
/// Pages watched at most (views of them included).
const SLOTS: usize = 128;
/// The longest range watched; longer ones are cut.
const MOST_RANGE: usize = 64 << 10;
/// Addresses taken per `VICTORY_WRITERS_OF` binding (double-buffered blocks
/// show at two).
#[cfg_attr(not(any(test, feature = "diagnostics")), allow(dead_code))] // the draws look in diagnostics builds
const MOST_OF: usize = 4;
/// Writers kept; more are counted as not kept.
const MOST_WRITERS: usize = 2048;
/// Instructions kept of writes elsewhere in the watched pages.
const MOST_ELSEWHERE: usize = 512;
/// Distinct values kept per writer.
const VALUES: usize = 4;
/// Bytes of a value kept.
const VALUE: usize = 16;
/// Frames of a stack kept at most.
const FRAMES: usize = 32;
/// Words of a range dumped.
const DUMP_WORDS: usize = 256;
/// The trap flag.
const TF: u32 = 0x100;
/// How often the table is told while there is something new.
const TABLE_EVERY: Duration = Duration::from_secs(10);

/// A page watched (at one of its addresses).
struct Slot {
    /// Its address; 0 for none. Never changes once set, but back to 0 for a
    /// page refused at arming.
    page: AtomicUsize,
    /// The address of the same bytes in the view the range was given in.
    canonical: AtomicUsize,
    /// Read-only by our hand now (and to be kept so).
    live: AtomicBool,
    /// Ranges armed that use it.
    users: AtomicU32,
    /// Threads stepping a write to it now.
    stepping: AtomicU32,
    /// Kernel writes into it now.
    kernel: AtomicU32,
}

impl Slot {
    const fn new() -> Self {
        Slot {
            page: AtomicUsize::new(0),
            canonical: AtomicUsize::new(0),
            live: AtomicBool::new(false),
            users: AtomicU32::new(0),
            stepping: AtomicU32::new(0),
            kernel: AtomicU32::new(0),
        }
    }
}

static SLOT: [Slot; SLOTS] = [const { Slot::new() }; SLOTS];
/// Slots `0..USED` have been given a page at some time.
static USED: AtomicUsize = AtomicUsize::new(0);
/// Taken by whoever gives slots pages or changes `live` (never by the
/// handler).
static ARMING: Mutex<()> = Mutex::new(());

/// The slot watching the 4 KiB page at `page`.
fn slot_of(page: usize) -> Option<usize> {
    let used = USED.load(SeqCst);
    (0..used).find(|&i| SLOT[i].page.load(SeqCst) == page)
}

/// Makes the 4 KiB page at `page` read-only, if it is read-write (or
/// read-only) now; what else is there keeps its protection.
fn read_only(page: usize) -> bool {
    set_protection(page, windows_sys::Win32::System::Memory::PAGE_READONLY)
}

/// Makes the 4 KiB page at `page` read-write, if it is read-only (or
/// read-write) now. For `crate::watch`, which calls it with the guards held.
pub(crate) fn read_write(page: usize) -> bool {
    set_protection(page, windows_sys::Win32::System::Memory::PAGE_READWRITE)
}

fn set_protection(page: usize, to: u32) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, PAGE_READWRITE, VirtualProtect};
    let mut old = 0;
    // SAFETY: only the protection of a page of title memory changes.
    let done = unsafe { VirtualProtect(page as *const _, SMALL, to, &mut old) } != 0;
    if done && !matches!(old, PAGE_READONLY | PAGE_READWRITE) {
        // Not ours to change (code, or memory the title protects): as it was.
        // SAFETY: as above.
        unsafe { VirtualProtect(page as *const _, SMALL, old, &mut old) };
        return false;
    }
    done
}

/// After a slot's page was made read-only on its way out of a step or a
/// kernel write: if the slot was let go meanwhile, the page must not stay
/// read-only (no one would open it for the title's next write).
fn kept_or_given_back(slot: usize) {
    let s = &SLOT[slot];
    if s.live.load(SeqCst) {
        return;
    }
    crate::watch::writer_retired(s.page.load(SeqCst));
}

/// One less of `count` holds the slot's page open: the last makes it
/// read-only again while the slot is armed.
fn let_go(slot: usize, count: &AtomicU32) {
    let s = &SLOT[slot];
    if count.fetch_sub(1, SeqCst) == 1
        && s.stepping.load(SeqCst) == 0
        && s.kernel.load(SeqCst) == 0
        && s.live.load(SeqCst)
    {
        read_only(s.page.load(SeqCst));
        kept_or_given_back(slot);
    }
}

/// The guards just made 64 KiB block `block` writable (`crate::watch`'s
/// `unguard`, with the guards' lock held): the watched pages in it are made
/// read-only again, but for those being stepped or written by the kernel.
pub(crate) fn reguard(block: usize) {
    let used = USED.load(SeqCst);
    for s in &SLOT[..used] {
        let page = s.page.load(SeqCst);
        if page != 0
            && page / BLOCK == block
            && s.live.load(SeqCst)
            && s.stepping.load(SeqCst) == 0
            && s.kernel.load(SeqCst) == 0
        {
            read_only(page);
        }
    }
}

/// The runtime mapped title memory `[at, at + len)` anew: what it shows is
/// read-write, watched pages there among it.
pub(crate) fn remapped(at: usize, len: usize) {
    let used = USED.load(SeqCst);
    if used == 0 {
        return;
    }
    let end = at.saturating_add(len);
    for s in &SLOT[..used] {
        let page = s.page.load(SeqCst);
        if page != 0
            && page < end
            && page + SMALL > at
            && s.live.load(SeqCst)
            && s.stepping.load(SeqCst) == 0
            && s.kernel.load(SeqCst) == 0
        {
            read_only(page);
            if !s.live.load(SeqCst) {
                // Let go meanwhile, its release perhaps before this.
                crate::watch::writer_retired(page);
            }
        }
    }
}

/// What made a write.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Kind {
    /// An instruction that faulted (the title's, or one of ours: a landing
    /// through `crate::safecopy`).
    Code,
    /// The kernel, for a call of the runtime's (`kernel_writing`); the
    /// instruction is the first caller outside our image.
    Kernel,
    /// A write the guards were told of after it was done
    /// (`kernel_will_write`).
    Told,
    /// DirectStorage's write of physical pages through a view of our own.
    Pages,
    /// What shaders wrote, landed beside the guards.
    Landed,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Code => "code",
            Kind::Kernel => "kernel",
            Kind::Told => "kernel (told after)",
            Kind::Pages => "kernel (page write)",
            Kind::Landed => "gpu (landed)",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Key {
    pub(crate) kind: Kind,
    /// The writing instruction (for the kernel's, the first caller outside
    /// our image; 0 for landings).
    pub(crate) rip: usize,
    pub(crate) range: u16,
    pub(crate) offset: u32,
}

/// One writer of one offset, so far.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Writer {
    pub(crate) count: u64,
    /// Writes noted in detail (with a stack).
    pub(crate) detailed: u32,
    pub(crate) first_frame: u64,
    pub(crate) last_frame: u64,
    first_at: Duration,
    last_at: Duration,
    /// Bytes per write (0: not known); bytes covered by the last one.
    pub(crate) size: u16,
    pub(crate) covered: u32,
    pub(crate) values: [[u8; VALUE]; VALUES],
    pub(crate) value_len: [u8; VALUES],
    pub(crate) distinct: u8,
    pub(crate) last: [u8; VALUE],
    pub(crate) last_len: u8,
    /// Writes whose value differed from the one before.
    pub(crate) changes: u64,
    pub(crate) valued: bool,
    pub(crate) stack: [u64; FRAMES],
    pub(crate) stack_len: u8,
    /// Detailed writes whose stack differed from the one kept.
    pub(crate) other_stacks: u32,
    pub(crate) thread: u32,
    pub(crate) last_thread: u32,
    said: bool,
}

impl Writer {
    fn new(frame: u64, at: Duration, thread: u32) -> Self {
        Writer {
            count: 0,
            detailed: 0,
            first_frame: frame,
            last_frame: frame,
            first_at: at,
            last_at: at,
            size: 0,
            covered: 0,
            values: [[0; VALUE]; VALUES],
            value_len: [0; VALUES],
            distinct: 0,
            last: [0; VALUE],
            last_len: 0,
            changes: 0,
            valued: false,
            stack: [0; FRAMES],
            stack_len: 0,
            other_stacks: 0,
            thread,
            last_thread: thread,
            said: false,
        }
    }

    fn value(&mut self, bytes: &[u8], covered: usize) -> bool {
        let n = bytes.len().min(VALUE);
        let mut v = [0; VALUE];
        v[..n].copy_from_slice(&bytes[..n]);
        self.covered = covered.min(u32::MAX as usize) as u32;
        if self.valued && (self.last != v || usize::from(self.last_len) != n) {
            self.changes += 1;
        }
        self.last = v;
        self.last_len = n as u8;
        self.valued = true;
        let seen = (0..usize::from(self.distinct))
            .any(|i| self.values[i] == v && usize::from(self.value_len[i]) == n);
        if !seen && usize::from(self.distinct) < VALUES {
            self.values[usize::from(self.distinct)] = v;
            self.value_len[usize::from(self.distinct)] = n as u8;
            self.distinct += 1;
            return true;
        }
        false
    }

    fn stack(&mut self, frames: &[u64]) {
        if self.stack_len == 0 {
            let n = frames.len().min(FRAMES);
            self.stack[..n].copy_from_slice(&frames[..n]);
            self.stack_len = n as u8;
        } else if self.stack[..usize::from(self.stack_len)] != frames[..frames.len().min(FRAMES)] {
            self.other_stacks += 1;
        }
    }
}

/// A watched range.
struct Range {
    start: usize,
    len: usize,
    /// What it was asked as (the switch's text, or the binding).
    origin: String,
    slots: Vec<usize>,
    armed: bool,
    /// Times it was to be armed and none of its pages could be (memory not
    /// there yet): it is tried again at every look while the window is open.
    tries: u32,
}

/// A `VICTORY_WRITERS_OF` binding: a shader and one of its table views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Of {
    pub(crate) hash: String,
    pub(crate) slot: u8,
    pub(crate) offset: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Config {
    pub(crate) ranges: Vec<(usize, usize)>,
    pub(crate) of: Vec<Of>,
    /// The window: from so long after the start, for so long (`None`: to the
    /// end).
    pub(crate) from: Duration,
    pub(crate) span: Option<Duration>,
    /// Writes of each (instruction, offset) noted in detail.
    pub(crate) detail: u32,
    /// Frames of a stack.
    pub(crate) frames: usize,
}

impl Config {
    /// From the switches `get` reads; `None` if neither `VICTORY_WRITERS`
    /// nor `VICTORY_WRITERS_OF` names anything. Parts that do not parse are
    /// named in the error list and left out.
    pub(crate) fn from_switches(
        get: impl Fn(&str) -> Option<String>,
    ) -> (Option<Config>, Vec<String>) {
        let mut bad = Vec::new();
        let hex = |s: &str| {
            let s = s.trim();
            usize::from_str_radix(
                s.strip_prefix("0x")
                    .or_else(|| s.strip_prefix("0X"))
                    .unwrap_or(s),
                16,
            )
            .ok()
        };
        let mut ranges = Vec::new();
        for spec in get("VICTORY_WRITERS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match spec
                .split_once('+')
                .and_then(|(at, len)| Some((hex(at)?, hex(len)?)))
            {
                Some((at, len)) if at != 0 && len != 0 && at.checked_add(len).is_some() => {
                    if len > MOST_RANGE {
                        bad.push(format!("{spec}: longer than {MOST_RANGE:#x} bytes, cut"));
                    }
                    ranges.push((at, len.min(MOST_RANGE)));
                }
                _ => bad.push(format!("{spec}: not <hex address>+<hex length>")),
            }
        }
        let mut of = Vec::new();
        for spec in get("VICTORY_WRITERS_OF")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let parsed = spec.split_once(':').and_then(|(hash, binding)| {
                let hash = hash.trim().to_lowercase();
                let (slot, offset) = binding.trim().strip_prefix('s')?.split_once('+')?;
                (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(())?;
                Some(Of {
                    hash,
                    slot: slot.parse().ok()?,
                    offset: offset.parse().ok()?,
                })
            });
            match parsed {
                Some(o) => of.push(o),
                None => bad.push(format!("{spec}: not <16-hex shader hash>:s<slot>+<offset>")),
            }
        }
        if ranges.is_empty() && of.is_empty() {
            return (None, bad);
        }
        let seconds = |name: &str, bad: &mut Vec<String>| {
            let text = get(name)?;
            match text
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(|s| Duration::try_from_secs_f64(s.max(0.0)).ok())
            {
                Some(d) => Some(d),
                None => {
                    bad.push(format!("{name}={text}: not seconds"));
                    None
                }
            }
        };
        let number = |name: &str, default: usize, bad: &mut Vec<String>| match get(name) {
            None => default,
            Some(text) => text.trim().parse().unwrap_or_else(|_| {
                bad.push(format!("{name}={text}: not a number"));
                default
            }),
        };
        let config = Config {
            ranges,
            of,
            from: seconds("VICTORY_WRITERS_FROM", &mut bad).unwrap_or_default(),
            span: seconds("VICTORY_WRITERS_FOR", &mut bad),
            detail: number("VICTORY_WRITERS_DETAIL", 64, &mut bad).min(u32::MAX as usize) as u32,
            frames: number("VICTORY_WRITERS_STACK", 12, &mut bad).min(FRAMES),
        };
        (Some(config), bad)
    }
}

/// Writes elsewhere in the watched pages, by instruction.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Elsewhere {
    pub(crate) count: u64,
    pub(crate) page: usize,
}

/// A kernel write under way into watched pages ([`kernel_begin`]).
pub(crate) struct Held {
    slots: Vec<usize>,
    at: usize,
    len: usize,
    caller: usize,
    stack: [u64; FRAMES],
    stack_len: usize,
    thread: u32,
}

struct State {
    config: Config,
    started: Instant,
    open: bool,
    closed: bool,
    ranges: Vec<Range>,
    /// Addresses found for each `of` binding.
    #[cfg_attr(not(any(test, feature = "diagnostics")), allow(dead_code))]
    // the draws look in diagnostics builds
    found: Vec<Vec<usize>>,
    writers: HashMap<Key, Writer>,
    elsewhere: HashMap<usize, Elsewhere>,
    /// Writes not kept (too many writers).
    dropped: u64,
    /// Writes stepped.
    steps: u64,
    kept: Vec<(Box<Held>, Instant)>,
    last_table: Duration,
    /// Something new since the last table.
    new: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// `VICTORY_WRITERS_OF` still looks at draws.
static LOOKING: AtomicBool = AtomicBool::new(false);
/// The shaders `VICTORY_WRITERS_OF` names (for draws, without the state's
/// lock).
static OF: RwLock<Vec<Of>> = RwLock::new(Vec::new());

type Namer = fn(usize) -> String;
type Unwinder = fn(&CONTEXT, &mut [u64]) -> usize;
static NAMER: OnceLock<Namer> = OnceLock::new();
static UNWINDER: OnceLock<Unwinder> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn name(address: usize) -> String {
    match NAMER.get() {
        Some(namer) => namer(address),
        None => format!("{address:#x}"),
    }
}

/// The stack of the thread whose context this is (the current one).
fn unwind(context: &CONTEXT, out: &mut [u64]) -> usize {
    match UNWINDER.get() {
        Some(unwinder) => unwinder(context, out),
        None => {
            if let Some(first) = out.first_mut() {
                *first = context.Rip;
                1
            } else {
                0
            }
        }
    }
}

fn thread_id() -> u32 {
    // SAFETY: a plain query.
    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
}

/// Starts the watch the switches ask for, if they ask for one: the time
/// window counts from now. `namer` names code as `module+rva`, `unwinder`
/// walks the current thread's stack from a context of it (the loader's
/// `locate` and `unwind_here`). Whether a watch was started.
pub fn writers_from_env(namer: Namer, unwinder: Unwinder) -> bool {
    let (config, bad) =
        Config::from_switches(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()));
    for bad in bad {
        tracing::warn!("writers: {bad}");
    }
    let Some(config) = config else {
        return false;
    };
    let _ = NAMER.set(namer);
    let _ = UNWINDER.set(unwinder);
    tracing::info!(
        ranges = ?config.ranges.iter().map(|&(at, len)| format!("{at:#x}+{len:#x}")).collect::<Vec<_>>(),
        of = ?config.of.iter().map(|o| format!("{}:s{}+{}", o.hash, o.slot, o.offset)).collect::<Vec<_>>(),
        from = config.from.as_secs_f64(),
        span = config.span.map(|s| s.as_secs_f64()),
        detail = config.detail,
        stack = config.frames,
        "writers: watch asked for"
    );
    let started = start(config);
    std::thread::Builder::new()
        .name("victory-writers".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                if !tick(started.elapsed()) {
                    break;
                }
            }
        })
        .is_ok()
}

/// Sets the watch up with `config`; its time counts from what this returns.
fn start(config: Config) -> Instant {
    crate::watch::install();
    let started = Instant::now();
    *OF.write().unwrap_or_else(|e| e.into_inner()) = config.of.clone();
    let ranges = config
        .ranges
        .iter()
        .map(|&(start, len)| Range {
            start,
            len,
            origin: format!("{start:#x}+{len:#x}"),
            slots: Vec::new(),
            armed: false,
            tries: 0,
        })
        .collect();
    let found = vec![Vec::new(); config.of.len()];
    LOOKING.store(!config.of.is_empty(), SeqCst);
    let mut writers = HashMap::new();
    writers.reserve(MOST_WRITERS);
    let mut elsewhere = HashMap::new();
    elsewhere.reserve(MOST_ELSEWHERE);
    *state() = Some(State {
        config,
        started,
        open: false,
        closed: false,
        ranges,
        found,
        writers,
        elsewhere,
        dropped: 0,
        steps: 0,
        kept: Vec::new(),
        last_table: Duration::ZERO,
        new: false,
    });
    started
}

/// The process ends: the table and the ranges' words as they are now.
pub fn writers_at_exit() {
    if USED.load(SeqCst) == 0 && state().is_none() {
        return;
    }
    let (table, dumps) = {
        let st = state();
        let Some(st) = st.as_ref() else {
            return;
        };
        (
            snapshot(st),
            st.ranges
                .iter()
                .map(|r| (r.start, r.len, r.origin.clone()))
                .collect::<Vec<_>>(),
        )
    };
    for line in table.lines("exit") {
        tracing::info!("{line}");
    }
    for (start, len, origin) in dumps {
        for line in dump(start, len, &origin, "exit") {
            tracing::info!("{line}");
        }
    }
}

/// What the reporter does every 100 ms, `now` after the start: opens and
/// closes the window, arms ranges found, lets go of kernel writes whose time
/// is up, tells new writers and, every 10 s while there is something new,
/// the table. Whether to go on.
pub(crate) fn tick(now: Duration) -> bool {
    let mut lines = Vec::new();
    let (to_arm, closing, expired, table) = {
        let mut guard = state();
        let Some(st) = guard.as_mut() else {
            return false;
        };
        if !st.open && !st.closed && now >= st.config.from {
            st.open = true;
            lines.push(format!(
                "writers: the window opens at {:.1} s",
                now.as_secs_f64()
            ));
        }
        let closing = st.open
            && st
                .config
                .span
                .is_some_and(|span| now >= st.config.from + span);
        if closing {
            st.open = false;
            st.closed = true;
            LOOKING.store(false, SeqCst);
        }
        let to_arm: Vec<usize> = if st.open {
            (0..st.ranges.len())
                .filter(|&i| !st.ranges[i].armed)
                .collect()
        } else {
            Vec::new()
        };
        let now_instant = Instant::now();
        let mut expired = Vec::new();
        let mut i = 0;
        while i < st.kept.len() {
            if st.kept[i].1 <= now_instant || closing {
                expired.push(st.kept.swap_remove(i).0);
            } else {
                i += 1;
            }
        }
        let told = st.new && now.saturating_sub(st.last_table) >= TABLE_EVERY;
        let table = (closing || told).then(|| {
            st.last_table = now;
            st.new = false;
            snapshot(st)
        });
        (to_arm, closing, expired, table)
    };
    for held in expired {
        kernel_end(held);
    }
    for i in to_arm {
        lines.extend(arm(i));
    }
    lines.extend(newly_seen());
    if let Some(table) = table {
        lines.extend(table.lines(if closing { "the window's end" } else { "10 s" }));
    }
    if closing {
        let dumps: Vec<(usize, usize, String)> = {
            let st = state();
            st.as_ref()
                .map(|st| {
                    st.ranges
                        .iter()
                        .filter(|r| r.armed)
                        .map(|r| (r.start, r.len, r.origin.clone()))
                        .collect()
                })
                .unwrap_or_default()
        };
        for (start, len, origin) in dumps {
            lines.extend(dump(start, len, &origin, "the window's end"));
        }
        lines.extend(disarm_all());
        lines.push(format!(
            "writers: the window closes at {:.1} s; the pages are left as they were",
            now.as_secs_f64()
        ));
    }
    for line in &lines {
        tracing::info!("{line}");
    }
    !closing
}

/// The addresses of the watched pages of `[start, start + len)` (each with
/// the address of its bytes in that range's view), the same pages at the
/// other addresses that show them among them.
fn pages_of(start: usize, len: usize) -> Vec<(usize, usize)> {
    let first = start & !(SMALL - 1);
    let end = start + len;
    let own: Vec<usize> = (first..end).step_by(SMALL).collect();
    let mut pages: Vec<(usize, usize)> = own.iter().map(|&p| (p, p)).collect();
    for (block, others) in crate::inplace::shown_elsewhere(first & !(BLOCK - 1), end) {
        for &page in own.iter().filter(|&&p| p & !(BLOCK - 1) == block) {
            pages.extend(others.iter().map(|&other| (other + (page - block), page)));
        }
    }
    pages
}

/// Arms range `i`: its pages (at every view) read-only, its words dumped.
fn arm(i: usize) -> Vec<String> {
    let Some((start, len, origin)) = state()
        .as_ref()
        .and_then(|st| st.ranges.get(i).map(|r| (r.start, r.len, r.origin.clone())))
    else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let pages = pages_of(start, len);
    let mut slots = Vec::new();
    let mut refused = Vec::new();
    {
        let _arming = ARMING.lock().unwrap_or_else(|e| e.into_inner());
        let mut asked: Vec<(usize, bool, usize, bool)> = Vec::new();
        for &(page, canonical) in &pages {
            let found = slot_of(page);
            let slot = match found {
                Some(slot) => slot,
                None => {
                    let used = USED.load(SeqCst);
                    let free = (0..used).find(|&s| SLOT[s].page.load(SeqCst) == 0);
                    match free.or_else(|| (used < SLOTS).then_some(used)) {
                        Some(s) => {
                            SLOT[s].canonical.store(canonical, SeqCst);
                            SLOT[s].page.store(page, SeqCst);
                            if s == used {
                                USED.store(used + 1, SeqCst);
                            }
                            s
                        }
                        None => {
                            refused.push((page, "no slot left"));
                            continue;
                        }
                    }
                }
            };
            let s = &SLOT[slot];
            if s.live.load(SeqCst) {
                s.users.fetch_add(1, SeqCst);
                slots.push(slot);
                continue;
            }
            // Before the protection: a write that faults sees it.
            s.live.store(true, SeqCst);
            asked.push((page, found.is_some(), slot, found.is_none()));
        }
        let pages: Vec<(usize, bool)> =
            asked.iter().map(|&(page, ours, ..)| (page, ours)).collect();
        let done = crate::watch::writers_protect(&pages);
        for (&(page, _, slot, new), ok) in asked.iter().zip(done) {
            let s = &SLOT[slot];
            if ok {
                s.users.fetch_add(1, SeqCst);
                slots.push(slot);
            } else {
                s.live.store(false, SeqCst);
                if new {
                    s.page.store(0, SeqCst);
                }
                refused.push((page, "not read-write title memory"));
            }
        }
    }
    let views = pages.iter().filter(|(p, c)| p != c).count();
    let tries = {
        let mut st = state();
        let Some(r) = st.as_mut().and_then(|st| st.ranges.get_mut(i)) else {
            return lines;
        };
        if slots.is_empty() {
            r.tries += 1;
            r.tries
        } else {
            r.slots = slots.clone();
            r.armed = true;
            0
        }
    };
    if tries != 0 {
        // Said at the 1st, 2nd, 4th... try (ten a second).
        if tries.is_power_of_two() {
            lines.push(format!(
                "writers: none of the {} pages of {origin} at {start:#x}+{len:#x} can be watched yet ({}); tried {tries} times",
                pages.len(),
                refused.first().map_or("no pages", |r| r.1)
            ));
        }
        return lines;
    }
    lines.push(format!(
        "writers: watching {origin} at {start:#x}+{len:#x}: {} of {} pages read-only ({views} at other views), frame {}",
        slots.len(),
        pages.len(),
        crate::frame::current()
    ));
    for (page, why) in refused {
        lines.push(format!(
            "writers: page {page:#x} of {origin} is not watched: {why}"
        ));
    }
    lines.extend(dump(start, len, &origin, "the start of its watch"));
    lines
}

/// Lets every armed range go: its pages read-write again where the guards
/// do not hold their block.
fn disarm_all() -> Vec<String> {
    let slots: Vec<usize> = {
        let mut st = state();
        let Some(st) = st.as_mut() else {
            return Vec::new();
        };
        st.ranges
            .iter_mut()
            .filter(|r| r.armed)
            .flat_map(|r| {
                r.armed = false;
                std::mem::take(&mut r.slots)
            })
            .collect()
    };
    let _arming = ARMING.lock().unwrap_or_else(|e| e.into_inner());
    let mut pages = Vec::new();
    for slot in slots {
        let s = &SLOT[slot];
        if s.users.fetch_sub(1, SeqCst) == 1 {
            s.live.store(false, SeqCst);
            pages.push(s.page.load(SeqCst));
        }
    }
    crate::watch::writers_release(&pages);
    vec![format!("writers: {} pages let go", pages.len())]
}

/// The range holding `canonical`, and the offset in it.
fn range_of(st: &State, canonical: usize) -> Option<(u16, u32)> {
    st.ranges
        .iter()
        .enumerate()
        .find(|(_, r)| r.armed && (r.start..r.start + r.len).contains(&canonical))
        .map(|(i, r)| (i as u16, (canonical - r.start) as u32))
}

/// What a step under way is about.
#[derive(Clone, Copy)]
enum Entry {
    None,
    Writer(Key),
}

/// A write being stepped on this thread.
#[derive(Clone, Copy)]
struct Step {
    active: bool,
    slots: [u8; 8],
    n: u8,
    rip: u64,
    at: usize,
    size: u16,
    rep: bool,
    rdi: u64,
    entry: Entry,
}

impl Step {
    const NONE: Step = Step {
        active: false,
        slots: [0; 8],
        n: 0,
        rip: 0,
        at: 0,
        size: 0,
        rep: false,
        rdi: 0,
        entry: Entry::None,
    };
}

thread_local! {
    static STEP: std::cell::Cell<Step> = const { std::cell::Cell::new(Step::NONE) };
}

/// The string store at the start of `code` (`rep movs`/`stos`, or one
/// without the prefix), and its element's size.
fn string_store(code: &[u8]) -> Option<(bool, u16)> {
    let (mut rep, mut narrow, mut wide) = (false, false, false);
    for &b in code.iter().take(15) {
        match b {
            0xf2 | 0xf3 => rep = true,
            0x66 => narrow = true,
            0x2e | 0x36 | 0x3e | 0x26 | 0x64 | 0x65 | 0x67 => {}
            0x40..=0x4f => wide = b & 8 != 0,
            0xa4 | 0xaa => return Some((rep, 1)),
            0xa5 | 0xab => {
                return Some((
                    rep,
                    if wide {
                        8
                    } else if narrow {
                        2
                    } else {
                        4
                    },
                ));
            }
            _ => return None,
        }
    }
    None
}

/// For the guards' handler, first: a write fault in a watched page, or the
/// single step after one, is done here (`Some`, how to go on); anything else
/// is the handler's (`None`).
///
/// # Safety
/// `record` and `context` are the exception being dispatched on this thread.
pub(crate) unsafe fn on_exception(record: &EXCEPTION_RECORD, context: &mut CONTEXT) -> Option<i32> {
    if record.ExceptionCode == EXCEPTION_SINGLE_STEP {
        let step = STEP.get();
        if !step.active {
            return None;
        }
        stepped(step, context);
        return Some(EXCEPTION_CONTINUE_EXECUTION);
    }
    if USED.load(SeqCst) == 0
        || record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION
        || record.NumberParameters < 2
        || record.ExceptionInformation[0] != 1
    {
        return None;
    }
    let at = record.ExceptionInformation[1];
    let page = at & !(SMALL - 1);
    let slot = slot_of(page)?;
    let s = &SLOT[slot];
    if !s.live.load(SeqCst) {
        // Let go: whatever made it read-only, it is not ours now (see
        // `kept_or_given_back` for what a step or kernel write left).
        return None;
    }
    let mut step = STEP.get();
    if step.active {
        // The instruction being stepped faulted again: its other page, or
        // the guards made the block read-only in between.
        if !step.slots[..usize::from(step.n)].contains(&(slot as u8)) {
            if usize::from(step.n) == step.slots.len() {
                return None;
            }
            s.stepping.fetch_add(1, SeqCst);
            step.slots[usize::from(step.n)] = slot as u8;
            step.n += 1;
            STEP.set(step);
        }
        crate::watch::writer_opens(at, page, context.Rip as usize);
        return Some(EXCEPTION_CONTINUE_EXECUTION);
    }
    s.stepping.fetch_add(1, SeqCst);
    if !crate::watch::writer_opens(at, page, context.Rip as usize) {
        let_go(slot, &s.stepping);
        return None;
    }
    let rip = context.Rip as usize;
    let mut code = [0u8; 15];
    let room = (SMALL - rip % SMALL).min(code.len());
    let read = crate::host::read_into(rip as *const u8, &mut code)
        || crate::host::read_into(rip as *const u8, &mut code[..room]);
    let (size, rep) = if !read {
        (0, false)
    } else if let Some((rep, size)) = string_store(&code) {
        (size, rep)
    } else {
        let size = crate::titlewrite::store_extent(&code, context)
            .filter(|&(store, size)| (store..store + size).contains(&at))
            .map_or(0, |(_, size)| size);
        (size.min(usize::from(u16::MAX)) as u16, false)
    };
    let entry = noted(context, at, slot, size);
    let mut slots = [0u8; 8];
    slots[0] = slot as u8;
    STEP.set(Step {
        active: true,
        slots,
        n: 1,
        rip: context.Rip,
        at,
        size,
        rep,
        rdi: context.Rdi,
        entry,
    });
    context.EFlags |= TF;
    Some(EXCEPTION_CONTINUE_EXECUTION)
}

/// The fault of a write at `at` (in the page of `slot`), as the state notes
/// it.
fn noted(context: &CONTEXT, at: usize, slot: usize, size: u16) -> Entry {
    let s = &SLOT[slot];
    let canonical = s.canonical.load(SeqCst) + (at - s.page.load(SeqCst));
    let rip = context.Rip as usize;
    let mut guard = state();
    let Some(st) = guard.as_mut() else {
        return Entry::None;
    };
    st.steps += 1;
    let Some((range, offset)) = range_of(st, canonical) else {
        let full = st.elsewhere.len() >= MOST_ELSEWHERE;
        match st.elsewhere.get_mut(&rip) {
            Some(e) => e.count += 1,
            None if !full => {
                st.elsewhere.insert(
                    rip,
                    Elsewhere {
                        count: 1,
                        page: canonical & !(SMALL - 1),
                    },
                );
                st.new = true;
            }
            None => st.dropped += 1,
        }
        return Entry::None;
    };
    let key = Key {
        kind: Kind::Code,
        rip,
        range,
        offset,
    };
    let frame = crate::frame::current();
    let now = st.started.elapsed();
    let thread = thread_id();
    if !st.writers.contains_key(&key) && st.writers.len() >= MOST_WRITERS {
        st.dropped += 1;
        return Entry::None;
    }
    let (detail, frames) = (st.config.detail, st.config.frames);
    let w = st
        .writers
        .entry(key)
        .or_insert_with(|| Writer::new(frame, now, thread));
    w.count += 1;
    w.last_frame = frame;
    w.last_at = now;
    w.last_thread = thread;
    if size != 0 {
        w.size = size;
    }
    if w.detailed < detail {
        w.detailed += 1;
        let mut stack = [0u64; FRAMES];
        let n = unwind(context, &mut stack[..frames]);
        w.stack(&stack[..n]);
    }
    Entry::Writer(key)
}

/// The single step after a write that faulted: what it wrote is read, and
/// its pages are read-only again.
fn stepped(step: Step, context: &mut CONTEXT) {
    if step.rep && context.Rip == step.rip {
        // A string store between two of its elements.
        context.EFlags |= TF;
        return;
    }
    context.EFlags &= !TF;
    STEP.set(Step::NONE);
    if let Entry::Writer(key) = step.entry {
        let covered = if step.rep {
            context.Rdi.abs_diff(step.rdi) as usize
        } else {
            usize::from(step.size)
        };
        let (from, n) = match covered {
            0 => (step.at & !3, 4),
            _ if step.rep && context.EFlags & 0x400 != 0 => {
                (step.at, usize::from(step.size).max(1))
            }
            n => (step.at, n.min(VALUE)),
        };
        let mut value = [0u8; VALUE];
        let read = crate::host::read_into(from as *const u8, &mut value[..n]);
        if read {
            let mut guard = state();
            if let Some(st) = guard.as_mut()
                && let Some(w) = st.writers.get_mut(&key)
                && w.value(&value[..n], covered)
            {
                st.new = true;
            }
        }
    }
    for &slot in &step.slots[..usize::from(step.n)] {
        let slot = usize::from(slot);
        let_go(slot, &SLOT[slot].stepping);
    }
}

/// The first frame of `stack` outside the image holding this code, or the
/// first frame.
fn caller(stack: &[u64]) -> usize {
    static OURS: OnceLock<(usize, usize)> = OnceLock::new();
    let (base, end) = *OURS.get_or_init(|| image_of(caller as fn(&[u64]) -> usize as usize));
    stack
        .iter()
        .map(|&pc| pc as usize)
        .find(|pc| !(base..end).contains(pc))
        .or_else(|| stack.first().map(|&pc| pc as usize))
        .unwrap_or(0)
}

/// The image holding `address`: its base and end (nothing if none).
fn image_of(address: usize) -> (usize, usize) {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleHandleExW,
    };
    let mut module = std::ptr::null_mut();
    // SAFETY: looks the module up; no reference is taken.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    } != 0;
    if !found {
        return (0, 0);
    }
    let base = module as usize;
    // SAFETY: a loaded image's headers: `e_lfanew` at 0x3c, `SizeOfImage` at
    // 0x50 of the NT headers.
    let size = unsafe {
        let nt = base + (base as *const u8).add(0x3c).cast::<u32>().read_unaligned() as usize;
        (nt as *const u8).add(0x50).cast::<u32>().read_unaligned() as usize
    };
    (base, base + size)
}

/// The current thread's stack, from its caller up.
fn own_stack(out: &mut [u64; FRAMES]) -> usize {
    use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureContext;
    #[repr(C, align(16))]
    struct Aligned(CONTEXT);
    // SAFETY: a zeroed CONTEXT is valid; it is captured 16-byte aligned.
    let context = unsafe {
        let mut context = Aligned(std::mem::zeroed());
        RtlCaptureContext(&mut context.0);
        context.0
    };
    unwind(&context, out)
}

/// Notes a write of `kind` into title memory `[at, at + len)`, done now, by
/// `rip` (or, for the kernel's, the caller in `stack`), with the bytes that
/// are there now, for every watched range it meets.
fn note_done(kind: Kind, rip: usize, stack: &[u64], thread: u32, at: usize, len: usize) {
    let end = at.saturating_add(len);
    let mut pieces = Vec::new();
    for page in (at & !(SMALL - 1)..end).step_by(SMALL) {
        let Some(slot) = slot_of(page) else {
            continue;
        };
        let canonical = SLOT[slot].canonical.load(SeqCst);
        let (from, to) = (at.max(page), end.min(page + SMALL));
        pieces.push((from, canonical + (from - page), to - from));
    }
    if pieces.is_empty() {
        return;
    }
    let mut guard = state();
    let Some(st) = guard.as_mut() else {
        return;
    };
    let frame = crate::frame::current();
    let now = st.started.elapsed();
    for (from, canonical, n) in pieces {
        for (i, r) in st.ranges.iter().enumerate().filter(|(_, r)| r.armed) {
            let (a, b) = (canonical.max(r.start), (canonical + n).min(r.start + r.len));
            if a >= b {
                continue;
            }
            let key = Key {
                kind,
                rip,
                range: i as u16,
                offset: (a - r.start) as u32,
            };
            if !st.writers.contains_key(&key) && st.writers.len() >= MOST_WRITERS {
                st.dropped += 1;
                continue;
            }
            let mut value = [0u8; VALUE];
            let shown = (b - a).min(VALUE);
            let read =
                crate::host::read_into((from + (a - canonical)) as *const u8, &mut value[..shown]);
            let w = st
                .writers
                .entry(key)
                .or_insert_with(|| Writer::new(frame, now, thread));
            w.count += 1;
            w.last_frame = frame;
            w.last_at = now;
            w.last_thread = thread;
            w.size = (b - a).min(usize::from(u16::MAX)) as u16;
            if w.detailed < st.config.detail {
                w.detailed += 1;
                w.stack(&stack[..stack.len().min(st.config.frames)]);
            }
            if read && w.value(&value[..shown], b - a) {
                st.new = true;
            }
        }
    }
}

/// Before the kernel writes title memory `[at, at + len)` for a call of the
/// runtime's (`crate::watch::kernel_writing`, which then opens what it
/// returns with [`kernel_open`] under the guards' lock): the watched pages
/// there are held open until [`kernel_end`].
pub(crate) fn kernel_begin(at: usize, len: usize) -> Option<Box<Held>> {
    if USED.load(SeqCst) == 0 || len == 0 {
        return None;
    }
    let end = at.saturating_add(len);
    let slots: Vec<usize> = (at & !(SMALL - 1)..end)
        .step_by(SMALL)
        .filter_map(slot_of)
        .filter(|&slot| SLOT[slot].live.load(SeqCst))
        .collect();
    if slots.is_empty() {
        return None;
    }
    for &slot in &slots {
        SLOT[slot].kernel.fetch_add(1, SeqCst);
    }
    let mut stack = [0u64; FRAMES];
    let stack_len = own_stack(&mut stack);
    Some(Box::new(Held {
        caller: caller(&stack[..stack_len]),
        slots,
        at,
        len,
        stack,
        stack_len,
        thread: thread_id(),
    }))
}

/// [`kernel_begin`]'s pages made writable, with the guards' lock held.
pub(crate) fn kernel_open(held: &Held) {
    for &slot in &held.slots {
        read_write(SLOT[slot].page.load(SeqCst));
    }
}

/// The kernel's write is done: it is noted, with the bytes it left, and its
/// pages are read-only again.
pub(crate) fn kernel_end(held: Box<Held>) {
    note_done(
        Kind::Kernel,
        held.caller,
        &held.stack[..held.stack_len],
        held.thread,
        held.at,
        held.len,
    );
    for &slot in &held.slots {
        let_go(slot, &SLOT[slot].kernel);
    }
}

/// The kernel's write goes on after its call returned (an overlapped
/// read): it is done when `time` is up.
pub(crate) fn kernel_keep(held: Box<Held>, time: Duration) {
    let mut guard = state();
    match guard.as_mut() {
        Some(st) => st.kept.push((held, Instant::now() + time)),
        None => {
            drop(guard);
            kernel_end(held);
        }
    }
}

/// The guards are told of a write into `[at, at + len)` that did not fault
/// (`kind`: how it was made).
pub(crate) fn told(kind: Kind, at: usize, len: usize) {
    if USED.load(SeqCst) == 0 || len == 0 {
        return;
    }
    let mut stack = [0u64; FRAMES];
    let n = if kind == Kind::Landed {
        0
    } else {
        own_stack(&mut stack)
    };
    let rip = if kind == Kind::Landed {
        0
    } else {
        caller(&stack[..n])
    };
    note_done(kind, rip, &stack[..n], thread_id(), at, len);
}

/// Whether draws are looked at for `VICTORY_WRITERS_OF` (`crate::draw`, in
/// diagnostics builds, which alone call this and the three below).
#[cfg_attr(not(any(test, feature = "diagnostics")), allow(dead_code))]
pub(crate) fn looking() -> bool {
    LOOKING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether `VICTORY_WRITERS_OF` names the binding (table `slot`, at
/// `offset` dwords) of the shader `hash`.
#[cfg_attr(not(any(test, feature = "diagnostics")), allow(dead_code))]
pub(crate) fn binding_wanted(hash: &str, slot: u8, offset: u32) -> bool {
    OF.read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|o| o.hash == hash && o.slot == slot && o.offset == offset)
}

/// Whether `VICTORY_WRITERS_OF` names the shader `hash`.
#[cfg_attr(not(any(test, feature = "diagnostics")), allow(dead_code))]
pub(crate) fn shader_wanted(hash: &str) -> bool {
    OF.read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|o| o.hash == hash)
}

/// A draw with the shader `hash` binds title memory `[address, address +
/// len)` at a binding `VICTORY_WRITERS_OF` names: watched from the window's
/// start (or at once, in it), up to [`MOST_OF`] addresses a binding.
#[cfg_attr(not(any(test, feature = "diagnostics")), allow(dead_code))]
pub(crate) fn bound(hash: &str, slot: u8, offset: u32, address: u64, len: u64) {
    let (address, len) = (address as usize, (len as usize).min(MOST_RANGE));
    if address == 0 || len == 0 {
        return;
    }
    let line = {
        let mut guard = state();
        let Some(st) = guard.as_mut() else {
            return;
        };
        if st.closed {
            return;
        }
        let Some(i) = st
            .config
            .of
            .iter()
            .position(|o| o.hash == hash && o.slot == slot && o.offset == offset)
        else {
            return;
        };
        if st.found[i].contains(&address) || st.found[i].len() >= MOST_OF {
            return;
        }
        st.found[i].push(address);
        let n = st.found[i].len();
        let origin = format!("{hash}:s{slot}+{offset} #{n}");
        let line = format!(
            "writers: {origin} is title memory {address:#x}+{len:#x} (frame {}, {:.1} s){}",
            crate::frame::current(),
            st.started.elapsed().as_secs_f64(),
            if st.open {
                ""
            } else {
                "; watched from the window's start"
            }
        );
        st.ranges.push(Range {
            start: address,
            len,
            origin,
            slots: Vec::new(),
            armed: false,
            tries: 0,
        });
        if st.found.iter().all(|f| f.len() >= MOST_OF) {
            LOOKING.store(false, SeqCst);
        }
        line
    };
    tracing::info!("{line}");
}

/// The table, as it is.
pub(crate) struct Table {
    pub(crate) rows: Vec<(Key, Writer, String)>,
    pub(crate) elsewhere: Vec<(usize, Elsewhere)>,
    steps: u64,
    dropped: u64,
    ranges: usize,
    frame: u64,
}

fn snapshot(st: &State) -> Table {
    let mut rows: Vec<(Key, Writer, String)> = st
        .writers
        .iter()
        .map(|(k, w)| {
            (
                *k,
                *w,
                st.ranges
                    .get(usize::from(k.range))
                    .map_or_else(String::new, |r| r.origin.clone()),
            )
        })
        .collect();
    rows.sort_by_key(|(k, w, _)| (k.range, k.offset, std::cmp::Reverse(w.count), k.rip));
    let mut elsewhere: Vec<(usize, Elsewhere)> =
        st.elsewhere.iter().map(|(&rip, &e)| (rip, e)).collect();
    elsewhere.sort_by_key(|(rip, e)| (std::cmp::Reverse(e.count), *rip));
    Table {
        rows,
        elsewhere,
        steps: st.steps,
        dropped: st.dropped,
        ranges: st.ranges.len(),
        frame: crate::frame::current(),
    }
}

impl Table {
    fn lines(&self, why: &str) -> Vec<String> {
        let mut lines = vec![format!(
            "writers: table at {why}: {} writers of {} ranges; {} writes stepped, {} by {} instructions elsewhere in the watched pages, {} not kept; frame {}",
            self.rows.len(),
            self.ranges,
            self.steps,
            self.elsewhere.iter().map(|(_, e)| e.count).sum::<u64>(),
            self.elsewhere.len(),
            self.dropped,
            self.frame
        )];
        for (key, w, origin) in &self.rows {
            lines.push(format!("writers:   {}", row(key, w, origin)));
        }
        for (rip, e) in self.elsewhere.iter().take(16) {
            lines.push(format!(
                "writers:   elsewhere {} x{} (page {:#x})",
                name(*rip),
                e.count,
                e.page
            ));
        }
        lines
    }
}

/// A writer, in a line.
fn row(key: &Key, w: &Writer, origin: &str) -> String {
    let values: Vec<String> = (0..usize::from(w.distinct))
        .map(|i| shown(&w.values[i][..usize::from(w.value_len[i])]))
        .collect();
    let stack: Vec<String> = w.stack[..usize::from(w.stack_len)]
        .iter()
        .map(|&pc| name(pc as usize))
        .collect();
    let code = if key.kind == Kind::Landed {
        "-".to_owned()
    } else {
        name(key.rip)
    };
    format!(
        "{code} ({}) {origin}+{:#x} x{} size {} frames {}-{} ({:.1}-{:.1} s) values [{}]{} last {} ({} changes) thread {} {}{} stack [{}]{}",
        key.kind.name(),
        key.offset,
        w.count,
        if w.size == 0 {
            "?".to_owned()
        } else {
            w.size.to_string()
        },
        w.first_frame,
        w.last_frame,
        w.first_at.as_secs_f64(),
        w.last_at.as_secs_f64(),
        values.join(" | "),
        if w.covered > u32::from(w.size) && w.size != 0 {
            format!(" covering {} bytes", w.covered)
        } else {
            String::new()
        },
        if w.valued {
            shown(&w.last[..usize::from(w.last_len)])
        } else {
            "?".into()
        },
        w.changes,
        w.thread,
        thread_name(w.thread),
        if w.last_thread != w.thread {
            format!(" (last {})", w.last_thread)
        } else {
            String::new()
        },
        stack.join(" < "),
        if w.other_stacks != 0 {
            format!(" and {} writes from other stacks", w.other_stacks)
        } else {
            String::new()
        },
    )
}

/// Bytes as little-endian words, with their meaning as floats.
fn shown(bytes: &[u8]) -> String {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(4) {
        return bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect();
    let hex: Vec<String> = words.iter().map(|w| format!("{w:08x}")).collect();
    let floats: Vec<String> = words
        .iter()
        .map(|&w| format!("{}", f32::from_bits(w)))
        .collect();
    format!("{} ({})", hex.join(" "), floats.join(", "))
}

/// A thread's name, as `"name"` (nothing if it has none or is gone).
fn thread_name(id: u32) -> String {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::System::Threading::{
        GetThreadDescription, OpenThread, THREAD_QUERY_LIMITED_INFORMATION,
    };
    static NAMES: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let names = names.get_or_insert_with(HashMap::new);
    if let Some(known) = names.get(&id) {
        return known.clone();
    }
    // SAFETY: opens a thread of this process, if it is there, to read its
    // name, which the system allocates and we free.
    let text = unsafe {
        let thread = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, id);
        if thread.is_null() {
            return String::new();
        }
        let mut name = std::ptr::null_mut();
        let text = if GetThreadDescription(thread, &mut name) >= 0 && !name.is_null() {
            let len = (0..).take_while(|&i| *name.add(i) != 0).count();
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(name, len));
            LocalFree(name.cast());
            text
        } else {
            String::new()
        };
        CloseHandle(thread);
        text
    };
    let text = if text.is_empty() {
        String::new()
    } else {
        format!("\"{text}\"")
    };
    names.insert(id, text.clone());
    text
}

/// One line for each writer seen since the last look, with its first value.
fn newly_seen() -> Vec<String> {
    let new: Vec<(Key, Writer, String)> = {
        let mut guard = state();
        let Some(st) = guard.as_mut() else {
            return Vec::new();
        };
        let origins: Vec<String> = st.ranges.iter().map(|r| r.origin.clone()).collect();
        st.writers
            .iter_mut()
            .filter(|(_, w)| !w.said && w.valued)
            .map(|(k, w)| {
                w.said = true;
                (
                    *k,
                    *w,
                    origins
                        .get(usize::from(k.range))
                        .cloned()
                        .unwrap_or_default(),
                )
            })
            .collect()
    };
    new.iter()
        .map(|(k, w, origin)| format!("writers: new writer {}", row(k, w, origin)))
        .collect()
}

/// The words of `[start, start + len)` (the first [`DUMP_WORDS`]), as lines.
fn dump(start: usize, len: usize, origin: &str, why: &str) -> Vec<String> {
    let n = len.min(DUMP_WORDS * 4) & !3;
    let mut bytes = vec![0u8; n];
    if n == 0 || !crate::host::read_into(start as *const u8, &mut bytes) {
        return vec![format!(
            "writers: {origin} at {start:#x} cannot be read at {why}"
        )];
    }
    let mut lines = vec![format!(
        "writers: words of {origin} at {start:#x} at {why} (frame {}): {} of {}",
        crate::frame::current(),
        n / 4,
        len / 4
    )];
    for (i, row) in bytes.chunks(32).enumerate() {
        let words: Vec<String> = row
            .chunks_exact(4)
            .map(|w| format!("{:08x}", u32::from_le_bytes([w[0], w[1], w[2], w[3]])))
            .collect();
        lines.push(format!(
            "writers:   {origin} +{:#05x}: {}",
            i * 32,
            words.join(" ")
        ));
    }
    lines
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READONLY, PAGE_READWRITE, VirtualAlloc,
        VirtualFree,
    };

    /// One test at a time: the watch is the process's.
    static ONE: Mutex<()> = Mutex::new(());

    fn config(ranges: Vec<(usize, usize)>, detail: u32) -> Config {
        Config {
            ranges,
            of: Vec::new(),
            from: Duration::ZERO,
            span: None,
            detail,
            frames: 4,
        }
    }

    /// Starts a watch of `config` and opens its window; the guard to hold.
    fn watching(config: Config) -> std::sync::MutexGuard<'static, ()> {
        let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let _ = UNWINDER.set(|context, out| {
            out[0] = context.Rip;
            out[1] = 0x1234;
            2
        });
        start(config);
        assert!(tick(Duration::ZERO));
        one
    }

    /// Ends the watch: its pages let go, the state gone.
    fn done() {
        disarm_all();
        *state() = None;
    }

    fn table() -> Table {
        snapshot(state().as_ref().unwrap())
    }

    fn alloc(bytes: usize) -> usize {
        // SAFETY: a fresh allocation, freed by the test.
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                bytes,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        } as usize;
        assert_ne!(p, 0);
        p
    }

    fn free(p: usize) {
        // SAFETY: our allocation.
        unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
    }

    fn protection(at: usize) -> u32 {
        use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer.
        unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
        info.Protect
    }

    /// Stores `value` at `at`: one 4-byte store, in this function's code
    /// (a `write_volatile` would be a call of its own in a debug build).
    #[inline(never)]
    fn store(at: usize, value: u32) {
        // SAFETY: memory of the test's; the store changes nothing else.
        unsafe {
            std::arch::asm!("mov dword ptr [{0}], {1:e}", in(reg) at, in(reg) value, options(nostack))
        };
    }

    #[test]
    fn switches_are_read() {
        let get = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(Config::from_switches(get(&[])).0, None);
        let (c, bad) = Config::from_switches(get(&[
            (
                "VICTORY_WRITERS",
                "0x956fdc200+800, 956fdca00+0x800,junk,1+20000",
            ),
            ("VICTORY_WRITERS_OF", "6318F669E4F17D65:s1+320,abc:s0+1"),
            ("VICTORY_WRITERS_FROM", "50"),
            ("VICTORY_WRITERS_FOR", "20.5"),
            ("VICTORY_WRITERS_STACK", "99"),
        ]));
        let c = c.unwrap();
        assert_eq!(
            c.ranges,
            [(0x956fdc200, 0x800), (0x956fdca00, 0x800), (1, MOST_RANGE)]
        );
        assert_eq!(
            c.of,
            [Of {
                hash: "6318f669e4f17d65".into(),
                slot: 1,
                offset: 320
            }]
        );
        assert_eq!(c.from, Duration::from_secs(50));
        assert_eq!(c.span, Some(Duration::from_millis(20_500)));
        assert_eq!((c.detail, c.frames), (64, FRAMES));
        assert_eq!(bad.len(), 3, "{bad:?}");
    }

    #[test]
    fn string_stores_are_known() {
        assert_eq!(string_store(&[0xf3, 0xa4]), Some((true, 1)));
        assert_eq!(string_store(&[0xf3, 0x48, 0xab]), Some((true, 8)));
        assert_eq!(string_store(&[0x66, 0xab]), Some((false, 2)));
        assert_eq!(string_store(&[0x89, 0x01]), None);
    }

    /// A write into a watched range is told with its instruction, offset,
    /// size and value, as often as it was made, and is there afterwards.
    #[test]
    fn a_write_in_a_watched_range_is_told_and_done() {
        let p = alloc(2 * SMALL);
        let at = p + 0xc00;
        let _one = watching(config(vec![(at, 0x800)], 64));
        assert_eq!(protection(p), PAGE_READONLY);
        assert_eq!(
            protection(p + SMALL),
            PAGE_READONLY,
            "the range's second page"
        );
        for i in 0..3u32 {
            store(at + 0x17c, 0x3f80_0000 + i);
        }
        // SAFETY: our memory.
        assert_eq!(
            unsafe { ((at + 0x17c) as *const u32).read() },
            0x3f80_0002,
            "the write was done"
        );
        assert_eq!(protection(p), PAGE_READONLY, "read-only again");
        let t = table();
        assert_eq!(
            t.rows.len(),
            1,
            "{:?}",
            t.rows.iter().map(|r| r.0).collect::<Vec<_>>()
        );
        let (key, w, _) = &t.rows[0];
        assert_eq!((key.kind, key.offset, key.range), (Kind::Code, 0x17c, 0));
        let code = store as fn(usize, u32) as usize;
        assert!(
            (code..code + 256).contains(&key.rip),
            "{:#x} vs {code:#x}",
            key.rip
        );
        assert_eq!((w.count, w.size, w.distinct, w.changes), (3, 4, 3, 2));
        assert_eq!(w.last[..4], 0x3f80_0002u32.to_le_bytes());
        assert_eq!(w.thread, thread_id());
        assert_eq!(&w.stack[..2], &[key.rip as u64, 0x1234], "the stack");
        let lines = newly_seen();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("+0x17c x3 size 4"), "{}", lines[0]);
        done();
        assert_eq!(protection(p), PAGE_READWRITE, "let go");
        store(at, 7);
        free(p);
    }

    /// A string store across both pages of a range is stepped to its end:
    /// all its bytes are there, and it is told once, with what it covered.
    #[test]
    fn a_string_store_across_pages_is_stepped_to_its_end() {
        let p = alloc(2 * SMALL);
        let at = p + SMALL - 0x40;
        let _one = watching(config(vec![(at, 0x80)], 64));
        // SAFETY: 0x80 bytes of our memory; `rep stosb` with the direction
        // flag clear, as the ABI keeps it.
        unsafe {
            std::arch::asm!("rep stosb", inout("rdi") at => _, inout("rcx") 0x80usize => _, in("al") 0xabu8, options(nostack));
        }
        // SAFETY: our memory.
        assert!(
            unsafe { std::slice::from_raw_parts(at as *const u8, 0x80) }
                .iter()
                .all(|&b| b == 0xab)
        );
        assert_eq!(
            (protection(p), protection(p + SMALL)),
            (PAGE_READONLY, PAGE_READONLY)
        );
        let t = table();
        assert_eq!(t.rows.len(), 1);
        let (key, w, _) = &t.rows[0];
        assert_eq!((key.offset, w.count, w.size, w.covered), (0, 1, 1, 0x80));
        assert_eq!(w.last, [0xab; VALUE]);
        done();
        free(p);
    }

    /// A page shown at two addresses is watched at both: a write through
    /// the other is told at the range's offset.
    #[test]
    fn a_write_through_another_view_is_told() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            UnmapViewOfFile,
        };
        // SAFETY: a fresh pagefile section mapped twice, told to the runtime's
        // record of views as the runtime does.
        let (section, one, two) = unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                BLOCK as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let one = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, BLOCK).Value as usize;
            let two = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, BLOCK).Value as usize;
            assert!(one != 0 && two != 0);
            crate::inplace::title_memory_view(one, BLOCK, section as usize, 0);
            crate::inplace::title_memory_view(two, BLOCK, section as usize, 0);
            (section, one, two)
        };
        let _one = watching(config(vec![(one + 2 * SMALL + 0x100, 0x40)], 64));
        assert_eq!(
            protection(two + 2 * SMALL),
            PAGE_READONLY,
            "the other view too"
        );
        store(two + 2 * SMALL + 0x108, 77);
        // SAFETY: our view.
        assert_eq!(
            unsafe { ((one + 2 * SMALL + 0x108) as *const u32).read() },
            77
        );
        let t = table();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(
            (t.rows[0].0.offset, t.rows[0].1.last[..4].to_vec()),
            (8, 77u32.to_le_bytes().to_vec())
        );
        done();
        assert_eq!(protection(two + 2 * SMALL), PAGE_READWRITE);
        for view in [one, two] {
            crate::watch::title_memory_unmapping(view, BLOCK);
            // SAFETY: our views.
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                })
            };
        }
        // SAFETY: the section's last reference.
        unsafe { CloseHandle(section) };
    }

    /// A write elsewhere in a watched page is done and counted only.
    #[test]
    fn a_write_beside_a_watched_range_is_counted() {
        let p = alloc(SMALL);
        let _one = watching(config(vec![(p + 0x800, 0x100)], 64));
        store(p + 0x10, 5);
        store(p + 0x14, 6);
        // SAFETY: our memory.
        assert_eq!(unsafe { ((p + 0x14) as *const u32).read() }, 6);
        let t = table();
        assert!(t.rows.is_empty());
        assert_eq!(t.elsewhere.len(), 1);
        assert_eq!(t.elsewhere[0].1.count, 2);
        assert_eq!(t.elsewhere[0].1.page, p);
        done();
        free(p);
    }

    /// Past the detail asked for, writes are still counted, with their
    /// values, but no more stacks are taken.
    #[test]
    fn detail_stops_and_counting_goes_on() {
        let p = alloc(SMALL);
        let _one = watching(config(vec![(p, 0x100)], 2));
        for i in 0..10 {
            store(p + 8, i);
        }
        let t = table();
        let (_, w, _) = &t.rows[0];
        assert_eq!((w.count, w.detailed), (10, 2));
        assert_eq!(w.last[..4], 9u32.to_le_bytes());
        assert_eq!(w.distinct as usize, VALUES, "the first values kept");
        assert_eq!(w.changes, 9);
        done();
        free(p);
    }

    /// A file read into a watched range lands (the page is opened for the
    /// kernel) and is told as the kernel's.
    #[test]
    fn a_kernel_read_into_a_watched_range_lands_and_is_told() {
        use std::io::{Read, Seek, Write};
        let p = alloc(SMALL);
        let _one = watching(config(vec![(p + 0x100, 0x100)], 64));
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"tuned values!!!!").unwrap();
        let read = |file: &mut std::fs::File| {
            file.rewind().unwrap();
            // SAFETY: 16 bytes of our memory, nothing else refers to them.
            let into = unsafe { std::slice::from_raw_parts_mut((p + 0x120) as *mut u8, 16) };
            file.read(into).is_ok_and(|n| n == 16)
        };
        assert!(!read(&mut file), "read-only to the kernel while watched");
        {
            let _writing = crate::watch::kernel_writing(p + 0x120, 16);
            assert!(read(&mut file));
        }
        assert_eq!(
            protection(p),
            PAGE_READONLY,
            "read-only again after the write"
        );
        // SAFETY: the bytes just read.
        assert_eq!(
            unsafe { std::slice::from_raw_parts((p + 0x120) as *const u8, 16) },
            b"tuned values!!!!"
        );
        let t = table();
        let kernel: Vec<_> = t.rows.iter().filter(|r| r.0.kind == Kind::Kernel).collect();
        assert_eq!(kernel.len(), 1);
        assert_eq!(
            (kernel[0].0.offset, kernel[0].1.size, kernel[0].1.count),
            (0x20, 16, 1)
        );
        assert_eq!(&kernel[0].1.last, b"tuned values!!!!");
        done();
        free(p);
    }

    /// A watched page in a block the guards hold: the write faults into us
    /// first, and the guards still see their block written.
    #[test]
    fn the_guards_still_see_the_write() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            UnmapViewOfFile,
        };
        // SAFETY: a pagefile section of one block, mapped read-write.
        let (section, view) = unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                BLOCK as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (
                section,
                MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, BLOCK).Value as usize,
            )
        };
        assert_ne!(view, 0);
        let block = view / BLOCK;
        assert_eq!(
            crate::watch::guard_now(view, view + BLOCK),
            Some(vec![block])
        );
        assert_eq!(
            crate::watch::guard_now(view, view + BLOCK),
            Some(Vec::new()),
            "clean"
        );
        let _one = watching(config(vec![(view + 3 * SMALL, 0x40)], 64));
        store(view + 3 * SMALL + 4, 42);
        // SAFETY: our view.
        assert_eq!(unsafe { ((view + 3 * SMALL + 4) as *const u32).read() }, 42);
        assert_eq!(protection(view + 3 * SMALL), PAGE_READONLY, "watched again");
        assert_eq!(
            protection(view),
            PAGE_READWRITE,
            "the guards opened their block"
        );
        assert_eq!(
            crate::watch::guard_now(view, view + BLOCK),
            Some(vec![block]),
            "the guards saw it"
        );
        // Guarded again: the watched page is read-only either way, and a
        // write still reaches both.
        store(view + 3 * SMALL + 4, 43);
        assert_eq!(
            crate::watch::guard_now(view, view + BLOCK),
            Some(vec![block])
        );
        assert_eq!(table().rows[0].1.count, 2);
        done();
        // The guards hold the block again: it stays read-only for them.
        assert_eq!(protection(view + 3 * SMALL), PAGE_READONLY);
        store(view + 3 * SMALL + 4, 44);
        assert_eq!(
            crate::watch::guard_now(view, view + BLOCK),
            Some(vec![block])
        );
        crate::watch::title_memory_unmapping(view, BLOCK);
        // SAFETY: our view and section.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: view as *mut _,
            });
            CloseHandle(section);
        }
    }

    /// The window: nothing is read-only before it opens or after it
    /// closes, and the range's words are told at both ends.
    #[test]
    fn the_window_arms_and_disarms() {
        let p = alloc(SMALL);
        let mut c = config(vec![(p, 0x40)], 64);
        c.from = Duration::from_secs(10);
        c.span = Some(Duration::from_secs(5));
        let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        start(c);
        assert!(tick(Duration::from_secs(1)));
        assert_eq!(protection(p), PAGE_READWRITE, "before the window");
        store(p, 1);
        assert!(tick(Duration::from_secs(10)));
        assert_eq!(protection(p), PAGE_READONLY, "in the window");
        store(p, 2);
        assert!(!tick(Duration::from_secs(15)), "the window is over");
        assert_eq!(protection(p), PAGE_READWRITE, "after the window");
        store(p, 3);
        assert_eq!(table().rows[0].1.count, 1, "only the write in the window");
        *state() = None;
        drop(one);
        free(p);
    }

    /// A range not there yet when the window opens is watched once it is.
    #[test]
    fn a_range_is_watched_once_it_is_there() {
        // SAFETY: a fresh reservation, committed below, freed at the end.
        let p =
            unsafe { VirtualAlloc(std::ptr::null(), SMALL, MEM_RESERVE, PAGE_READWRITE) } as usize;
        assert_ne!(p, 0);
        let _one = watching(config(vec![(p + 0x10, 0x10)], 64));
        assert!(table().rows.is_empty());
        // SAFETY: commits our reservation.
        assert_ne!(
            unsafe { VirtualAlloc(p as *const _, SMALL, MEM_COMMIT, PAGE_READWRITE) } as usize,
            0
        );
        assert!(tick(Duration::from_secs(1)));
        assert_eq!(protection(p), PAGE_READONLY);
        store(p + 0x10, 1);
        assert_eq!(table().rows.len(), 1);
        done();
        free(p);
    }

    /// A binding found by a draw is watched from the window's start.
    #[test]
    fn a_binding_found_is_watched() {
        let p = alloc(SMALL);
        let mut c = config(Vec::new(), 64);
        c.of = vec![Of {
            hash: "6318f669e4f17d65".into(),
            slot: 0,
            offset: 48,
        }];
        c.from = Duration::from_secs(1);
        let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        start(c);
        assert!(looking());
        assert!(shader_wanted("6318f669e4f17d65") && binding_wanted("6318f669e4f17d65", 0, 48));
        assert!(!binding_wanted("6318f669e4f17d65", 1, 48));
        bound("6318f669e4f17d65", 0, 48, (p + 0x200) as u64, 0x800);
        bound("6318f669e4f17d65", 0, 48, (p + 0x200) as u64, 0x800);
        assert!(tick(Duration::ZERO));
        assert_eq!(protection(p), PAGE_READWRITE);
        assert!(tick(Duration::from_secs(2)));
        assert_eq!(protection(p), PAGE_READONLY);
        store(p + 0x204, 9);
        let t = table();
        assert_eq!((t.rows.len(), t.rows[0].0.offset), (1, 4));
        assert_eq!(t.rows[0].2, "6318f669e4f17d65:s0+48 #1");
        done();
        drop(one);
        free(p);
    }
}

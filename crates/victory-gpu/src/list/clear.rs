//! `ClearUnorderedAccessViewUint` (list slot 0x1a8) and
//! `ClearUnorderedAccessViewFloat` (0x1b0), as on PC:
//! `(gpu handle in the heap set, cpu handle, resource, values[4], rect
//! count, rects)`. The console's list keeps the PC order of the base list
//! here (targets 0x190, depth clear 0x198, target clear 0x1a0), and its
//! handles are addresses of view records, the CPU and the GPU one alike
//! (`crate::descriptor`). The arguments are taken as PC's: an 8-byte handle
//! in a register each, then the resource, then the stack. (inferred: the
//! slots and the argument order are not proven from the title's code; a
//! diagnostics build logs the first calls' raw arguments, see [`called`].)
//!
//! A clear is a write of the card, and is done where the card has the view
//! the clear names, at its place in the list, by the host's clear of the
//! same view:
//!
//! - of a texture: on its host copy (of a depth texture: on its colour
//!   twin, `HostList::twin_use`, which brings the twin up to date first), in
//!   the unordered access state;
//! - of a buffer, of buffer memory the title placed a view on, or of title
//!   memory outside buffers (a window): on the host copy the view's slot
//!   shows (`crate::descriptor::host::buffer_copy`), whether its bytes are
//!   in the card's memory (two sides) or the PC's. The range the view
//!   covers is first brought up to date, then declared written
//!   (`Shadow::gpu_writes`), as for a shader's unordered access view of it:
//!   no refresh copies title memory over it until the submission has
//!   landed, and what the card made of it (the cleared bytes) is put into
//!   title memory before the title is told the submission ran.
//!
//! The host's clear wants the view in the shader-visible heap the list has
//! set and again in a heap the card does not see: a dynamic slot of the host
//! heap behind the title's view heap ([`crate::draw::State::clear_slot`],
//! given back with the list's tables) and a slot of a ring of ours
//! (`HostHeap::make_for_clear`). The list waits for all unordered access
//! before and after the clear, as for any shader write next to it.
//!
//! Float values for a raw or structured buffer view go in as their bits (a
//! Uint clear; the host has no float clear of untyped views), and for a view
//! in an integer format as integers (inferred from what the host's float
//! clear does for the formats it takes).

use super::{Rect, This};

/// The values a clear writes, as the title passes them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Values {
    Uint([u32; 4]),
    Float([f32; 4]),
}

impl Values {
    /// The method's name.
    fn method(self) -> &'static str {
        match self {
            Values::Uint(_) => "ClearUnorderedAccessViewUint",
            Values::Float(_) => "ClearUnorderedAccessViewFloat",
        }
    }

    /// 0 for Uint, 1 for Float.
    fn which(self) -> usize {
        usize::from(matches!(self, Values::Float(_)))
    }
}

/// `count` rectangles at `rects`, or none.
fn rect_slice<'a>(count: u32, rects: *const Rect) -> &'a [Rect] {
    if count == 0 || rects.is_null() {
        return &[];
    }
    // SAFETY: the title passes `count` rectangles.
    unsafe { std::slice::from_raw_parts(rects, count as usize) }
}

/// List slot 0x1a8.
pub(super) extern "system" fn clear_uint(
    this: This,
    gpu: u64,
    cpu: u64,
    resource: This,
    values: *const [u32; 4],
    count: u32,
    rects: *const Rect,
) {
    // SAFETY: the title passes four values (null is tolerated).
    let Some(&values) = (unsafe { values.as_ref() }) else {
        return;
    };
    let values = Values::Uint(values);
    called(values, gpu, cpu, resource, count);
    record(this, gpu, cpu, resource, values, rect_slice(count, rects));
}

/// List slot 0x1b0.
pub(super) extern "system" fn clear_float(
    this: This,
    gpu: u64,
    cpu: u64,
    resource: This,
    values: *const [f32; 4],
    count: u32,
    rects: *const Rect,
) {
    // SAFETY: the title passes four values (null is tolerated).
    let Some(&values) = (unsafe { values.as_ref() }) else {
        return;
    };
    let values = Values::Float(values);
    called(values, gpu, cpu, resource, count);
    record(this, gpu, cpu, resource, values, rect_slice(count, rects));
}

/// At the title's call, before anything is made of it: in a diagnostics
/// build the first calls of each method are logged as they came (to prove
/// the arguments' order: a view record at a handle, a resource of ours, a
/// size and format that match the view).
pub(super) fn called(values: Values, gpu: u64, cpu: u64, resource: This, count: u32) {
    #[cfg(all(windows, feature = "diagnostics"))]
    {
        static CALLS: [std::sync::atomic::AtomicU32; 2] =
            [const { std::sync::atomic::AtomicU32::new(0) }; 2];
        if CALLS[values.which()].fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 16 {
            let record = |h: u64| {
                crate::descriptor::record_at(h).map(|w| format!("{:08x} {:08x}", w[1], w[2]))
            };
            tracing::info!(
                method = values.method(),
                gpu = format_args!("{gpu:#x}"),
                cpu = format_args!("{cpu:#x}"),
                gpu_record = ?record(gpu),
                cpu_record = ?record(cpu),
                view = ?crate::descriptor::view(cpu).or_else(|| crate::descriptor::view(gpu)),
                resource = ?resource,
                resource_is = %described(resource),
                values = ?values,
                bits = ?bits(values),
                rects = count,
                "an unordered access view clear's arguments as the title passed them"
            );
        }
    }
    #[cfg(not(all(windows, feature = "diagnostics")))]
    let _ = (values, gpu, cpu, resource, count);
}

/// The values' bits.
#[cfg(all(windows, feature = "diagnostics"))]
fn bits(values: Values) -> [u32; 4] {
    match values {
        Values::Uint(v) => v,
        Values::Float(v) => v.map(f32::to_bits),
    }
}

/// What `resource` is, for the log: a title buffer and its bytes, a title
/// texture and its size and format, or neither.
#[cfg(windows)]
fn described(resource: This) -> String {
    if resource.is_null() || !crate::resource::is_resource(resource) {
        return format!("not a resource of ours ({resource:?})");
    }
    if let Some((data, len)) = crate::resource::buffer_bytes(resource) {
        return format!("a buffer of {len} bytes at {data:?}");
    }
    match crate::resource::texture_memory(resource) {
        Some((_, desc)) => {
            // SAFETY: 56 bytes in the description's layout.
            let w = unsafe {
                std::ptr::from_ref(&desc)
                    .cast::<[u32; 14]>()
                    .read_unaligned()
            };
            format!(
                "a texture {}x{}x{} format {} levels {} flags {:#x}",
                u64::from(w[4]) | u64::from(w[5]) << 32,
                w[6],
                w[7] & 0xffff,
                w[8],
                w[7] >> 16,
                w[13]
            )
        }
        None => "a resource neither a buffer nor a texture".into(),
    }
}

/// Records the clear into the title list's host list (thin lists: when
/// their words are translated, with the view record as it was at the call).
pub(super) fn record(
    this: This,
    gpu: u64,
    cpu: u64,
    resource: This,
    values: Values,
    rects: &[Rect],
) {
    #[cfg(windows)]
    super::with_state(this, |list, state| {
        if let Err(why) = imp::clear(list, state, gpu, cpu, resource, values, rects) {
            imp::refused(values, gpu, cpu, resource, why);
        }
    });
    #[cfg(not(windows))]
    let _ = (this, gpu, cpu, resource, values, rects);
}

/// For the frame-rate line: the clears recorded per frame since the last
/// one, the bytes they wrote (about: of textures, the texels of the first
/// level or the rectangles, whatever levels and slices the view has), and
/// those refused.
#[cfg(windows)]
pub(crate) fn line(frames: f64) {
    use std::sync::atomic::Ordering::Relaxed;
    let [n, bytes, refused] = imp::COUNTS.each_ref().map(|c| c.swap(0, Relaxed));
    if n + refused > 0 {
        tracing::info!(
            "UAV clears per frame: {:.1}, {:.0} bytes ({:.1} refused, ClearUnorderedAccessView*)",
            n as f64 / frames,
            bytes as f64 / frames,
            refused as f64 / frames
        );
    }
}

#[cfg(windows)]
mod imp {
    use super::{Rect, This, Values};
    use crate::descriptor::View;
    use crate::draw::State;
    use crate::host::HostList;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Direct3D12::*;

    /// Since the last frame-rate line: clears recorded, their bytes, clears
    /// refused.
    pub(super) static COUNTS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

    /// Runs `f` the first time this place is reached.
    macro_rules! once {
        ($f:expr $(,)?) => {{
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once($f);
        }};
    }

    /// What a clear was made on, for the logs and the count.
    struct Made {
        what: &'static str,
        bytes: u64,
        format: u32,
        untyped: bool,
    }

    /// Records the clear of the view at `cpu` (or, if there is none there,
    /// at `gpu`), or says why not.
    #[allow(clippy::too_many_arguments)] // the method's own arguments, and where they go
    pub(super) fn clear(
        list: &HostList,
        state: &mut State,
        gpu: u64,
        cpu: u64,
        resource: This,
        values: Values,
        rects: &[Rect],
    ) -> Result<(), &'static str> {
        const UNORDERED_ACCESS: u32 = 0x8;
        let view = crate::descriptor::view(cpu)
            .or_else(|| crate::descriptor::view(gpu))
            .ok_or("no view recorded at either handle")?;
        // The view's description: format, dimension, then for a buffer the
        // first element (2), count (4), stride (5) and flags (8).
        let (title, words) = match &view {
            View::Unordered(r, _, desc) => (*r as This, *desc),
            View::Placed(true, _, desc) => (std::ptr::null(), desc[..10].try_into().ok()),
            _ => return Err("not an unordered access view"),
        };
        if !title.is_null() && !resource.is_null() && title != resource {
            once!(
                || tracing::warn!(?resource, view_of = ?title, "an unordered access view clear names another resource than its view's: the view's is cleared"),
            );
        }
        let buffer = crate::descriptor::host::buffer_copy(&view);
        if buffer.is_none() && crate::resource::buffer_bytes(title).is_some() {
            return Err("a buffer view without a host copy (an empty view, or no room)");
        }
        let (heap, index) = state.clear_slot(list)?;
        let (gpu_handle, cpu_handle, host) = heap
            .make_for_clear(index, &view)
            .ok_or("the host cannot make the view (logged where it is made)")?;
        // SAFETY: a live resource.
        let own = unsafe { host.GetDesc() };
        let view_format = words.map_or(0, |w| crate::resource::pc_format(w[0]));
        let made = match &buffer {
            Some((shadow, address, len)) => {
                if windows::core::Interface::as_raw(&shadow.buffer)
                    != windows::core::Interface::as_raw(&host)
                {
                    once!(|| {
                        tracing::warn!(
                            "an unordered access view clear's host view is of another copy than the one declared written: its bytes may not come back"
                        )
                    });
                }
                // What the title wrote around and under it so far goes
                // first; then the range is the card's until it lands.
                shadow.refresh(list, *address, *len);
                shadow.gpu_writes(list, *address, *len);
                // Indirect records clamped earlier may be of these bytes.
                state.copied();
                let untyped = words.is_some_and(|w| w[0] == 0 || w[5] != 0 || w[8] & 1 != 0);
                Made {
                    what: if shadow.carded() {
                        "a buffer (on the card)"
                    } else {
                        "a buffer"
                    },
                    bytes: *len,
                    format: view_format,
                    untyped,
                }
            }
            None => {
                if title.is_null() {
                    return Err("a view of a texture the title placed (not mirrored)");
                }
                list.texture_written(title);
                if let Some(texture) = crate::resource::host_resource(title) {
                    crate::texture::card_writes(list, title, texture, None);
                }
                crate::texture::host_written(title);
                // Unordered access to a depth texture is its colour twin's.
                let twin = list.twin_use(title, true, true);
                if !twin {
                    super::super::ensure_state(list, title, UNORDERED_ACCESS);
                }
                // A typed texture's views take its format (`fits`).
                let typed = {
                    let f = own.Format.0 as u32;
                    crate::resource::family(f) != f || crate::resource::family(f + 1) != f
                };
                let format = if typed {
                    own.Format.0 as u32
                } else {
                    view_format
                };
                let texel = crate::resource::element(format).map_or(4, |(bytes, _)| bytes);
                let area: u64 = if rects.is_empty() {
                    own.Width * u64::from(own.Height)
                } else {
                    rects
                        .iter()
                        .map(|r| {
                            u64::from(r.right.saturating_sub(r.left).max(0) as u32)
                                * u64::from(r.bottom.saturating_sub(r.top).max(0) as u32)
                        })
                        .sum()
                };
                Made {
                    what: if twin {
                        "a depth texture (its colour twin)"
                    } else {
                        "a texture"
                    },
                    bytes: area * texel,
                    format,
                    untyped: false,
                }
            }
        };
        let host_values = for_host(values, made.untyped, made.format);
        // SAFETY: the same layout.
        let host_rects: &[RECT] =
            unsafe { std::slice::from_raw_parts(rects.as_ptr().cast(), rects.len()) };
        #[cfg(feature = "diagnostics")]
        list.note_op(|| {
            format!(
                "{} of {:?}: {} {}x{} f{}, values {values:?}, {} rects",
                values.method(),
                title,
                made.what,
                own.Width,
                own.Height,
                made.format,
                rects.len()
            )
        });
        // SAFETY: an open list; the view was just made in the heap the list
        // has set and in ours, of `host`, a live resource the list keeps.
        unsafe {
            list.list.ResourceBarrier(&[every_write()]);
            match host_values {
                Values::Uint(v) => list
                    .list
                    .ClearUnorderedAccessViewUint(gpu_handle, cpu_handle, &host, &v, host_rects),
                Values::Float(v) => list
                    .list
                    .ClearUnorderedAccessViewFloat(gpu_handle, cpu_handle, &host, &v, host_rects),
            }
            list.list.ResourceBarrier(&[every_write()]);
        }
        list.keep(&host);
        // A depth texture's twin now holds what it lacks.
        list.twins_written();
        list.changed_something();
        COUNTS[0].fetch_add(1, Relaxed);
        COUNTS[1].fetch_add(made.bytes, Relaxed);
        first(values, host_values, &made, &own, rects.len());
        Ok(())
    }

    /// Values as the host takes them for a view in `format` (`untyped`: raw
    /// or structured): floats as bits for untyped views, as integers for
    /// integer formats.
    pub(super) fn for_host(values: Values, untyped: bool, format: u32) -> Values {
        match values {
            Values::Uint(_) => values,
            Values::Float(v) if untyped => Values::Uint(v.map(f32::to_bits)),
            Values::Float(v) => match integer(format) {
                Some(false) => Values::Uint(v.map(|x| x as u32)),
                Some(true) => Values::Uint(v.map(|x| x as i32 as u32)),
                None => values,
            },
        }
    }

    /// Whether a DXGI format is an integer one: signed or not.
    fn integer(format: u32) -> Option<bool> {
        match format {
            3 | 7 | 12 | 17 | 25 | 30 | 36 | 42 | 50 | 57 | 62 => Some(false),
            4 | 8 | 14 | 18 | 32 | 38 | 43 | 52 | 59 | 64 => Some(true),
            _ => None,
        }
    }

    /// A barrier after which every unordered access before it is done.
    fn every_write() -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                    pResource: std::mem::ManuallyDrop::new(None),
                }),
            },
        }
    }

    /// The first clear each method made, logged.
    fn first(values: Values, host: Values, made: &Made, own: &D3D12_RESOURCE_DESC, rects: usize) {
        static FIRST: [std::sync::Once; 2] = [const { std::sync::Once::new() }; 2];
        FIRST[values.which()].call_once(|| {
            tracing::info!(
                method = values.method(),
                of = made.what,
                bytes = made.bytes,
                width = own.Width,
                height = own.Height,
                format = made.format,
                values = ?values,
                as_host_takes_them = ?host,
                rects,
                "first unordered access view clear"
            );
        });
    }

    /// A clear that is not made: counted, and the first 16 logged.
    pub(super) fn refused(values: Values, gpu: u64, cpu: u64, resource: This, why: &str) {
        static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        COUNTS[2].fetch_add(1, Relaxed);
        if LOGGED.fetch_add(1, Relaxed) < 16 {
            tracing::warn!(
                method = values.method(),
                gpu = format_args!("{gpu:#x}"),
                cpu = format_args!("{cpu:#x}"),
                resource_is = %super::described(resource),
                why,
                "an unordered access view clear is left out"
            );
        }
    }
}

#[cfg(all(test, windows))]
mod tests;

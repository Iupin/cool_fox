//! Clears through the title's objects, as the title makes them: a list, a
//! shader-visible view heap with the view, a queue that runs the list. The
//! tests run without the relay (`cfg(test)`): each submission is waited for
//! and what it wrote is copied back before the queue's call returns.

use super::super::{ALLOCATOR_IIDS, LIST_IIDS, Rect, create_allocator, create_list};
use super::Values;
use super::imp::for_host;
use crate::object::{HRESULT, S_OK, This, guid, slot};
use windows::Win32::Graphics::Direct3D12::*;

type Release = extern "system" fn(This) -> u32;
type Heaps = extern "system" fn(This, u32, *const This);
type ClearUint = extern "system" fn(This, u64, u64, This, *const [u32; 4], u32, *const Rect);
type ClearFloat = extern "system" fn(This, u64, u64, This, *const [f32; 4], u32, *const Rect);

/// A title queue, list (open) and shader-visible view heap of 16 slots.
struct Title {
    queue: This,
    allocator: This,
    list: This,
    heap: This,
    start: u64,
}

impl Title {
    fn new() -> Title {
        let null = std::ptr::null();
        let queue_iid = guid(
            0x0ec870a6,
            0x5d7e,
            0x4c22,
            [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed],
        );
        let heap_iid = guid(
            0x8efb471d,
            0x616c,
            0x4f49,
            [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51],
        );
        let (mut queue, mut allocator, mut list, mut heap) = (null, null, null, null);
        let queue_desc = [0u32; 4];
        assert_eq!(
            crate::queue::create(null, queue_desc.as_ptr().cast(), &queue_iid, &mut queue),
            S_OK
        );
        assert_eq!(
            create_allocator(null, 0, &ALLOCATOR_IIDS[0], &mut allocator),
            S_OK
        );
        assert_eq!(
            create_list(null, 0, 0, allocator, null, &LIST_IIDS[1], &mut list),
            S_OK
        );
        // Views, 16 of them, shader-visible; then no placement.
        let desc = [0u32, 16, 1, 0, 0, 0, 0, 0];
        assert_eq!(
            crate::descriptor::create_heap(null, desc.as_ptr().cast(), &heap_iid, &mut heap),
            S_OK
        );
        let start = slot::<extern "system" fn(This) -> u64>(heap, 0x48)(heap);
        slot::<Heaps>(list, 0x100)(list, 1, &heap);
        Title {
            queue,
            allocator,
            list,
            heap,
            start,
        }
    }

    /// The handle of view slot `i`.
    fn handle(&self, i: u64) -> u64 {
        self.start + 32 * i
    }

    /// Records an unordered access view of `resource` (`words`: its
    /// description) in slot `i`.
    fn view(&self, i: u64, resource: This, words: [u32; 10]) {
        crate::descriptor::create_uav(
            std::ptr::null(),
            resource,
            std::ptr::null(),
            &words,
            self.handle(i),
        );
    }

    fn uint(&self, i: u64, resource: This, values: [u32; 4], rects: &[Rect]) {
        let h = self.handle(i);
        slot::<ClearUint>(self.list, 0x1a8)(
            self.list,
            h,
            h,
            resource,
            &values,
            rects.len() as u32,
            rects.as_ptr(),
        );
    }

    fn float(&self, i: u64, resource: This, values: [f32; 4], rects: &[Rect]) {
        let h = self.handle(i);
        slot::<ClearFloat>(self.list, 0x1b0)(
            self.list,
            h,
            h,
            resource,
            &values,
            rects.len() as u32,
            rects.as_ptr(),
        );
    }

    /// Closes the list, runs it (and waits for it to land), and opens it
    /// again.
    fn run(&self) {
        assert_eq!(
            slot::<extern "system" fn(This) -> HRESULT>(self.list, 0x48)(self.list),
            S_OK
        );
        slot::<extern "system" fn(This, u32, *const This)>(self.queue, 0x50)(
            self.queue, 1, &self.list,
        );
        // With the relay (`VICTORY_RELAY=2`, and `VICTORY_THIN=1`, in a
        // process of their own): until the submission is sent and landed.
        crate::queue::before_sync();
        crate::host::settle_for_fence();
        let reset = slot::<extern "system" fn(This, This, This) -> HRESULT>(self.list, 0x50);
        assert_eq!(reset(self.list, self.allocator, std::ptr::null()), S_OK);
        slot::<Heaps>(self.list, 0x100)(self.list, 1, &self.heap);
    }
}

impl Drop for Title {
    fn drop(&mut self) {
        assert_eq!(
            slot::<extern "system" fn(This) -> HRESULT>(self.list, 0x48)(self.list),
            S_OK
        );
        for object in [self.list, self.allocator, self.heap, self.queue] {
            slot::<Release>(object, 0x10)(object);
        }
    }
}

/// A raw view of `count` dwords from dword `first`.
fn raw(first: u32, count: u32) -> [u32; 10] {
    const R32_TYPELESS: u32 = 39;
    const BUFFER: u32 = 1;
    [R32_TYPELESS, BUFFER, first, 0, count, 0, 0, 0, 1, 0]
}

/// A 2D view in `format` of level 0.
fn flat(format: u32) -> [u32; 10] {
    const TEXTURE_2D: u32 = 4;
    [format, TEXTURE_2D, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// With the debug layer (`VICTORY_D3D12_DEBUG=1`, diagnostics): what it
/// said of the clears.
fn said() {
    if let Some(said) = crate::host::host().and_then(|h| h.log_messages()) {
        eprintln!("{said}");
    }
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    Rect {
        left,
        top,
        right,
        bottom,
    }
}

/// The dwords of title memory `bytes` long at `at`.
fn dwords(at: *mut u8, bytes: usize) -> &'static mut [u32] {
    // SAFETY: a title buffer of that many bytes, aligned, which the test owns.
    unsafe { std::slice::from_raw_parts_mut(at.cast::<u32>(), bytes / 4) }
}

/// A buffer cleared through raw views comes back into title memory, the
/// bytes outside the views as they were, the later of two clears over the
/// same bytes standing, floats as their bits; and so for one whose host copy
/// is on the card (two sides) where the budget gives it one.
#[test]
fn a_cleared_buffer_reaches_title_memory() {
    if crate::host::host().is_none() {
        return; // no host GPU here
    }
    for len in [4096usize, 16 << 20] {
        let t = Title::new();
        let buffer = crate::resource::test_buffer(len as u64);
        let (data, _) = crate::resource::buffer_bytes(buffer).unwrap();
        let words = dwords(data, len);
        words.fill(0x1111_1111);
        // Dwords 16..272; within them 100..110 again; and 300..310 as floats.
        t.view(0, buffer, raw(16, 256));
        t.view(1, buffer, raw(100, 10));
        t.view(2, buffer, raw(300, 10));
        t.uint(0, buffer, [0xdead_beef, 1, 2, 3], &[]);
        t.uint(1, buffer, [7, 0, 0, 0], &[]);
        t.float(2, buffer, [1.5, 0.0, 0.0, 0.0], &[]);
        t.run();
        let carded = crate::shadow::find(data as u64, len as u64).is_some_and(|s| s.carded());
        eprintln!("a buffer of {len} bytes, its host copy on the card: {carded}");
        let words = dwords(data, len);
        for (i, &w) in words.iter().enumerate().take(400) {
            let want = match i {
                100..110 => 7,
                16..272 => 0xdead_beef,
                300..310 => 1.5f32.to_bits(),
                _ => 0x1111_1111,
            };
            assert_eq!(w, want, "dword {i} of {len} bytes");
        }
        assert!(
            words[400..].iter().all(|&w| w == 0x1111_1111),
            "{len} bytes"
        );
        // The title writes beside the cleared bytes; the next submission's
        // refresh takes that, and leaves what the clear put there.
        words[20] = 5;
        words[0] = 6;
        t.uint(1, buffer, [8, 0, 0, 0], &[]);
        t.run();
        let words = dwords(data, len);
        assert_eq!(
            (words[0], words[20], words[16], words[100], words[300]),
            (6, 5, 0xdead_beef, 8, 1.5f32.to_bits())
        );
        drop(t);
        slot::<Release>(buffer, 0x10)(buffer);
        said();
    }
}

/// The texels of level 0 of a title texture of 4-byte texels, from its host
/// copy, row by row.
fn texels(resource: This, width: usize, height: usize) -> Vec<u32> {
    use windows::core::Interface;
    let host = crate::host::host().unwrap();
    let texture = crate::resource::host_resource(resource).unwrap();
    let (back, data) = host.shared_buffer(256 * height as u64).unwrap();
    let list = host.new_list().unwrap();
    let footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
        Offset: 0,
        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
            // SAFETY: a live texture.
            Format: unsafe { texture.GetDesc() }.Format,
            Width: width as u32,
            Height: height as u32,
            Depth: 1,
            RowPitch: 256,
        },
    };
    #[repr(C)]
    struct Location(
        *mut std::ffi::c_void,
        i32,
        D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    );
    let to = Location(back.as_raw(), 1, footprint);
    let from = Location(
        texture.as_raw(),
        0,
        D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(),
    );
    // SAFETY: an open list, live resources; locations in the host's layout.
    unsafe {
        if let Some((_, before)) = crate::resource::host_transition(resource, 0x800) {
            list.list.ResourceBarrier(&[crate::host::transition(
                texture,
                D3D12_RESOURCE_STATES(before as i32),
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
        }
        list.list.CopyTextureRegion(
            (&raw const to).cast(),
            0,
            0,
            0,
            (&raw const from).cast(),
            None,
        );
    }
    list.close();
    host.execute(&[&list]);
    (0..height)
        .flat_map(|y| {
            // SAFETY: the mapping holds `height` rows of 256 bytes; the copy has run.
            unsafe { std::slice::from_raw_parts(data.add(256 * y).cast::<u32>(), width) }.to_vec()
        })
        .collect()
}

/// Textures cleared whole and in a rectangle, by Uint and by Float, and a
/// Float clear of an integer texture (its values as integers).
#[test]
fn a_cleared_texture_holds_the_values() {
    if crate::host::host().is_none() {
        return; // no host GPU here
    }
    const UNORDERED: u32 = 4;
    const R32_FLOAT: u32 = 41;
    const R32_UINT: u32 = 42;
    let t = Title::new();
    let ints = crate::resource::test_texture(8, 8, R32_UINT, UNORDERED);
    let floats = crate::resource::test_texture(8, 8, R32_FLOAT, UNORDERED);
    t.view(0, ints, flat(R32_UINT));
    t.view(1, floats, flat(R32_FLOAT));
    t.uint(0, ints, [7, 0, 0, 0], &[]);
    t.uint(0, ints, [9, 0, 0, 0], &[rect(2, 2, 4, 4)]);
    t.float(1, floats, [0.25, 0.0, 0.0, 0.0], &[]);
    t.float(
        1,
        floats,
        [2.0, 0.0, 0.0, 0.0],
        &[rect(0, 0, 8, 1), rect(6, 6, 8, 8)],
    );
    t.run();
    let inside = |x: usize, y: usize, r: (usize, usize, usize, usize)| {
        (r.0..r.2).contains(&x) && (r.1..r.3).contains(&y)
    };
    let got = texels(ints, 8, 8);
    for (i, &v) in got.iter().enumerate() {
        let (x, y) = (i % 8, i / 8);
        assert_eq!(
            v,
            if inside(x, y, (2, 2, 4, 4)) { 9 } else { 7 },
            "uint texel {x},{y}"
        );
    }
    let got = texels(floats, 8, 8);
    for (i, &v) in got.iter().enumerate() {
        let (x, y) = (i % 8, i / 8);
        let two = inside(x, y, (0, 0, 8, 1)) || inside(x, y, (6, 6, 8, 8));
        assert_eq!(
            f32::from_bits(v),
            if two { 2.0 } else { 0.25 },
            "float texel {x},{y}"
        );
    }
    t.float(0, ints, [3.0, 0.0, 0.0, 0.0], &[rect(0, 0, 1, 1)]);
    t.run();
    assert_eq!(texels(ints, 8, 8)[..3], [3, 7, 7]);
    drop(t);
    for object in [ints, floats] {
        slot::<Release>(object, 0x10)(object);
    }
    said();
}

/// A depth texture shaders also write: the clear is of its colour twin, and
/// the depth texture has it once the list is closed.
#[test]
fn a_cleared_depth_texture_holds_the_values() {
    if crate::host::host().is_none() {
        return; // no host GPU here
    }
    const DEPTH_AND_UNORDERED: u32 = 2 | 4;
    const D32_FLOAT: u32 = 40;
    const R32_FLOAT: u32 = 41;
    let t = Title::new();
    let depth = crate::resource::test_texture(8, 8, D32_FLOAT, DEPTH_AND_UNORDERED);
    t.view(0, depth, flat(R32_FLOAT));
    t.float(0, depth, [0.75, 0.0, 0.0, 0.0], &[]);
    t.float(0, depth, [0.125, 0.0, 0.0, 0.0], &[rect(4, 0, 8, 8)]);
    t.run();
    let got = texels(depth, 8, 8);
    for (i, &v) in got.iter().enumerate() {
        let x = i % 8;
        assert_eq!(
            f32::from_bits(v),
            if x >= 4 { 0.125 } else { 0.75 },
            "texel {x},{}",
            i / 8
        );
    }
    drop(t);
    slot::<Release>(depth, 0x10)(depth);
    said();
}

#[test]
fn float_values_go_to_the_host_as_it_takes_them() {
    let v = Values::Float([1.5, -2.0, 3.7, 0.0]);
    assert_eq!(
        for_host(v, true, 39),
        Values::Uint([1.5f32.to_bits(), (-2.0f32).to_bits(), 3.7f32.to_bits(), 0])
    );
    assert_eq!(for_host(v, false, 42), Values::Uint([1, 0, 3, 0]));
    assert_eq!(
        for_host(v, false, 43),
        Values::Uint([1, -2i32 as u32, 3, 0])
    );
    assert_eq!(for_host(v, false, 41), v);
    assert_eq!(
        for_host(Values::Uint([1, 2, 3, 4]), true, 0),
        Values::Uint([1, 2, 3, 4])
    );
}

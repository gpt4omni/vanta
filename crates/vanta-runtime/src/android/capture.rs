//! Reading the frame the engine just presented, out of the swapchain itself.
//!
//! This exists because **nothing else on this host can photograph a Wayland
//! window.** Five routes were tried on 2026-08-21 and every one was refused:
//! `xprop`/`import` see nothing for a native Wayland client, GNOME's
//! `org.gnome.Shell.Screenshot` answers `AccessDenied`, `ffmpeg -f kmsgrab`
//! wants membership of the `video` group, the portal wants a human to click a
//! dialog, and no nested compositor is installable on an immutable host. So
//! every visual check ended with a person being asked to look at the window,
//! and a whole session's worth of bisecting stalled on that.
//!
//! Reading the swapchain sidesteps all of it, and gets a *better* answer than
//! any compositor route could: the image is whatever the engine drew, so it is
//! unaffected by occlusion, by the window being off-screen, by another window
//! covering it, or by the compositor's own colour management. A screenshot
//! taken here is what Roblox rendered rather than what the screen happened to
//! be showing.
//!
//! The copy runs inside `vkQueuePresentKHR`, *before* the frame is handed on,
//! because that is the one moment the image is complete and its layout is
//! known: the engine has finished rendering and has just transitioned it to
//! `VK_IMAGE_LAYOUT_PRESENT_SRC_KHR`. Everything allocated here is created and
//! destroyed per capture. That is deliberately wasteful -- a capture costs a
//! command pool, a buffer and a device-wide wait -- because a screenshot is
//! taken by a human or an agent a handful of times a run, and holding a
//! command pool and a mapped buffer alive for the whole session to save a
//! millisecond on an occasional call would be a permanent cost paid for a
//! rare benefit. It also keeps the failure mode clean: nothing here can leak
//! into a run that never takes a screenshot.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

// ---------------------------------------------------------------- constants
const VK_SUCCESS: i32 = 0;
const VK_TRUE: u32 = 1;
const ST_SUBMIT_INFO: u32 = 4;
const ST_MEMORY_ALLOCATE_INFO: u32 = 5;
const ST_FENCE_CREATE_INFO: u32 = 8;
const ST_SEMAPHORE_CREATE_INFO: u32 = 9;
const ST_BUFFER_CREATE_INFO: u32 = 12;
const ST_IMAGE_MEMORY_BARRIER: u32 = 45;
const ST_COMMAND_POOL_CREATE_INFO: u32 = 39;
const ST_COMMAND_BUFFER_ALLOCATE_INFO: u32 = 40;
const ST_COMMAND_BUFFER_BEGIN_INFO: u32 = 42;
/// Number of in-flight staging slots for async video readback. With 3 slots
/// we always wait on a fence submitted 2 frames ago rather than draining the
/// GPU pipeline on every recorded frame.
const RING_SLOTS: usize = 3;

const BUFFER_USAGE_TRANSFER_DST: u32 = 0x0000_0002;
const MEMORY_HOST_VISIBLE: u32 = 0x0000_0002;
const MEMORY_HOST_COHERENT: u32 = 0x0000_0004;
const CMD_BUFFER_ONE_TIME_SUBMIT: u32 = 0x0000_0001;
const POOL_TRANSIENT: u32 = 0x0000_0001;

const LAYOUT_PRESENT_SRC_KHR: i32 = 1_000_001_002;
const LAYOUT_TRANSFER_SRC_OPTIMAL: i32 = 6;
const ACCESS_TRANSFER_READ: u32 = 0x0000_0800;
const ACCESS_MEMORY_READ: u32 = 0x0000_8000;
const STAGE_TRANSFER: u32 = 0x0000_1000;
const STAGE_ALL_COMMANDS: u32 = 0x0001_0000;
pub const LAYOUT_COLOR_ATTACHMENT_OPTIMAL: i32 = 2;
pub const ACCESS_COLOR_ATTACHMENT_WRITE: u32 = 0x0000_0100;
const ASPECT_COLOR: u32 = 0x0000_0001;
const QUEUE_FAMILY_IGNORED: u32 = u32::MAX;

// ------------------------------------------------------------------ structs
//
// Laid out by hand rather than bindgen'd for the same reason the rest of this
// directory is: the whole Vulkan surface Vanta touches is a dozen calls, and
// a generated crate would bring a build-time dependency and a version to keep
// aligned for no gain. Every field order and padding below follows the Vulkan
// specification's C layout, which is what the driver reads.

#[repr(C)]
struct BufferCreateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    flags: u32,
    _pad2: u32,
    size: u64,
    usage: u32,
    sharing_mode: u32,
    queue_family_index_count: u32,
    _pad3: u32,
    queue_family_indices: *const u32,
}

#[repr(C)]
struct MemoryRequirements {
    size: u64,
    alignment: u64,
    memory_type_bits: u32,
    _pad: u32,
}

#[repr(C)]
struct MemoryAllocateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    allocation_size: u64,
    memory_type_index: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryType {
    property_flags: u32,
    heap_index: u32,
}

#[repr(C)]
pub struct PhysicalDeviceMemoryProperties {
    memory_type_count: u32,
    memory_types: [MemoryType; 32],
    memory_heap_count: u32,
    memory_heaps: [[u64; 2]; 16],
}

#[repr(C)]
struct CommandPoolCreateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    flags: u32,
    queue_family_index: u32,
}

#[repr(C)]
struct CommandBufferAllocateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    command_pool: u64,
    level: u32,
    command_buffer_count: u32,
}

#[repr(C)]
struct CommandBufferBeginInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    flags: u32,
    _pad2: u32,
    inheritance_info: *const c_void,
}

#[repr(C)]
struct ImageSubresourceRange {
    aspect_mask: u32,
    base_mip_level: u32,
    level_count: u32,
    base_array_layer: u32,
    layer_count: u32,
}

#[repr(C)]
struct ImageMemoryBarrier {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    src_access_mask: u32,
    dst_access_mask: u32,
    old_layout: i32,
    new_layout: i32,
    src_queue_family_index: u32,
    dst_queue_family_index: u32,
    image: u64,
    subresource: ImageSubresourceRange,
    _pad2: u32,
}

#[repr(C)]
struct ImageSubresourceLayers {
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
}

#[repr(C)]
struct BufferImageCopy {
    buffer_offset: u64,
    buffer_row_length: u32,
    buffer_image_height: u32,
    image_subresource: ImageSubresourceLayers,
    image_offset: [i32; 3],
    image_extent: [u32; 3],
}

#[repr(C)]
struct FenceCreateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    flags: u32,
    _pad2: u32,
}

#[repr(C)]
struct SemaphoreCreateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    flags: u32,
    _pad2: u32,
}

#[repr(C)]
struct SubmitInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    wait_semaphore_count: u32,
    _pad2: u32,
    wait_semaphores: *const u64,
    wait_dst_stage_mask: *const u32,
    command_buffer_count: u32,
    _pad3: u32,
    command_buffers: *const u64,
    signal_semaphore_count: u32,
    _pad4: u32,
    signal_semaphores: *const u64,
}

// -------------------------------------------------------- recorded per swapchain
//
// Filled by `vulkan.rs` as it interposes the calls that already pass through
// it, so nothing extra is hooked for this: the device and queue family come
// from `vkCreateDevice`, and the swapchain's handle, extent and format from
// `vkCreateSwapchainKHR`.

pub static DEVICE: AtomicUsize = AtomicUsize::new(0);
pub static QUEUE_FAMILY: AtomicU64 = AtomicU64::new(0);
pub static SWAPCHAIN: AtomicU64 = AtomicU64::new(0);
/// Width in the high half, height in the low half, so one atomic carries both.
pub static EXTENT: AtomicU64 = AtomicU64::new(0);
pub static FORMAT: AtomicU64 = AtomicU64::new(0);

pub fn note_swapchain(swapchain: u64, width: u32, height: u32, format: u32) {
    SWAPCHAIN.store(swapchain, Ordering::Relaxed);
    EXTENT.store(((width as u64) << 32) | height as u64, Ordering::Relaxed);
    FORMAT.store(format as u64, Ordering::Relaxed);
}

pub fn extent() -> (u32, u32) {
    let v = EXTENT.load(Ordering::Relaxed);
    ((v >> 32) as u32, (v & 0xffff_ffff) as u32)
}

/// A capture asked for and not yet taken. One at a time: a second request
/// while one is pending replaces it, because a harness that asks twice wants
/// the newer frame.
static PENDING: Mutex<Option<String>> = Mutex::new(None);
/// Lock-free hint for the per-frame present path. The request itself remains
/// behind `PENDING`; this only prevents taking that mutex on every ordinary
/// frame when no capture was requested.
static PENDING_FAST: AtomicBool = AtomicBool::new(false);
/// Set once the pending capture finishes, so the requester can be told.
static RESULT: Mutex<Option<Result<String, String>>> = Mutex::new(None);

pub fn request(path: &str) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *pending = Some(path.to_string());
    PENDING_FAST.store(true, Ordering::Release);
}

pub fn take_result() -> Option<Result<String, String>> {
    RESULT.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Give up on the pending capture, so a requester waiting on it is told why
/// rather than timing out. Without this every setup failure looked identical
/// to a wedged engine, which is the one thing this surface exists to tell
/// apart.
pub fn abandon(why: &str) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let had_request = pending.take().is_some();
    PENDING_FAST.store(false, Ordering::Release);
    drop(pending);
    if had_request {
        finish(Err(why.to_string()));
    }
}

fn finish(r: Result<String, String>) {
    *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
}

/// Whether a capture is waiting, checked once per present.
///
/// The full request is mutex-protected, but the normal (no capture) case is a
/// single atomic load so presenting frames never contends with the harness.
pub fn pending() -> bool {
    PENDING_FAST.load(Ordering::Acquire)
}

/// Device-level entry points, resolved through the host's
/// `vkGetDeviceProcAddr` once per capture.
struct DeviceFns {
    create_buffer: extern "C" fn(u64, *const BufferCreateInfo, *const c_void, *mut u64) -> i32,
    get_buffer_memory_requirements: extern "C" fn(u64, u64, *mut MemoryRequirements),
    allocate_memory: extern "C" fn(u64, *const MemoryAllocateInfo, *const c_void, *mut u64) -> i32,
    bind_buffer_memory: extern "C" fn(u64, u64, u64, u64) -> i32,
    create_command_pool:
        extern "C" fn(u64, *const CommandPoolCreateInfo, *const c_void, *mut u64) -> i32,
    reset_command_pool: extern "C" fn(u64, u64, u32) -> i32,
    allocate_command_buffers:
        extern "C" fn(u64, *const CommandBufferAllocateInfo, *mut u64) -> i32,
    begin_command_buffer: extern "C" fn(u64, *const CommandBufferBeginInfo) -> i32,
    cmd_pipeline_barrier: extern "C" fn(
        u64,
        u32,
        u32,
        u32,
        u32,
        *const c_void,
        u32,
        *const c_void,
        u32,
        *const ImageMemoryBarrier,
    ),
    cmd_copy_image_to_buffer: extern "C" fn(u64, u64, i32, u64, u32, *const BufferImageCopy),
    end_command_buffer: extern "C" fn(u64) -> i32,
    queue_submit: extern "C" fn(u64, u32, *const SubmitInfo, u64) -> i32,
    create_fence: extern "C" fn(u64, *const FenceCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_fence: extern "C" fn(u64, u64, *const c_void),
    create_semaphore: extern "C" fn(u64, *const SemaphoreCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_semaphore: extern "C" fn(u64, u64, *const c_void),
    wait_for_fences: extern "C" fn(u64, u32, *const u64, u32, u64) -> i32,
    reset_fences: extern "C" fn(u64, u32, *const u64) -> i32,
    map_memory: extern "C" fn(u64, u64, u64, u64, u32, *mut *mut c_void) -> i32,
    unmap_memory: extern "C" fn(u64, u64),
    destroy_buffer: extern "C" fn(u64, u64, *const c_void),
    free_memory: extern "C" fn(u64, u64, *const c_void),
    destroy_command_pool: extern "C" fn(u64, u64, *const c_void),
}

macro_rules! load {
    ($get:expr, $dev:expr, $name:literal) => {{
        let n = concat!($name, "\0");
        let p = $get($dev, n.as_ptr() as *const std::ffi::c_char);
        if p.is_null() {
            return Err(concat!("driver has no ", $name).into());
        }
        // SAFETY: the loader returned a function for exactly this name, whose
        // signature is the one the Vulkan specification gives it.
        unsafe { std::mem::transmute(p) }
    }};
}

/// Do the copy. Called from inside `vkQueuePresentKHR`, with the queue the
/// engine is presenting on and the image it is presenting.
pub fn capture(
    queue: u64,
    swapchain: u64,
    image_index: u32,
    get_device_proc_addr: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    get_physical_device_memory_properties: extern "C" fn(
        *mut c_void,
        *mut PhysicalDeviceMemoryProperties,
    ),
    physical_device: *mut c_void,
) {
    let _ = capture_synced(
        queue,
        swapchain,
        image_index,
        get_device_proc_addr,
        get_physical_device_memory_properties,
        physical_device,
        &[],
    );
}

/// Synchronized counterpart of [`capture`] that chains `VkPresentInfoKHR`'s
/// `pWaitSemaphores` through the capture submit and returns the newly signaled
/// semaphore (if any) for `vkQueuePresentKHR` to wait on.
pub fn capture_synced(
    queue: u64,
    swapchain: u64,
    image_index: u32,
    get_device_proc_addr: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    get_physical_device_memory_properties: extern "C" fn(
        *mut c_void,
        *mut PhysicalDeviceMemoryProperties,
    ),
    physical_device: *mut c_void,
    wait_semaphores: &[u64],
) -> Option<u64> {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let path = pending.take();
    PENDING_FAST.store(false, Ordering::Release);
    drop(pending);
    let path = path?;
    let (r, signaled) = do_capture_synced(
        queue,
        swapchain,
        image_index,
        get_device_proc_addr,
        get_physical_device_memory_properties,
        physical_device,
        &path,
        wait_semaphores,
    );
    if let Err(ref e) = r {
        println!("[android] vulkan: capture failed: {e}");
    }
    finish(r);
    signaled
}

fn do_capture_synced(
    queue: u64,
    swapchain: u64,
    image_index: u32,
    gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
    path: &str,
    wait_semaphores: &[u64],
) -> (Result<String, String>, Option<u64>) {
    let target = match frame_target(queue, swapchain, image_index, gdpa) {
        Ok(t) => t,
        Err(e) => return (Err(e), None),
    };
    copy_to_png_synced(&target, gdpa, gpdmp, physical_device, path, wait_semaphores)
}

/// The image the engine is presenting, as a [`Target`]: the swapchain's own
/// list of images indexed by the present's `pImageIndices`, in the layout the
/// engine leaves it in. Shared by the screenshot and the recorder, which both
/// read the same frame at the same moment.
pub fn frame_target(
    queue: u64,
    swapchain: u64,
    image_index: u32,
    gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
) -> Result<Target, String> {
    let device = DEVICE.load(Ordering::Relaxed) as u64;
    let (width, height) = extent();
    if device == 0 || swapchain == 0 || width == 0 || height == 0 {
        return Err("no swapchain has been created yet".into());
    }

    let get_swapchain_images: extern "C" fn(u64, u64, *mut u32, *mut u64) -> i32 =
        load!(gdpa, device, "vkGetSwapchainImagesKHR");

    // The image being presented, by index into the swapchain's own list.
    let mut count = 0u32;
    if get_swapchain_images(device, swapchain, &mut count, std::ptr::null_mut()) != VK_SUCCESS {
        return Err("vkGetSwapchainImagesKHR failed to count".into());
    }
    let mut images = vec![0u64; count as usize];
    if get_swapchain_images(device, swapchain, &mut count, images.as_mut_ptr()) != VK_SUCCESS {
        return Err("vkGetSwapchainImagesKHR failed".into());
    }
    let image = *images
        .get(image_index as usize)
        .ok_or_else(|| format!("image index {image_index} is outside {count} swapchain images"))?;
    Ok(Target {
        device,
        queue,
        queue_family: QUEUE_FAMILY.load(Ordering::Relaxed) as u32,
        image,
        layout: LAYOUT_PRESENT_SRC_KHR,
        src_access: ACCESS_MEMORY_READ,
        layer: 0,
        rect: (0, 0, width, height),
        format: FORMAT.load(Ordering::Relaxed) as u32,
    })
}

/// One image to read back: where it is, the layout it is in and must be
/// left in, and which layer and rectangle of it.
pub struct Target {
    pub device: u64,
    pub queue: u64,
    pub queue_family: u32,
    pub image: u64,
    /// The layout the image is in now, and is put back into after the copy.
    pub layout: i32,
    /// What last touched it, for the barrier into the copy.
    pub src_access: u32,
    pub layer: u32,
    /// x, y, width, height.
    pub rect: (i32, i32, u32, u32),
    pub format: u32,
}

/// The capture an OpenXR frame takes (`guest_xr`): an image the engine has
/// just finished rendering into, read before it is released to the runtime.
/// Same one-at-a-time request as the present path's.
#[cfg(all(feature = "vr", target_arch = "x86_64"))]
pub fn capture_target(
    target: &Target,
    gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let path = pending.take();
    PENDING_FAST.store(false, Ordering::Release);
    drop(pending);
    let Some(path) = path else { return };
    let r = copy_to_png(target, gdpa, gpdmp, physical_device, &path);
    if let Err(ref e) = r {
        println!("[android] vulkan: capture failed: {e}");
    }
    finish(r);
}

struct ReadbackSlot {
    buffer: u64,
    memory: u64,
    pool: u64,
    cb: u64,
    fence: u64,
    semaphore: u64,
    mapped: *const u8,
    in_flight: bool,
}

impl ReadbackSlot {
    fn empty() -> Self {
        Self {
            buffer: 0,
            memory: 0,
            pool: 0,
            cb: 0,
            fence: 0,
            semaphore: 0,
            mapped: std::ptr::null(),
            in_flight: false,
        }
    }
}

/// Reads images back into host memory via a multi-slot async `VkFence` ring.
///
/// For video recording ([`Readback::new`]), allocates [`RING_SLOTS`] staging
/// buffers, command pools/buffers, and fences so each recorded frame submits
/// its GPU copy and harvests a slot submitted 2 frames earlier — eliminating
/// the synchronous `vkQueueWaitIdle` pipeline drain on every frame. For
/// one-shot screenshots ([`copy_to_png`]), allocates a single slot and waits
/// only on its own fence.
///
/// Sized for one extent. A swapchain that is recreated at another size needs a
/// new `Readback`; reading a different rectangle into this one is refused
/// rather than overrun.
pub struct Readback {
    f: DeviceFns,
    device: u64,
    slots: Vec<ReadbackSlot>,
    submit_idx: usize,
    last_ready: Option<usize>,
    size: usize,
    width: u32,
    height: u32,
}

// SAFETY: each slot's `mapped` points into device memory this value owns for
// its whole life and is only ever read through `&mut self`, so moving the value
// to another thread moves the only handle to it.
unsafe impl Send for Readback {}

impl Drop for Readback {
    fn drop(&mut self) {
        // Wait cleanly on all in-flight fences before destroying any buffers,
        // fences, or command pools (teardown or swapchain resize).
        let mut pending = Vec::with_capacity(self.slots.len());
        for slot in &self.slots {
            if slot.in_flight && slot.fence != 0 {
                pending.push(slot.fence);
            }
        }
        if !pending.is_empty() {
            (self.f.wait_for_fences)(
                self.device,
                pending.len() as u32,
                pending.as_ptr(),
                VK_TRUE,
                u64::MAX,
            );
        }
        // Frees whatever `new` got as far as creating, in reverse. Handles are
        // zero until made, so a failure half way through allocation cleans up
        // exactly what exists and nothing else.
        for slot in &mut self.slots {
            if !slot.mapped.is_null() {
                (self.f.unmap_memory)(self.device, slot.memory);
            }
            if slot.semaphore != 0 {
                (self.f.destroy_semaphore)(self.device, slot.semaphore, std::ptr::null());
            }
            if slot.fence != 0 {
                (self.f.destroy_fence)(self.device, slot.fence, std::ptr::null());
            }
            if slot.pool != 0 {
                (self.f.destroy_command_pool)(self.device, slot.pool, std::ptr::null());
            }
            if slot.memory != 0 {
                (self.f.free_memory)(self.device, slot.memory, std::ptr::null());
            }
            if slot.buffer != 0 {
                (self.f.destroy_buffer)(self.device, slot.buffer, std::ptr::null());
            }
        }
    }
}

impl Readback {
    pub fn new(
        t: &Target,
        gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
        gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
        physical_device: *mut c_void,
    ) -> Result<Self, String> {
        Self::new_with_slots(t, gdpa, gpdmp, physical_device, RING_SLOTS)
    }

    fn new_with_slots(
        t: &Target,
        gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
        gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
        physical_device: *mut c_void,
        slot_count: usize,
    ) -> Result<Self, String> {
        let device = t.device;
        let (_, _, width, height) = t.rect;
        if device == 0 || t.image == 0 || width == 0 || height == 0 || slot_count == 0 {
            return Err("no image to read".into());
        }
        let f = DeviceFns {
            create_buffer: load!(gdpa, device, "vkCreateBuffer"),
            get_buffer_memory_requirements: load!(gdpa, device, "vkGetBufferMemoryRequirements"),
            allocate_memory: load!(gdpa, device, "vkAllocateMemory"),
            bind_buffer_memory: load!(gdpa, device, "vkBindBufferMemory"),
            create_command_pool: load!(gdpa, device, "vkCreateCommandPool"),
            reset_command_pool: load!(gdpa, device, "vkResetCommandPool"),
            allocate_command_buffers: load!(gdpa, device, "vkAllocateCommandBuffers"),
            begin_command_buffer: load!(gdpa, device, "vkBeginCommandBuffer"),
            cmd_pipeline_barrier: load!(gdpa, device, "vkCmdPipelineBarrier"),
            cmd_copy_image_to_buffer: load!(gdpa, device, "vkCmdCopyImageToBuffer"),
            end_command_buffer: load!(gdpa, device, "vkEndCommandBuffer"),
            queue_submit: load!(gdpa, device, "vkQueueSubmit"),
            create_fence: load!(gdpa, device, "vkCreateFence"),
            destroy_fence: load!(gdpa, device, "vkDestroyFence"),
            create_semaphore: load!(gdpa, device, "vkCreateSemaphore"),
            destroy_semaphore: load!(gdpa, device, "vkDestroySemaphore"),
            wait_for_fences: load!(gdpa, device, "vkWaitForFences"),
            reset_fences: load!(gdpa, device, "vkResetFences"),
            map_memory: load!(gdpa, device, "vkMapMemory"),
            unmap_memory: load!(gdpa, device, "vkUnmapMemory"),
            destroy_buffer: load!(gdpa, device, "vkDestroyBuffer"),
            free_memory: load!(gdpa, device, "vkFreeMemory"),
            destroy_command_pool: load!(gdpa, device, "vkDestroyCommandPool"),
        };
        let mut rb = Readback {
            f,
            device,
            slots: Vec::with_capacity(slot_count),
            submit_idx: 0,
            last_ready: None,
            // Four bytes a pixel: every format a swapchain is created with on
            // this path is a 32-bit BGRA or RGBA, and `FORMAT` records which
            // so the channel order can be fixed up on the way out rather than
            // guessed.
            size: width as usize * height as usize * 4,
            width,
            height,
        };

        // SAFETY: the driver fills this out entirely; it is read-only afterwards.
        let mut props: PhysicalDeviceMemoryProperties = unsafe { std::mem::zeroed() };
        (gpdmp)(physical_device, &mut props);
        let wanted = MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT;
        // Prefer HOST_CACHED (0x8) so CPU readback of 8-15 MB frames runs at
        // cached RAM bandwidth rather than stalling 10-80 ms on uncached
        // write-combined memory.
        let cached = wanted | 0x8;
        let pick = |type_bits: u32, req_flags: u32| {
            (0..props.memory_type_count).find(|i| {
                type_bits & (1 << i) != 0
                    && props.memory_types[*i as usize].property_flags & req_flags == req_flags
            })
        };

        for _ in 0..slot_count {
            rb.slots.push(ReadbackSlot::empty());
            let slot = rb.slots.last_mut().expect("just pushed");

            let bci = BufferCreateInfo {
                s_type: ST_BUFFER_CREATE_INFO,
                _pad: 0,
                next: std::ptr::null(),
                flags: 0,
                _pad2: 0,
                size: rb.size as u64,
                usage: BUFFER_USAGE_TRANSFER_DST,
                sharing_mode: 0,
                queue_family_index_count: 0,
                _pad3: 0,
                queue_family_indices: std::ptr::null(),
            };
            let mut buffer = 0u64;
            if (rb.f.create_buffer)(device, &bci, std::ptr::null(), &mut buffer) != VK_SUCCESS {
                return Err("vkCreateBuffer failed".into());
            }
            slot.buffer = buffer;

            let mut req = MemoryRequirements { size: 0, alignment: 0, memory_type_bits: 0, _pad: 0 };
            (rb.f.get_buffer_memory_requirements)(device, buffer, &mut req);

            let type_index = pick(req.memory_type_bits, cached)
                .or_else(|| pick(req.memory_type_bits, wanted))
                .ok_or("no host-visible coherent memory type accepts this buffer")?;

            let mai = MemoryAllocateInfo {
                s_type: ST_MEMORY_ALLOCATE_INFO,
                _pad: 0,
                next: std::ptr::null(),
                allocation_size: req.size,
                memory_type_index: type_index,
                _pad2: 0,
            };
            let mut memory = 0u64;
            if (rb.f.allocate_memory)(device, &mai, std::ptr::null(), &mut memory) != VK_SUCCESS {
                return Err("vkAllocateMemory failed".into());
            }
            slot.memory = memory;
            if (rb.f.bind_buffer_memory)(device, buffer, memory, 0) != VK_SUCCESS {
                return Err("vkBindBufferMemory failed".into());
            }

            let pool_ci = CommandPoolCreateInfo {
                s_type: ST_COMMAND_POOL_CREATE_INFO,
                _pad: 0,
                next: std::ptr::null(),
                flags: POOL_TRANSIENT,
                queue_family_index: t.queue_family,
            };
            let mut pool = 0u64;
            if (rb.f.create_command_pool)(device, &pool_ci, std::ptr::null(), &mut pool) != VK_SUCCESS {
                return Err("vkCreateCommandPool failed".into());
            }
            slot.pool = pool;

            let cb_ai = CommandBufferAllocateInfo {
                s_type: ST_COMMAND_BUFFER_ALLOCATE_INFO,
                _pad: 0,
                next: std::ptr::null(),
                command_pool: pool,
                level: 0,
                command_buffer_count: 1,
            };
            let mut cb = 0u64;
            if (rb.f.allocate_command_buffers)(device, &cb_ai, &mut cb) != VK_SUCCESS {
                return Err("vkAllocateCommandBuffers failed".into());
            }
            slot.cb = cb;

            let fci = FenceCreateInfo {
                s_type: ST_FENCE_CREATE_INFO,
                _pad: 0,
                next: std::ptr::null(),
                flags: 0,
                _pad2: 0,
            };
            let mut fence = 0u64;
            if (rb.f.create_fence)(device, &fci, std::ptr::null(), &mut fence) != VK_SUCCESS {
                return Err("vkCreateFence failed".into());
            }
            slot.fence = fence;

            let sci = SemaphoreCreateInfo {
                s_type: ST_SEMAPHORE_CREATE_INFO,
                _pad: 0,
                next: std::ptr::null(),
                flags: 0,
                _pad2: 0,
            };
            let mut semaphore = 0u64;
            if (rb.f.create_semaphore)(device, &sci, std::ptr::null(), &mut semaphore) != VK_SUCCESS
            {
                return Err("vkCreateSemaphore failed".into());
            }
            slot.semaphore = semaphore;

            // Mapped once and left mapped. The memory is host-coherent, so nothing
            // needs invalidating between reads.
            let mut ptr: *mut c_void = std::ptr::null_mut();
            if (rb.f.map_memory)(device, memory, 0, rb.size as u64, 0, &mut ptr) != VK_SUCCESS
                || ptr.is_null()
            {
                return Err("vkMapMemory failed".into());
            }
            slot.mapped = ptr as *const u8;
        }
        Ok(rb)
    }

    /// Submit `t`'s copy into the current free ring slot with its `VkFence` and
    /// return completed pixels without draining the GPU pipeline.
    ///
    /// In steady state (`slots.len() == 3`), waits only on the oldest in-flight
    /// slot's fence (submitted 2 frames ago) and hands back its pixels while
    /// the current frame's copy executes asynchronously on the GPU.
    pub fn read(&mut self, queue: u64, t: &Target) -> Result<&[u8], String> {
        self.read_synced(queue, t, &[]).map(|(pixels, _)| pixels)
    }

    /// Submit `t`'s copy into the current free ring slot with its `VkFence`,
    /// waiting on `wait_semaphores` (the engine's `VkPresentInfoKHR` render-done
    /// semaphores) before `vkCmdCopyImageToBuffer` and signaling this slot's
    /// semaphore for `vkQueuePresentKHR` to wait on.
    pub fn read_synced(
        &mut self,
        queue: u64,
        t: &Target,
        wait_semaphores: &[u64],
    ) -> Result<(&[u8], Option<u64>), String> {
        let (x, y, width, height) = t.rect;
        if (width, height) != (self.width, self.height) {
            return Err(format!(
                "readback was sized {}x{} and was asked for {width}x{height}",
                self.width, self.height
            ));
        }
        let idx = self.submit_idx;
        let slot_count = self.slots.len();
        let (image, pool, cb, buffer, fence, slot_semaphore) = {
            let s = &self.slots[idx];
            debug_assert!(!s.in_flight, "submit slot must not be in flight");
            (t.image, s.pool, s.cb, s.buffer, s.fence, s.semaphore)
        };
        let f = &self.f;
        (f.reset_command_pool)(self.device, pool, 0);

        let begin = CommandBufferBeginInfo {
            s_type: ST_COMMAND_BUFFER_BEGIN_INFO,
            _pad: 0,
            next: std::ptr::null(),
            flags: CMD_BUFFER_ONE_TIME_SUBMIT,
            _pad2: 0,
            inheritance_info: std::ptr::null(),
        };
        (f.begin_command_buffer)(cb, &begin);

        let to_src = ImageMemoryBarrier {
            s_type: ST_IMAGE_MEMORY_BARRIER,
            _pad: 0,
            next: std::ptr::null(),
            src_access_mask: t.src_access,
            dst_access_mask: ACCESS_TRANSFER_READ,
            old_layout: t.layout,
            new_layout: LAYOUT_TRANSFER_SRC_OPTIMAL,
            src_queue_family_index: QUEUE_FAMILY_IGNORED,
            dst_queue_family_index: QUEUE_FAMILY_IGNORED,
            image,
            subresource: ImageSubresourceRange {
                aspect_mask: ASPECT_COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: t.layer,
                layer_count: 1,
            },
            _pad2: 0,
        };
        (f.cmd_pipeline_barrier)(
            cb,
            STAGE_ALL_COMMANDS,
            STAGE_TRANSFER,
            0,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            1,
            &to_src,
        );

        let region = BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: ImageSubresourceLayers {
                aspect_mask: ASPECT_COLOR,
                mip_level: 0,
                base_array_layer: t.layer,
                layer_count: 1,
            },
            image_offset: [x, y, 0],
            image_extent: [width, height, 1],
        };
        (f.cmd_copy_image_to_buffer)(cb, image, LAYOUT_TRANSFER_SRC_OPTIMAL, buffer, 1, &region);

        let back = ImageMemoryBarrier {
            src_access_mask: ACCESS_TRANSFER_READ,
            dst_access_mask: ACCESS_MEMORY_READ,
            old_layout: LAYOUT_TRANSFER_SRC_OPTIMAL,
            new_layout: t.layout,
            ..to_src
        };
        (f.cmd_pipeline_barrier)(
            cb,
            STAGE_TRANSFER,
            STAGE_TRANSFER,
            0,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            1,
            &back,
        );
        (f.end_command_buffer)(cb);

        let chain_sem = !wait_semaphores.is_empty() && slot_semaphore != 0;
        let wait_stages: Vec<u32> = if chain_sem {
            vec![STAGE_TRANSFER; wait_semaphores.len()]
        } else {
            Vec::new()
        };
        let submit = SubmitInfo {
            s_type: ST_SUBMIT_INFO,
            _pad: 0,
            next: std::ptr::null(),
            wait_semaphore_count: if chain_sem { wait_semaphores.len() as u32 } else { 0 },
            _pad2: 0,
            wait_semaphores: if chain_sem { wait_semaphores.as_ptr() } else { std::ptr::null() },
            wait_dst_stage_mask: if chain_sem { wait_stages.as_ptr() } else { std::ptr::null() },
            command_buffer_count: 1,
            _pad3: 0,
            command_buffers: &cb,
            signal_semaphore_count: u32::from(chain_sem),
            _pad4: 0,
            signal_semaphores: if chain_sem { &slot_semaphore } else { std::ptr::null() },
        };
        if (f.queue_submit)(queue, 1, &submit, fence) != VK_SUCCESS {
            return Err("vkQueueSubmit failed".into());
        }
        self.slots[idx].in_flight = true;

        let harvest_idx = if slot_count == 1 || self.last_ready.is_none() {
            if (f.wait_for_fences)(self.device, 1, &fence, VK_TRUE, u64::MAX) != VK_SUCCESS {
                return Err("vkWaitForFences failed".into());
            }
            if (f.reset_fences)(self.device, 1, &fence) != VK_SUCCESS {
                return Err("vkResetFences failed".into());
            }
            self.slots[idx].in_flight = false;
            self.last_ready = Some(idx);
            self.submit_idx = (idx + 1) % slot_count;
            idx
        } else {
            let oldest = (idx + 1) % slot_count;
            if self.slots[oldest].in_flight {
                let oldest_fence = self.slots[oldest].fence;
                if (f.wait_for_fences)(self.device, 1, &oldest_fence, VK_TRUE, u64::MAX) != VK_SUCCESS
                {
                    return Err("vkWaitForFences failed".into());
                }
                if (f.reset_fences)(self.device, 1, &oldest_fence) != VK_SUCCESS {
                    return Err("vkResetFences failed".into());
                }
                self.slots[oldest].in_flight = false;
                self.last_ready = Some(oldest);
                self.submit_idx = oldest;
                oldest
            } else {
                self.submit_idx = oldest;
                self.last_ready.expect("checked is_some above")
            }
        };

        let signaled = chain_sem.then_some(slot_semaphore);
        // SAFETY: `mapped` is `size` bytes of host-coherent memory owned by
        // `self.slots[harvest_idx]`, which is no longer in flight on the GPU.
        Ok((
            unsafe { std::slice::from_raw_parts(self.slots[harvest_idx].mapped, self.size) },
            signaled,
        ))
    }
}

#[cfg(all(feature = "vr", target_arch = "x86_64"))]
fn copy_to_png(
    t: &Target,
    gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
    path: &str,
) -> Result<String, String> {
    copy_to_png_synced(t, gdpa, gpdmp, physical_device, path, &[]).0
}

fn copy_to_png_synced(
    t: &Target,
    gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
    path: &str,
    wait_semaphores: &[u64],
) -> (Result<String, String>, Option<u64>) {
    let (_, _, width, height) = t.rect;
    let mut rb = match Readback::new_with_slots(t, gdpa, gpdmp, physical_device, 1) {
        Ok(rb) => rb,
        Err(e) => return (Err(e), None),
    };
    let (pixels, signaled) = match rb.read_synced(t.queue, t, wait_semaphores) {
        Ok(res) => res,
        Err(e) => return (Err(e), None),
    };
    let written = write_png(path, width, height, pixels, t.format)
        .map(|_| format!("{path} {width}x{height}"));
    // Keep the 1-slot Readback (and its signaled semaphore) alive until the
    // next `vkQueuePresentKHR` on the Vulkan present thread so the driver can
    // wait on `signaled` during this frame's present.
    if signaled.is_some() {
        crate::recording::retire_readback(rb);
    }
    (written, signaled)
}

/// Whether a swapchain format stores blue first.
///
/// 44/50 are B8G8R8A8_UNORM/SRGB, 37/43 are R8G8B8A8_UNORM/SRGB. Anything else
/// is reported rather than guessed at, because a silently swapped red and blue
/// channel is exactly the kind of wrong-but-plausible output this project keeps
/// having to retract. The recorder asks too, so a video and a screenshot of the
/// same frame cannot disagree about which channel is which.
pub fn is_bgra(format: u32) -> Result<bool, String> {
    match format {
        44 | 50 => Ok(true),
        37 | 43 => Ok(false),
        other => Err(format!("unhandled swapchain format {other}")),
    }
}

/// Write the pixels out as a PNG, with no encoder dependency.
///
/// Stored uncompressed, in zlib's "stored" block form, which every PNG reader
/// accepts. A 1920x1200 frame lands around 9 MB, which is fine for something a
/// harness looks at and deletes; pulling in a deflate implementation to make it
/// smaller would add a dependency to the runtime crate for a development aid.
fn write_png(path: &str, width: u32, height: u32, pixels: &[u8], format: u32) -> Result<(), String> {
    let bgra = is_bgra(format)?;

    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0u8); // filter: none
        let row = &pixels[y * width as usize * 4..][..width as usize * 4];
        for px in row.chunks_exact(4) {
            if bgra {
                raw.extend_from_slice(&[px[2], px[1], px[0]]);
            } else {
                raw.extend_from_slice(&[px[0], px[1], px[2]]);
            }
        }
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    chunk(&mut png, b"IEND", &[]);
    std::fs::write(path, png).map_err(|e| format!("writing {path}: {e}"))
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut c = crc32(kind);
    c = crc32_continue(c, data);
    out.extend_from_slice(&c.to_be_bytes());
}

/// zlib stream whose deflate blocks are all "stored", i.e. uncompressed.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut i = 0;
    while i < data.len() {
        let n = usize::min(65535, data.len() - i);
        let last = if i + n >= data.len() { 1 } else { 0 };
        out.push(last);
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(&data[i..i + n]);
        i += n;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (n, e) in t.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

fn crc32(data: &[u8]) -> u32 {
    // `crc32_continue` takes the *previous finished* CRC and undoes the
    // final XOR-out (`prev ^ 0xffff_ffff`) to recover the raw running
    // register before folding more data in -- see below. For a first call
    // that raw register must start at the standard 0xffff_ffff, which is
    // `prev = 0`. This used to be spelled `0xffff_ffff ^ 0xffff_ffff`, which
    // clippy's `eq_op` flagged as a likely typo; it was not one -- verified
    // against the standard CRC-32/ISO-HDLC check value, `crc32(b"123456789")
    // == 0xcbf4_3926` -- but the constant it reduces to is the honest way to
    // write it.
    crc32_continue(0, data)
}

/// PNG's CRC is over the type and the data together, so it has to be resumable.
fn crc32_continue(prev: u32, data: &[u8]) -> u32 {
    let t = crc_table();
    let mut c = prev ^ 0xffff_ffff;
    for &x in data {
        c = t[((c ^ x as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    struct MockVk {
        next_handle: u64,
        buffers: Vec<(u64, Vec<u8>)>,
        mem_to_buf: std::collections::HashMap<u64, u64>,
        cb_to_buf: std::collections::HashMap<u64, u64>,
        submits: Vec<(u64, u64)>, // (cb, fence)
        submit_waits: Vec<Vec<u64>>,
        submit_signals: Vec<Vec<u64>>,
        waits: Vec<Vec<u64>>,
        resets: Vec<u64>,
        destroyed_fences: Vec<u64>,
        destroyed_semaphores: Vec<u64>,
        destroyed_pools: Vec<u64>,
        destroyed_buffers: Vec<u64>,
        freed_memories: Vec<u64>,
        unmapped_memories: Vec<u64>,
        in_flight_fences: std::collections::HashSet<u64>,
    }

    impl MockVk {
        fn new() -> Self {
            Self {
                next_handle: 1,
                buffers: Vec::new(),
                mem_to_buf: std::collections::HashMap::new(),
                cb_to_buf: std::collections::HashMap::new(),
                submits: Vec::new(),
                submit_waits: Vec::new(),
                submit_signals: Vec::new(),
                waits: Vec::new(),
                resets: Vec::new(),
                destroyed_fences: Vec::new(),
                destroyed_semaphores: Vec::new(),
                destroyed_pools: Vec::new(),
                destroyed_buffers: Vec::new(),
                freed_memories: Vec::new(),
                unmapped_memories: Vec::new(),
                in_flight_fences: std::collections::HashSet::new(),
            }
        }
        fn alloc_handle(&mut self) -> u64 {
            let h = self.next_handle;
            self.next_handle += 1;
            h
        }
    }

    static MOCK: Mutex<Option<MockVk>> = Mutex::new(None);
    static MOCK_GUARD: Mutex<()> = Mutex::new(());

    extern "C" fn mock_create_buffer(_d: u64, info: *const BufferCreateInfo, _: *const c_void, out: *mut u64) -> i32 {
        let mut g = MOCK.lock().unwrap();
        let m = g.as_mut().unwrap();
        let h = m.alloc_handle();
        let sz = unsafe { (*info).size as usize };
        m.buffers.push((h, vec![0u8; sz]));
        unsafe { *out = h };
        VK_SUCCESS
    }
    extern "C" fn mock_get_reqs(_d: u64, buf: u64, req: *mut MemoryRequirements) {
        let g = MOCK.lock().unwrap();
        let m = g.as_ref().unwrap();
        let sz = m.buffers.iter().find(|(h, _)| *h == buf).unwrap().1.len() as u64;
        unsafe {
            (*req).size = sz;
            (*req).alignment = 64;
            (*req).memory_type_bits = 1;
        }
    }
    extern "C" fn mock_alloc_mem(_d: u64, _i: *const MemoryAllocateInfo, _: *const c_void, out: *mut u64) -> i32 {
        let mut g = MOCK.lock().unwrap();
        let h = g.as_mut().unwrap().alloc_handle();
        unsafe { *out = h };
        VK_SUCCESS
    }
    extern "C" fn mock_bind_mem(_d: u64, buf: u64, mem: u64, _: u64) -> i32 {
        MOCK.lock().unwrap().as_mut().unwrap().mem_to_buf.insert(mem, buf);
        VK_SUCCESS
    }
    extern "C" fn mock_create_pool(_d: u64, _: *const CommandPoolCreateInfo, _: *const c_void, out: *mut u64) -> i32 {
        let h = MOCK.lock().unwrap().as_mut().unwrap().alloc_handle();
        unsafe { *out = h };
        VK_SUCCESS
    }
    extern "C" fn mock_reset_pool(_d: u64, _p: u64, _f: u32) -> i32 { VK_SUCCESS }
    extern "C" fn mock_alloc_cbs(_d: u64, _: *const CommandBufferAllocateInfo, out: *mut u64) -> i32 {
        let h = MOCK.lock().unwrap().as_mut().unwrap().alloc_handle();
        unsafe { *out = h };
        VK_SUCCESS
    }
    extern "C" fn mock_begin_cb(_cb: u64, _: *const CommandBufferBeginInfo) -> i32 { VK_SUCCESS }
    extern "C" fn mock_barrier(
        _cb: u64, _: u32, _: u32, _: u32, _: u32, _: *const c_void, _: u32, _: *const c_void, _: u32, _: *const ImageMemoryBarrier,
    ) {}
    extern "C" fn mock_copy(cb: u64, image: u64, _: i32, dst_buf: u64, _: u32, _: *const BufferImageCopy) {
        let mut g = MOCK.lock().unwrap();
        let m = g.as_mut().unwrap();
        m.cb_to_buf.insert(cb, dst_buf);
        // Stamp the buffer with the low byte of `image` so the test can verify
        // which exact frame's pixels are returned from each slot.
        let (_, data) = m.buffers.iter_mut().find(|(h, _)| *h == dst_buf).unwrap();
        data.fill(image as u8);
    }
    extern "C" fn mock_end_cb(_cb: u64) -> i32 { VK_SUCCESS }
    extern "C" fn mock_submit(_q: u64, _cnt: u32, info: *const SubmitInfo, fence: u64) -> i32 {
        let mut g = MOCK.lock().unwrap();
        let m = g.as_mut().unwrap();
        let si = unsafe { &*info };
        let cb = unsafe { *si.command_buffers };
        let wait_sems = if si.wait_semaphore_count > 0 && !si.wait_semaphores.is_null() {
            unsafe { std::slice::from_raw_parts(si.wait_semaphores, si.wait_semaphore_count as usize) }.to_vec()
        } else {
            Vec::new()
        };
        let sig_sems = if si.signal_semaphore_count > 0 && !si.signal_semaphores.is_null() {
            unsafe { std::slice::from_raw_parts(si.signal_semaphores, si.signal_semaphore_count as usize) }.to_vec()
        } else {
            Vec::new()
        };
        assert!(!m.in_flight_fences.contains(&fence), "resubmitted an in-flight fence");
        m.in_flight_fences.insert(fence);
        m.submits.push((cb, fence));
        m.submit_waits.push(wait_sems);
        m.submit_signals.push(sig_sems);
        VK_SUCCESS
    }
    extern "C" fn mock_create_fence(_d: u64, _: *const FenceCreateInfo, _: *const c_void, out: *mut u64) -> i32 {
        let h = MOCK.lock().unwrap().as_mut().unwrap().alloc_handle();
        unsafe { *out = h };
        VK_SUCCESS
    }
    extern "C" fn mock_destroy_fence(_d: u64, f: u64, _: *const c_void) {
        let mut g = MOCK.lock().unwrap();
        let m = g.as_mut().unwrap();
        assert!(!m.in_flight_fences.contains(&f), "destroyed fence while still in flight");
        m.destroyed_fences.push(f);
    }
    extern "C" fn mock_create_semaphore(
        _d: u64,
        _: *const SemaphoreCreateInfo,
        _: *const c_void,
        out: *mut u64,
    ) -> i32 {
        let h = MOCK.lock().unwrap().as_mut().unwrap().alloc_handle();
        unsafe { *out = h };
        VK_SUCCESS
    }
    extern "C" fn mock_destroy_semaphore(_d: u64, s: u64, _: *const c_void) {
        MOCK.lock().unwrap().as_mut().unwrap().destroyed_semaphores.push(s);
    }
    extern "C" fn mock_wait_fences(_d: u64, cnt: u32, fences: *const u64, _: u32, _: u64) -> i32 {
        let mut g = MOCK.lock().unwrap();
        let m = g.as_mut().unwrap();
        let slice = unsafe { std::slice::from_raw_parts(fences, cnt as usize) };
        for &f in slice {
            m.in_flight_fences.remove(&f);
        }
        m.waits.push(slice.to_vec());
        VK_SUCCESS
    }
    extern "C" fn mock_reset_fences(_d: u64, cnt: u32, fences: *const u64) -> i32 {
        let mut g = MOCK.lock().unwrap();
        let m = g.as_mut().unwrap();
        let slice = unsafe { std::slice::from_raw_parts(fences, cnt as usize) };
        m.resets.extend_from_slice(slice);
        VK_SUCCESS
    }
    extern "C" fn mock_map(_d: u64, mem: u64, _: u64, _: u64, _: u32, out: *mut *mut c_void) -> i32 {
        let mut g = MOCK.lock().unwrap();
        let m = g.as_mut().unwrap();
        let buf = *m.mem_to_buf.get(&mem).unwrap();
        let (_, data) = m.buffers.iter_mut().find(|(h, _)| *h == buf).unwrap();
        unsafe { *out = data.as_mut_ptr() as *mut c_void };
        VK_SUCCESS
    }
    extern "C" fn mock_unmap(_d: u64, mem: u64) {
        MOCK.lock().unwrap().as_mut().unwrap().unmapped_memories.push(mem);
    }
    extern "C" fn mock_destroy_buf(_d: u64, buf: u64, _: *const c_void) {
        MOCK.lock().unwrap().as_mut().unwrap().destroyed_buffers.push(buf);
    }
    extern "C" fn mock_free_mem(_d: u64, mem: u64, _: *const c_void) {
        MOCK.lock().unwrap().as_mut().unwrap().freed_memories.push(mem);
    }
    extern "C" fn mock_destroy_pool(_d: u64, pool: u64, _: *const c_void) {
        MOCK.lock().unwrap().as_mut().unwrap().destroyed_pools.push(pool);
    }

    extern "C" fn mock_gdpa(_d: u64, name: *const std::ffi::c_char) -> *mut c_void {
        match unsafe { CStr::from_ptr(name) }.to_str().unwrap() {
            "vkCreateBuffer" => mock_create_buffer as *const () as *mut c_void,
            "vkGetBufferMemoryRequirements" => mock_get_reqs as *const () as *mut c_void,
            "vkAllocateMemory" => mock_alloc_mem as *const () as *mut c_void,
            "vkBindBufferMemory" => mock_bind_mem as *const () as *mut c_void,
            "vkCreateCommandPool" => mock_create_pool as *const () as *mut c_void,
            "vkResetCommandPool" => mock_reset_pool as *const () as *mut c_void,
            "vkAllocateCommandBuffers" => mock_alloc_cbs as *const () as *mut c_void,
            "vkBeginCommandBuffer" => mock_begin_cb as *const () as *mut c_void,
            "vkCmdPipelineBarrier" => mock_barrier as *const () as *mut c_void,
            "vkCmdCopyImageToBuffer" => mock_copy as *const () as *mut c_void,
            "vkEndCommandBuffer" => mock_end_cb as *const () as *mut c_void,
            "vkQueueSubmit" => mock_submit as *const () as *mut c_void,
            "vkCreateFence" => mock_create_fence as *const () as *mut c_void,
            "vkDestroyFence" => mock_destroy_fence as *const () as *mut c_void,
            "vkCreateSemaphore" => mock_create_semaphore as *const () as *mut c_void,
            "vkDestroySemaphore" => mock_destroy_semaphore as *const () as *mut c_void,
            "vkWaitForFences" => mock_wait_fences as *const () as *mut c_void,
            "vkResetFences" => mock_reset_fences as *const () as *mut c_void,
            "vkMapMemory" => mock_map as *const () as *mut c_void,
            "vkUnmapMemory" => mock_unmap as *const () as *mut c_void,
            "vkDestroyBuffer" => mock_destroy_buf as *const () as *mut c_void,
            "vkFreeMemory" => mock_free_mem as *const () as *mut c_void,
            "vkDestroyCommandPool" => mock_destroy_pool as *const () as *mut c_void,
            other => panic!("unexpected Vulkan proc lookup: {other}"),
        }
    }

    extern "C" fn mock_gpdmp(_p: *mut c_void, props: *mut PhysicalDeviceMemoryProperties) {
        unsafe {
            (*props).memory_type_count = 1;
            (*props).memory_types[0] = MemoryType {
                property_flags: MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT | 0x8,
                heap_index: 0,
            };
        }
    }

    fn make_target(image: u64) -> Target {
        Target {
            device: 1,
            queue: 1,
            queue_family: 0,
            image,
            layout: LAYOUT_PRESENT_SRC_KHR,
            src_access: ACCESS_MEMORY_READ,
            layer: 0,
            rect: (0, 0, 2, 2),
            format: 44,
        }
    }

    #[test]
    fn async_ring_pipelines_fences_and_waits_all_in_flight_on_drop() {
        let _guard = MOCK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        *MOCK.lock().unwrap() = Some(MockVk::new());

        let mut rb = Readback::new(&make_target(10), mock_gdpa, mock_gpdmp, std::ptr::null_mut())
            .expect("ring created");
        assert_eq!(rb.slots.len(), RING_SLOTS);
        let fences: Vec<u64> = rb.slots.iter().map(|s| s.fence).collect();

        // Frame 0: primes slot 0, waits on fence[0].
        assert_eq!(rb.read(1, &make_target(10)).unwrap()[0], 10);
        // Frame 1 & 2: fill slots 1 & 2 asynchronously with ZERO waits.
        assert_eq!(rb.read(1, &make_target(11)).unwrap()[0], 10);
        assert_eq!(rb.read(1, &make_target(12)).unwrap()[0], 10);
        {
            let g = MOCK.lock().unwrap();
            let m = g.as_ref().unwrap();
            assert_eq!(m.waits.len(), 1, "warmup frames 1 and 2 must not wait on any fence");
            assert_eq!(m.waits[0], vec![fences[0]]);
        }

        // Frame 3: submits into slot 0, waits on fence[1] (submitted 2 frames ago!) -> yields frame 11.
        assert_eq!(rb.read(1, &make_target(13)).unwrap()[0], 11);
        // Frame 4: submits into slot 1, waits on fence[2] -> yields frame 12.
        assert_eq!(rb.read(1, &make_target(14)).unwrap()[0], 12);
        // Frame 5: submits into slot 2, waits on fence[0] -> yields frame 13.
        assert_eq!(rb.read(1, &make_target(15)).unwrap()[0], 13);

        // Drop with slots 1 and 2 still in flight: Drop must wait on both fences before destroying.
        drop(rb);
        let g = MOCK.lock().unwrap();
        let m = g.as_ref().unwrap();
        assert_eq!(m.destroyed_fences.len(), RING_SLOTS);
        assert_eq!(m.destroyed_semaphores.len(), RING_SLOTS);
        assert_eq!(m.destroyed_pools.len(), RING_SLOTS);
        assert_eq!(m.destroyed_buffers.len(), RING_SLOTS);
        assert_eq!(m.freed_memories.len(), RING_SLOTS);
        assert_eq!(m.unmapped_memories.len(), RING_SLOTS);
        assert!(m.in_flight_fences.is_empty(), "all in-flight fences must be waited before drop");
        assert_eq!(m.waits.last().unwrap().len(), 2, "drop waits on the two remaining in-flight slots");
    }

    #[test]
    fn read_synced_chains_wait_and_signal_semaphores() {
        let _guard = MOCK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        *MOCK.lock().unwrap() = Some(MockVk::new());

        let mut rb = Readback::new(&make_target(42), mock_gdpa, mock_gpdmp, std::ptr::null_mut())
            .expect("ring created");
        let slot0_sem = rb.slots[0].semaphore;
        assert_ne!(slot0_sem, 0);

        let (pixels, signaled) = rb.read_synced(1, &make_target(42), &[777, 888]).unwrap();
        assert_eq!(pixels[0], 42);
        assert_eq!(signaled, Some(slot0_sem));
        {
            let g = MOCK.lock().unwrap();
            let m = g.as_ref().unwrap();
            assert_eq!(m.submit_waits[0], vec![777, 888]);
            assert_eq!(m.submit_signals[0], vec![slot0_sem]);
        }
        drop(rb);
        let g = MOCK.lock().unwrap();
        let m = g.as_ref().unwrap();
        assert_eq!(m.destroyed_semaphores.len(), RING_SLOTS);
    }
}

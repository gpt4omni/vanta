use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::etc_decode::{self, EtcFormat, Surface};
use super::etc_decode_spv::ETC_DECODE_SPV;
use super::glcount;

const VK_SUCCESS: i32 = 0;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_FORMAT_NOT_SUPPORTED: i32 = -11;

const ST_BUFFER_CREATE_INFO: u32 = 12;
const ST_MEMORY_ALLOCATE_INFO: u32 = 5;
const ST_SHADER_MODULE_CREATE_INFO: u32 = 16;
const ST_PIPELINE_SHADER_STAGE_CREATE_INFO: u32 = 18;
const ST_COMPUTE_PIPELINE_CREATE_INFO: u32 = 29;
const ST_PIPELINE_LAYOUT_CREATE_INFO: u32 = 30;
const ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO: u32 = 32;
const ST_DESCRIPTOR_POOL_CREATE_INFO: u32 = 33;
const ST_DESCRIPTOR_SET_ALLOCATE_INFO: u32 = 34;
const ST_WRITE_DESCRIPTOR_SET: u32 = 35;
const ST_BUFFER_MEMORY_BARRIER: u32 = 44;
const ST_PHYSICAL_DEVICE_FEATURES_2: u32 = 1_000_059_000;
const ST_FORMAT_PROPERTIES_3: u32 = 1_000_360_000;
const ST_IMAGE_FORMAT_LIST_CREATE_INFO: u32 = 1_000_147_000;

const FEATURE_COUNT: usize = 55;
const FEATURE_ETC2: usize = 20;
const FEATURE_ASTC_LDR: usize = 21;
const FEATURE_BC: usize = 22;

const FORMAT_FEATURE_SAMPLED_IMAGE: u32 = 0x1;
const FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR: u32 = 0x1000;
const FORMAT_FEATURE_TRANSFER_DST: u32 = 0x8000;
const EMULATED_FORMAT_FEATURES: u32 =
    FORMAT_FEATURE_SAMPLED_IMAGE | FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR | FORMAT_FEATURE_TRANSFER_DST;

const IMAGE_USAGE_TRANSFER_SRC: u32 = 0x1;
const IMAGE_USAGE_TRANSFER_DST: u32 = 0x2;
const IMAGE_USAGE_SAMPLED: u32 = 0x4;
const EMULATED_USAGE: u32 = IMAGE_USAGE_TRANSFER_SRC | IMAGE_USAGE_TRANSFER_DST | IMAGE_USAGE_SAMPLED;
const IMAGE_CREATE_MUTABLE_FORMAT: u32 = 0x8;
const IMAGE_CREATE_CUBE_COMPATIBLE: u32 = 0x10;
const EMULATED_CREATE_FLAGS: u32 = IMAGE_CREATE_MUTABLE_FORMAT | IMAGE_CREATE_CUBE_COMPATIBLE;
const IMAGE_TILING_OPTIMAL: u32 = 0;
const SAMPLE_COUNT_1: u32 = 0x1;

const BUFFER_USAGE_TRANSFER_SRC: u32 = 0x1;
const BUFFER_USAGE_STORAGE_BUFFER: u32 = 0x20;
const MEMORY_DEVICE_LOCAL: u32 = 0x1;
const MEMORY_HOST_VISIBLE: u32 = 0x2;
const MEMORY_HOST_COHERENT: u32 = 0x4;
const MEMORY_HOST_CACHED: u32 = 0x8;
const WHOLE_SIZE: u64 = u64::MAX;

const DESCRIPTOR_TYPE_STORAGE_BUFFER: u32 = 7;
const SHADER_STAGE_COMPUTE_BIT: u32 = 0x20;
const PIPELINE_BIND_POINT_COMPUTE: u32 = 1;
const PIPELINE_STAGE_COMPUTE_SHADER_BIT: u32 = 0x0800;
const PIPELINE_STAGE_TRANSFER_BIT: u32 = 0x1000;
const PIPELINE_STAGE_HOST_BIT: u32 = 0x4000;
const ACCESS_SHADER_READ_BIT: u32 = 0x20;
const ACCESS_SHADER_WRITE_BIT: u32 = 0x40;
const ACCESS_TRANSFER_READ_BIT: u32 = 0x800;
const ACCESS_TRANSFER_WRITE_BIT: u32 = 0x1000;
const ACCESS_HOST_WRITE_BIT: u32 = 0x4000;
const QUEUE_COMPUTE_BIT: u32 = 0x2;
const QUEUE_FAMILY_IGNORED: u32 = u32::MAX;

const STAGING_CHUNK: u64 = 8 << 20;
const STAGING_ALIGN: u64 = 16;
const MAX_POOLED_CHUNKS: usize = 8;
const DS_POOL_SETS: u32 = 64;
const MAX_POOLED_DS_POOLS: usize = 8;
/// Regions at or below this texel count decode inline on the CPU fallback
/// without paying snapshot allocation, mutex/condvar and thread wake overhead.
const CPU_INLINE_TEXELS: usize = 64 * 64;

pub fn disabled() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("VANTA_NO_ETC_EMULATION").is_some_and(|v| v != "0"))
}

/// `VANTA_NO_GPU_ETC_DECODE=1`: keep ETC2/EAC emulation on the CPU fallback
/// path even when the Vulkan device has compute queues. Diagnostic A/B switch.
pub fn gpu_decode_disabled() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("VANTA_NO_GPU_ETC_DECODE").is_some_and(|v| v != "0"))
}

/// `VANTA_FORCE_ETC_EMULATION=1`: emulate on a driver that has native ETC2
/// too. A test switch, in the family of `VANTA_FORCE_GPU_VENDOR`
/// (ADR-046), so that native and decoded output can be compared on one machine
/// without NVIDIA hardware. `VANTA_NO_ETC_EMULATION` wins if both are set.
pub fn forced() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VANTA_FORCE_ETC_EMULATION").is_some_and(|v| v != "0"))
}

/// Whether to emulate on a device whose `textureCompressionETC2` is `native`.
/// The gate is the driver lacking the feature -- not the vendor id, which is
/// what ADR-046 gates everything else NVIDIA on -- because the feature bit is
/// the thing the engine reads and the thing that is wrong; see ADR-049.
fn should_emulate(native: bool, forced: bool) -> bool {
    !native || forced
}

fn trace(args: std::fmt::Arguments<'_>) {
    crate::android::trace(format_args!("vulkan-etc: {args}"));
}

fn unhandled(args: std::fmt::Arguments<'_>) {
    glcount::ETC_UNHANDLED.fetch_add(1, Relaxed);
    trace(format_args!("UNHANDLED {args}"));
}

pub fn etc_format(format: u32) -> Option<(EtcFormat, u32, &'static str)> {
    Some(match format {
        147 => (EtcFormat::Rgb8, 37, "ETC2_R8G8B8_UNORM"),
        148 => (EtcFormat::Rgb8, 43, "ETC2_R8G8B8_SRGB"),
        149 => (EtcFormat::Rgb8A1, 37, "ETC2_R8G8B8A1_UNORM"),
        150 => (EtcFormat::Rgb8A1, 43, "ETC2_R8G8B8A1_SRGB"),
        151 => (EtcFormat::Rgba8, 37, "ETC2_R8G8B8A8_UNORM"),
        152 => (EtcFormat::Rgba8, 43, "ETC2_R8G8B8A8_SRGB"),
        153 => (EtcFormat::R11, 70, "EAC_R11_UNORM"),
        154 => (EtcFormat::R11Signed, 71, "EAC_R11_SNORM"),
        155 => (EtcFormat::Rg11, 77, "EAC_R11G11_UNORM"),
        156 => (EtcFormat::Rg11Signed, 78, "EAC_R11G11_SNORM"),
        _ => return None,
    })
}

fn rd<T: Copy>(base: *const c_void, offset: usize) -> T {
    unsafe { std::ptr::read_unaligned((base as *const u8).add(offset) as *const T) }
}

fn wr<T: Copy>(base: *mut c_void, offset: usize, value: T) {
    unsafe { std::ptr::write_unaligned((base as *mut u8).add(offset) as *mut T, value) }
}

fn slice<'a>(base: *const c_void, len: usize) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(base as *const u8, len) }
}

fn slice_mut<'a>(base: *mut c_void, len: usize) -> &'a mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(base as *mut u8, len) }
}

fn as_fn<F: Copy>(p: usize) -> Option<F> {
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<usize>());
    (p != 0).then(|| unsafe { std::mem::transmute_copy::<usize, F>(&p) })
}

fn chain_find(mut node: *const c_void, s_type: u32) -> *const c_void {
    while !node.is_null() {
        if rd::<u32>(node, 0) == s_type {
            return node;
        }
        node = rd::<*const c_void>(node, 8);
    }
    std::ptr::null()
}

macro_rules! slots {
    ($($name:ident),* $(,)?) => { $(static $name: AtomicUsize = AtomicUsize::new(0);)* };
}

slots!(
    H_FEATURES,
    H_FEATURES2,
    H_FORMAT_PROPS2,
    H_IMAGE_FORMAT_PROPS,
    H_IMAGE_FORMAT_PROPS2,
    H_CREATE_IMAGE,
    H_DEVICE_IMAGE_REQS,
    H_DEVICE_IMAGE_SPARSE_REQS,
    H_DESTROY_IMAGE,
    H_CREATE_IMAGE_VIEW,
    H_CMD_COPY_BUFFER_TO_IMAGE,
    H_CMD_COPY_BUFFER_TO_IMAGE2,
    H_CMD_EXECUTE_COMMANDS,
    H_CMD_COPY_IMAGE,
    H_CMD_COPY_IMAGE2,
    H_CMD_BLIT_IMAGE,
    H_CMD_BLIT_IMAGE2,
    H_CMD_COPY_IMAGE_TO_BUFFER,
    H_CMD_COPY_IMAGE_TO_BUFFER2,
    H_CREATE_BUFFER,
    H_BIND_BUFFER_MEMORY,
    H_BIND_BUFFER_MEMORY2,
    H_DESTROY_BUFFER,
    H_MAP_MEMORY,
    H_MAP_MEMORY2,
    H_UNMAP_MEMORY,
    H_FREE_MEMORY,
    H_QUEUE_SUBMIT,
    H_QUEUE_SUBMIT2,
    H_CREATE_COMMAND_POOL,
    H_ALLOCATE_COMMAND_BUFFERS,
    H_BEGIN_COMMAND_BUFFER,
    H_RESET_COMMAND_BUFFER,
    H_FREE_COMMAND_BUFFERS,
    H_RESET_COMMAND_POOL,
    H_DESTROY_COMMAND_POOL,
    H_CMD_BIND_PIPELINE,
    H_DESTROY_DEVICE,
);

/// Devices whose `vkCreateDevice` was made on an emulating physical device, by
/// handle. Recorded whether or not the staging calls could then be resolved:
/// the engine was told ETC2 works on these, so a call on one that cannot be
/// served has to fail rather than reach the driver with a raw ETC format.
static EMULATING: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Fast-path cache of the single active emulating device handle so hot hooks
/// avoid locking `EMULATING`.
static EMULATING_FAST: AtomicUsize = AtomicUsize::new(0);

fn emulating() -> MutexGuard<'static, Vec<usize>> {
    EMULATING.lock().unwrap_or_else(|e| e.into_inner())
}

fn device_emulates(device: *mut c_void) -> bool {
    if device.is_null() {
        return false;
    }
    let d = device as usize;
    if EMULATING_FAST.load(Relaxed) == d {
        return true;
    }
    emulating().contains(&d)
}

/// `device` is the handle the name was resolved against, null from
/// `vkGetInstanceProcAddr`. A device that is not emulating is not routed
/// through this module: Intel and AMD, and NVIDIA's own native path, get the
/// driver's function directly. The instance-level lookup cannot know which
/// device a pointer will be used on, so it stays hooked, and each hook
/// forwards straight through for a device that is not emulating.
pub fn hook(name: &[u8], host: *mut c_void, device: *mut c_void) -> Option<*mut c_void> {
    if host.is_null() || (disabled() && !crate::android::tracing()) {
        return None;
    }
    if !device.is_null() && !device_emulates(device) && !crate::android::tracing() {
        return None;
    }
    let (slot, ours): (&AtomicUsize, *const ()) = match name {
        b"vkGetPhysicalDeviceFeatures" => (&H_FEATURES, get_features as *const ()),
        b"vkGetPhysicalDeviceFeatures2" | b"vkGetPhysicalDeviceFeatures2KHR" => {
            (&H_FEATURES2, get_features2 as *const ())
        }
        b"vkGetPhysicalDeviceFormatProperties2" | b"vkGetPhysicalDeviceFormatProperties2KHR" => {
            (&H_FORMAT_PROPS2, get_format_props2 as *const ())
        }
        b"vkGetPhysicalDeviceImageFormatProperties" => {
            (&H_IMAGE_FORMAT_PROPS, get_image_format_props as *const ())
        }
        b"vkGetPhysicalDeviceImageFormatProperties2" | b"vkGetPhysicalDeviceImageFormatProperties2KHR" => {
            (&H_IMAGE_FORMAT_PROPS2, get_image_format_props2 as *const ())
        }
        b"vkGetDeviceImageMemoryRequirements" | b"vkGetDeviceImageMemoryRequirementsKHR" => {
            (&H_DEVICE_IMAGE_REQS, get_device_image_requirements as *const ())
        }
        b"vkGetDeviceImageSparseMemoryRequirements" | b"vkGetDeviceImageSparseMemoryRequirementsKHR" => {
            (&H_DEVICE_IMAGE_SPARSE_REQS, get_device_image_sparse_requirements as *const ())
        }
        b"vkCreateImage" => (&H_CREATE_IMAGE, create_image as *const ()),
        b"vkDestroyImage" => (&H_DESTROY_IMAGE, destroy_image as *const ()),
        b"vkCreateImageView" => (&H_CREATE_IMAGE_VIEW, create_image_view as *const ()),
        b"vkCmdCopyBufferToImage" => (&H_CMD_COPY_BUFFER_TO_IMAGE, cmd_copy_buffer_to_image as *const ()),
        b"vkCmdCopyBufferToImage2" | b"vkCmdCopyBufferToImage2KHR" => {
            (&H_CMD_COPY_BUFFER_TO_IMAGE2, cmd_copy_buffer_to_image2 as *const ())
        }
        b"vkCmdExecuteCommands" | b"vkCmdExecuteCommandsKHR" => {
            (&H_CMD_EXECUTE_COMMANDS, cmd_execute_commands as *const ())
        }
        b"vkCmdCopyImage" => (&H_CMD_COPY_IMAGE, cmd_copy_image as *const ()),
        b"vkCmdCopyImage2" | b"vkCmdCopyImage2KHR" => (&H_CMD_COPY_IMAGE2, cmd_copy_image2 as *const ()),
        b"vkCmdBlitImage" => (&H_CMD_BLIT_IMAGE, cmd_blit_image as *const ()),
        b"vkCmdBlitImage2" | b"vkCmdBlitImage2KHR" => (&H_CMD_BLIT_IMAGE2, cmd_blit_image2 as *const ()),
        b"vkCmdCopyImageToBuffer" => (&H_CMD_COPY_IMAGE_TO_BUFFER, cmd_copy_image_to_buffer as *const ()),
        b"vkCmdCopyImageToBuffer2" | b"vkCmdCopyImageToBuffer2KHR" => {
            (&H_CMD_COPY_IMAGE_TO_BUFFER2, cmd_copy_image_to_buffer2 as *const ())
        }
        b"vkCreateBuffer" => (&H_CREATE_BUFFER, create_buffer as *const ()),
        b"vkBindBufferMemory" => (&H_BIND_BUFFER_MEMORY, bind_buffer_memory as *const ()),
        b"vkBindBufferMemory2" | b"vkBindBufferMemory2KHR" => {
            (&H_BIND_BUFFER_MEMORY2, bind_buffer_memory2 as *const ())
        }
        b"vkDestroyBuffer" => (&H_DESTROY_BUFFER, destroy_buffer as *const ()),
        b"vkMapMemory" => (&H_MAP_MEMORY, map_memory as *const ()),
        b"vkMapMemory2" | b"vkMapMemory2KHR" => (&H_MAP_MEMORY2, map_memory2 as *const ()),
        b"vkUnmapMemory" => (&H_UNMAP_MEMORY, unmap_memory as *const ()),
        b"vkFreeMemory" => (&H_FREE_MEMORY, free_memory as *const ()),
        b"vkQueueSubmit" => (&H_QUEUE_SUBMIT, queue_submit as *const ()),
        b"vkQueueSubmit2" | b"vkQueueSubmit2KHR" => (&H_QUEUE_SUBMIT2, queue_submit2 as *const ()),
        b"vkCreateCommandPool" => (&H_CREATE_COMMAND_POOL, create_command_pool as *const ()),
        b"vkAllocateCommandBuffers" => (&H_ALLOCATE_COMMAND_BUFFERS, allocate_command_buffers as *const ()),
        b"vkBeginCommandBuffer" => (&H_BEGIN_COMMAND_BUFFER, begin_command_buffer as *const ()),
        b"vkResetCommandBuffer" => (&H_RESET_COMMAND_BUFFER, reset_command_buffer as *const ()),
        b"vkFreeCommandBuffers" => (&H_FREE_COMMAND_BUFFERS, free_command_buffers as *const ()),
        b"vkResetCommandPool" => (&H_RESET_COMMAND_POOL, reset_command_pool as *const ()),
        b"vkDestroyCommandPool" => (&H_DESTROY_COMMAND_POOL, destroy_command_pool as *const ()),
        b"vkCmdBindPipeline" => (&H_CMD_BIND_PIPELINE, cmd_bind_pipeline as *const ()),
        b"vkDestroyDevice" => (&H_DESTROY_DEVICE, destroy_device as *const ()),
        _ => return None,
    };
    slot.store(host as usize, Relaxed);
    Some(ours as *mut c_void)
}

fn host_features(pd: *mut c_void) -> Option<[u32; FEATURE_COUNT]> {
    let f: extern "C" fn(*mut c_void, *mut u32) =
        as_fn(super::vulkan::host_instance_proc(c"vkGetPhysicalDeviceFeatures") as usize)?;
    let mut feats = [0u32; FEATURE_COUNT];
    f(pd, feats.as_mut_ptr());
    Some(feats)
}

pub fn emulate_for(pd: *mut c_void) -> bool {
    if disabled() || pd.is_null() {
        return false;
    }
    static SEEN: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(_, e)) = seen.iter().find(|(p, _)| *p == pd as usize) {
        return e;
    }
    let emulate = match host_features(pd) {
        Some(f) => {
            let e = should_emulate(f[FEATURE_ETC2] != 0, forced());
            trace(format_args!(
                "physical device {pd:p}: host textureCompressionETC2={} ASTC_LDR={} BC={}; {}",
                f[FEATURE_ETC2],
                f[FEATURE_ASTC_LDR],
                f[FEATURE_BC],
                match (e, f[FEATURE_ETC2] != 0) {
                    (true, true) => "native ETC2 present, emulation FORCED (VANTA_FORCE_ETC_EMULATION)",
                    (true, false) => "ETC2/EAC will be emulated",
                    _ => "native ETC2, emulation off",
                }
            ));
            if e && f[FEATURE_ETC2] != 0 {
                println!(
                    "[android] vulkan-etc: VANTA_FORCE_ETC_EMULATION is set; decoding ETC2/EAC on the CPU \
                     although this driver has it natively"
                );
            }
            e
        }
        None => {
            trace(format_args!("physical device {pd:p}: no vkGetPhysicalDeviceFeatures, emulation off"));
            false
        }
    };
    seen.push((pd as usize, emulate));
    emulate
}

extern "C" fn get_features(pd: *mut c_void, out: *mut u32) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *mut u32)>(H_FEATURES.load(Relaxed)) else {
        return;
    };
    f(pd, out);
    if out.is_null() {
        return;
    }
    patch_features(pd, out as *mut c_void, "vkGetPhysicalDeviceFeatures");
}

extern "C" fn get_features2(pd: *mut c_void, out: *mut c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *mut c_void)>(H_FEATURES2.load(Relaxed)) else {
        return;
    };
    f(pd, out);
    if out.is_null() {
        return;
    }
    let features = (out as *mut u8).wrapping_add(16) as *mut c_void;
    patch_features(pd, features, "vkGetPhysicalDeviceFeatures2");
}

fn patch_features(pd: *mut c_void, features: *mut c_void, via: &str) {
    let off = FEATURE_ETC2 * 4;
    let native: u32 = rd(features, off);
    let emulate = native == 0 && emulate_for(pd);
    if emulate {
        wr::<u32>(features, off, 1);
    }
    trace(format_args!(
        "{via}({pd:p}): textureCompressionETC2 host={native} reported={} ASTC_LDR={} BC={}",
        u32::from(emulate) | native,
        rd::<u32>(features, FEATURE_ASTC_LDR * 4),
        rd::<u32>(features, FEATURE_BC * 4),
    ));
}

pub fn patch_format_properties(
    pd: *mut c_void,
    format: u32,
    out: *mut c_void,
    host: extern "C" fn(*mut c_void, u32, *mut c_void),
) {
    let Some((_, sub, name)) = etc_format(format) else { return };
    if out.is_null() || !emulate_for(pd) {
        return;
    }
    let mut s = [0u32; 3];
    host(pd, sub, s.as_mut_ptr() as *mut c_void);
    let optimal = s[1] & EMULATED_FORMAT_FEATURES;
    wr::<[u32; 3]>(out, 0, [0, optimal, 0]);
    trace(format_args!(
        "vkGetPhysicalDeviceFormatProperties({name}): EMULATED from format {sub}, optimalTiling=0x{optimal:x}"
    ));
}

extern "C" fn get_format_props2(pd: *mut c_void, format: u32, out: *mut c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u32, *mut c_void)>(H_FORMAT_PROPS2.load(Relaxed)) else {
        return;
    };
    let etc = etc_format(format);
    let emulate = etc.is_some() && emulate_for(pd);
    let Some((_, sub, name)) = etc.filter(|_| emulate) else {
        f(pd, format, out);
        if let Some((_, _, name)) = etc {
            if !out.is_null() {
                let p: [u32; 3] = rd(out, 16);
                trace(format_args!(
                    "vkGetPhysicalDeviceFormatProperties2({name}): passthrough linear=0x{:x} optimal=0x{:x} buffer=0x{:x}",
                    p[0], p[1], p[2]
                ));
            }
        }
        return;
    };
    f(pd, sub, out);
    if out.is_null() {
        return;
    }
    let p: [u32; 3] = rd(out, 16);
    let optimal = p[1] & EMULATED_FORMAT_FEATURES;
    wr::<[u32; 3]>(out, 16, [0, optimal, 0]);
    let p3 = chain_find(rd::<*const c_void>(out, 8), ST_FORMAT_PROPERTIES_3) as *mut c_void;
    if !p3.is_null() {
        let o: u64 = rd(p3, 24);
        wr::<[u64; 3]>(p3, 16, [0, o & u64::from(EMULATED_FORMAT_FEATURES), 0]);
    }
    trace(format_args!(
        "vkGetPhysicalDeviceFormatProperties2({name}): EMULATED from format {sub}, optimalTiling=0x{optimal:x}{}",
        if p3.is_null() { "" } else { " (+FormatProperties3)" }
    ));
}

fn image_request_ok(tiling: u32, usage: u32, flags: u32) -> bool {
    tiling == IMAGE_TILING_OPTIMAL && usage & !EMULATED_USAGE == 0 && flags & !EMULATED_CREATE_FLAGS == 0
}

type ImageFormatPropsFn = extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, *mut c_void) -> i32;

extern "C" fn get_image_format_props(
    pd: *mut c_void,
    format: u32,
    kind: u32,
    tiling: u32,
    usage: u32,
    flags: u32,
    out: *mut c_void,
) -> i32 {
    let Some(f) = as_fn::<ImageFormatPropsFn>(H_IMAGE_FORMAT_PROPS.load(Relaxed)) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Some((_, sub, name)) = etc_format(format) else {
        return f(pd, format, kind, tiling, usage, flags, out);
    };
    if !emulate_for(pd) {
        let rc = f(pd, format, kind, tiling, usage, flags, out);
        trace(format_args!(
            "vkGetPhysicalDeviceImageFormatProperties({name}, type={kind}, tiling={tiling}, usage=0x{usage:x}, flags=0x{flags:x}): passthrough rc={rc}"
        ));
        return rc;
    }
    let rc = if image_request_ok(tiling, usage, flags) {
        let rc = f(pd, sub, kind, tiling, usage, flags, out);
        if rc == VK_SUCCESS && !out.is_null() {
            wr::<u32>(out, 20, SAMPLE_COUNT_1);
        }
        rc
    } else {
        VK_ERROR_FORMAT_NOT_SUPPORTED
    };
    trace(format_args!(
        "vkGetPhysicalDeviceImageFormatProperties({name}, type={kind}, tiling={tiling}, usage=0x{usage:x}, flags=0x{flags:x}): EMULATED rc={rc}"
    ));
    rc
}

extern "C" fn get_image_format_props2(pd: *mut c_void, info: *const c_void, out: *mut c_void) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> i32>(H_IMAGE_FORMAT_PROPS2.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() {
        return f(pd, info, out);
    }
    let format: u32 = rd(info, 16);
    let Some((_, sub, name)) = etc_format(format) else {
        return f(pd, info, out);
    };
    let (kind, tiling, usage, flags): (u32, u32, u32, u32) = (rd(info, 20), rd(info, 24), rd(info, 28), rd(info, 32));
    if !emulate_for(pd) {
        let rc = f(pd, info, out);
        trace(format_args!(
            "vkGetPhysicalDeviceImageFormatProperties2({name}, type={kind}, tiling={tiling}, usage=0x{usage:x}, flags=0x{flags:x}): passthrough rc={rc}"
        ));
        return rc;
    }
    let rc = if image_request_ok(tiling, usage, flags) {
        let mut copy: [u8; 40] = rd(info, 0);
        copy[16..20].copy_from_slice(&sub.to_ne_bytes());
        let rc = f(pd, copy.as_ptr() as *const c_void, out);
        if rc == VK_SUCCESS && !out.is_null() {
            wr::<u32>(out, 16 + 20, SAMPLE_COUNT_1);
        }
        rc
    } else {
        VK_ERROR_FORMAT_NOT_SUPPORTED
    };
    trace(format_args!(
        "vkGetPhysicalDeviceImageFormatProperties2({name}, type={kind}, tiling={tiling}, usage=0x{usage:x}, flags=0x{flags:x}): EMULATED rc={rc}"
    ));
    rc
}

#[repr(C, align(8))]
struct StrippedDeviceInfo {
    info: [u8; 72],
    features: [u32; FEATURE_COUNT],
    features2: [u8; 240],
}

/// What [`strip_device_features`] built for one `vkCreateDevice`: usually a copy
/// of the create info with `textureCompressionETC2` cleared, and, for a
/// `VkPhysicalDeviceFeatures2` that is not first in the chain, the caller's own
/// field cleared for the duration of the call and put back on drop.
pub struct Stripped {
    copy: Option<Box<StrippedDeviceInfo>>,
    restore: Vec<usize>,
}

impl Stripped {
    /// The create info the host driver should be given.
    pub fn as_ptr(&self, original: *const c_void) -> *const c_void {
        self.copy.as_ref().map_or(original, |c| c.info.as_ptr() as *const c_void)
    }
}

impl Drop for Stripped {
    fn drop(&mut self) {
        for &at in &self.restore {
            // SAFETY: `at` is the address of the ETC2 feature word inside the
            // caller's `VkPhysicalDeviceFeatures2`, which outlives the
            // `vkCreateDevice` call this guard lives within.
            unsafe { std::ptr::write_unaligned(at as *mut u32, 1) };
        }
    }
}

pub fn strip_device_features(pd: *mut c_void, info: *const c_void) -> Option<Stripped> {
    if info.is_null() {
        return None;
    }
    strip_with(emulate_for(pd), info)
}

/// The decision and the rewrite, without the physical-device query, so the
/// chain handling can be tested.
fn strip_with(emulate: bool, info: *const c_void) -> Option<Stripped> {
    let enabled: *const c_void = rd(info, 64);
    let head: *const c_void = rd(info, 8);
    let f2 = chain_find(head, ST_PHYSICAL_DEVICE_FEATURES_2);
    let via_features = !enabled.is_null() && rd::<u32>(enabled, FEATURE_ETC2 * 4) != 0;
    let via_features2 = !f2.is_null() && rd::<u32>(f2, 16 + FEATURE_ETC2 * 4) != 0;
    trace(format_args!(
        "vkCreateDevice: pEnabledFeatures={} textureCompressionETC2 via pEnabledFeatures={} via Features2={} (Features2 in chain: {}, first node: {}); emulation {}",
        if enabled.is_null() { "null" } else { "set" },
        via_features,
        via_features2,
        !f2.is_null(),
        !f2.is_null() && f2 == head,
        if emulate { "on" } else { "off" },
    ));
    if !emulate || !(via_features || via_features2) {
        return None;
    }
    let mut s = Stripped { copy: None, restore: Vec::new() };
    if via_features || (via_features2 && f2 == head) {
        let mut c = Box::new(StrippedDeviceInfo { info: rd(info, 0), features: [0; FEATURE_COUNT], features2: [0; 240] });
        if via_features {
            c.features = rd(enabled, 0);
            c.features[FEATURE_ETC2] = 0;
            let p = c.features.as_ptr() as usize;
            c.info[64..72].copy_from_slice(&p.to_ne_bytes());
        }
        if via_features2 && f2 == head {
            c.features2 = rd(f2, 0);
            let at = 16 + FEATURE_ETC2 * 4;
            c.features2[at..at + 4].copy_from_slice(&0u32.to_ne_bytes());
            let p = c.features2.as_ptr() as usize;
            c.info[8..16].copy_from_slice(&p.to_ne_bytes());
        }
        s.copy = Some(c);
    }
    if via_features2 && f2 != head {
        // **Not first in the chain, so the chain cannot be copied.** The nodes
        // ahead of it are structures this module has no reason to know the size
        // of, and relinking around them needs their memory. Handing the driver
        // the feature switched on instead is `VK_ERROR_FEATURE_NOT_PRESENT` from
        // a driver that lacks it, and a client that never starts. So the one
        // word is cleared in place for the length of the call and restored by
        // `Stripped`'s drop; the engine's create info is read by nothing else
        // while the engine is inside this call.
        let at = (f2 as usize) + 16 + FEATURE_ETC2 * 4;
        // SAFETY: `f2` is a live `VkPhysicalDeviceFeatures2` in the caller's
        // chain, and the word is restored before `vkCreateDevice` returns.
        unsafe { std::ptr::write_unaligned(at as *mut u32, 0) };
        s.restore.push(at);
        trace(format_args!("vkCreateDevice: Features2 is not the first pNext node; its ETC2 bit is cleared in place for the call"));
    }
    trace(format_args!("vkCreateDevice: textureCompressionETC2 stripped before the host driver sees it"));
    Some(s)
}

#[repr(C)]
struct VkShaderModuleCreateInfo {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    flags: u32,
    _pad2: u32,
    code_size: usize,
    p_code: *const u32,
}

#[repr(C)]
struct VkDescriptorSetLayoutBinding {
    binding: u32,
    descriptor_type: u32,
    descriptor_count: u32,
    stage_flags: u32,
    p_immutable_samplers: *const u64,
}

#[repr(C)]
struct VkDescriptorSetLayoutCreateInfo {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    flags: u32,
    binding_count: u32,
    p_bindings: *const VkDescriptorSetLayoutBinding,
}

#[repr(C)]
struct VkPushConstantRange {
    stage_flags: u32,
    offset: u32,
    size: u32,
}

#[repr(C)]
struct VkPipelineLayoutCreateInfo {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    flags: u32,
    set_layout_count: u32,
    p_set_layouts: *const u64,
    push_constant_range_count: u32,
    _pad2: u32,
    p_push_constant_ranges: *const VkPushConstantRange,
}

#[repr(C)]
struct VkPipelineShaderStageCreateInfo {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    flags: u32,
    stage: u32,
    module: u64,
    p_name: *const c_char,
    p_specialization_info: *const c_void,
}

#[repr(C)]
struct VkComputePipelineCreateInfo {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    flags: u32,
    _pad2: u32,
    stage: VkPipelineShaderStageCreateInfo,
    layout: u64,
    base_pipeline_handle: u64,
    base_pipeline_index: i32,
    _pad3: u32,
}

#[repr(C)]
struct VkDescriptorPoolSize {
    ty: u32,
    descriptor_count: u32,
}

#[repr(C)]
struct VkDescriptorPoolCreateInfo {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    flags: u32,
    max_sets: u32,
    pool_size_count: u32,
    _pad2: u32,
    p_pool_sizes: *const VkDescriptorPoolSize,
}

#[repr(C)]
struct VkDescriptorSetAllocateInfo {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    descriptor_pool: u64,
    descriptor_set_count: u32,
    _pad2: u32,
    p_set_layouts: *const u64,
}

#[repr(C)]
struct VkDescriptorBufferInfo {
    buffer: u64,
    offset: u64,
    range: u64,
}

#[repr(C)]
struct VkWriteDescriptorSet {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    dst_set: u64,
    dst_binding: u32,
    dst_array_element: u32,
    descriptor_count: u32,
    descriptor_type: u32,
    p_image_info: *const c_void,
    p_buffer_info: *const VkDescriptorBufferInfo,
    p_texel_buffer_view: *const u64,
}

#[repr(C)]
struct VkBufferMemoryBarrier {
    s_type: u32,
    _pad: u32,
    p_next: *const c_void,
    src_access_mask: u32,
    dst_access_mask: u32,
    src_queue_family_index: u32,
    dst_queue_family_index: u32,
    buffer: u64,
    offset: u64,
    size: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VkQueueFamilyProperties {
    queue_flags: u32,
    queue_count: u32,
    timestamp_valid_bits: u32,
    min_image_transfer_granularity: [u32; 3],
}

#[repr(C)]
struct GpuPushConstants {
    src_byte_offset: u32,
    dst_word_offset: u32,
    width: u32,
    height: u32,
    row_blocks: u32,
    layer_stride: u32,
    dst_row_words: u32,
    fmt_and_layers: u32,
}

struct DsPool {
    pool: u64,
    used: u32,
    cap: u32,
}

struct GpuDecoder {
    shader_module: u64,
    ds_layout: u64,
    pipeline_layout: u64,
    pipeline: u64,
    compute_queue_families: Vec<bool>,
    destroy_shader_module: extern "C" fn(usize, u64, *const c_void),
    destroy_ds_layout: extern "C" fn(usize, u64, *const c_void),
    destroy_pipeline_layout: extern "C" fn(usize, u64, *const c_void),
    destroy_pipeline: extern "C" fn(usize, u64, *const c_void),
    create_ds_pool: extern "C" fn(usize, *const VkDescriptorPoolCreateInfo, *const c_void, *mut u64) -> i32,
    destroy_ds_pool: extern "C" fn(usize, u64, *const c_void),
    reset_ds_pool: extern "C" fn(usize, u64, u32) -> i32,
    alloc_ds: extern "C" fn(usize, *const VkDescriptorSetAllocateInfo, *mut u64) -> i32,
    update_ds: extern "C" fn(usize, u32, *const VkWriteDescriptorSet, u32, *const c_void),
    cmd_bind_pipeline: extern "C" fn(*mut c_void, u32, u64),
    cmd_bind_ds: extern "C" fn(*mut c_void, u32, u64, u32, u32, *const u64, u32, *const u32),
    cmd_push_constants: extern "C" fn(*mut c_void, u64, u32, u32, u32, *const c_void),
    cmd_dispatch: extern "C" fn(*mut c_void, u32, u32, u32),
    cmd_pipeline_barrier: extern "C" fn(
        *mut c_void, u32, u32, u32,
        u32, *const c_void,
        u32, *const VkBufferMemoryBarrier,
        u32, *const c_void,
    ),
    free_gpu_chunks: Mutex<Vec<Chunk>>,
    free_ds_pools: Mutex<Vec<DsPool>>,
}

impl GpuDecoder {
    fn init(
        pd: *mut c_void,
        device: usize,
        get: &dyn Fn(&CStr) -> usize,
    ) -> Option<Self> {
        if gpu_decode_disabled() {
            return None;
        }
        let qf_fn: extern "C" fn(*mut c_void, *mut u32, *mut VkQueueFamilyProperties) =
            as_fn(super::vulkan::host_instance_proc(c"vkGetPhysicalDeviceQueueFamilyProperties") as usize)?;
        let mut qf_count = 0u32;
        qf_fn(pd, &mut qf_count, std::ptr::null_mut());
        let mut qf_props = vec![VkQueueFamilyProperties::default(); qf_count as usize];
        if qf_count > 0 {
            qf_fn(pd, &mut qf_count, qf_props.as_mut_ptr());
        }
        let compute_queue_families: Vec<bool> =
            qf_props.iter().map(|q| q.queue_flags & QUEUE_COMPUTE_BIT != 0).collect();
        if !compute_queue_families.iter().any(|&c| c) {
            return None;
        }

        let create_shader_module: extern "C" fn(usize, *const VkShaderModuleCreateInfo, *const c_void, *mut u64) -> i32 =
            as_fn(get(c"vkCreateShaderModule"))?;
        let destroy_shader_module: extern "C" fn(usize, u64, *const c_void) =
            as_fn(get(c"vkDestroyShaderModule"))?;
        let create_ds_layout: extern "C" fn(usize, *const VkDescriptorSetLayoutCreateInfo, *const c_void, *mut u64) -> i32 =
            as_fn(get(c"vkCreateDescriptorSetLayout"))?;
        let destroy_ds_layout: extern "C" fn(usize, u64, *const c_void) =
            as_fn(get(c"vkDestroyDescriptorSetLayout"))?;
        let create_pipeline_layout: extern "C" fn(usize, *const VkPipelineLayoutCreateInfo, *const c_void, *mut u64) -> i32 =
            as_fn(get(c"vkCreatePipelineLayout"))?;
        let destroy_pipeline_layout: extern "C" fn(usize, u64, *const c_void) =
            as_fn(get(c"vkDestroyPipelineLayout"))?;
        let create_compute_pipelines: extern "C" fn(usize, u64, u32, *const VkComputePipelineCreateInfo, *const c_void, *mut u64) -> i32 =
            as_fn(get(c"vkCreateComputePipelines"))?;
        let destroy_pipeline: extern "C" fn(usize, u64, *const c_void) =
            as_fn(get(c"vkDestroyPipeline"))?;
        let create_ds_pool = as_fn(get(c"vkCreateDescriptorPool"))?;
        let destroy_ds_pool = as_fn(get(c"vkDestroyDescriptorPool"))?;
        let reset_ds_pool = as_fn(get(c"vkResetDescriptorPool"))?;
        let alloc_ds = as_fn(get(c"vkAllocateDescriptorSets"))?;
        let update_ds = as_fn(get(c"vkUpdateDescriptorSets"))?;
        let cmd_bind_pipeline = as_fn(get(c"vkCmdBindPipeline"))?;
        let cmd_bind_ds = as_fn(get(c"vkCmdBindDescriptorSets"))?;
        let cmd_push_constants = as_fn(get(c"vkCmdPushConstants"))?;
        let cmd_dispatch = as_fn(get(c"vkCmdDispatch"))?;
        let cmd_pipeline_barrier = as_fn(get(c"vkCmdPipelineBarrier"))?;

        let sm_ci = VkShaderModuleCreateInfo {
            s_type: ST_SHADER_MODULE_CREATE_INFO,
            _pad: 0,
            p_next: std::ptr::null(),
            flags: 0,
            _pad2: 0,
            code_size: std::mem::size_of_val(ETC_DECODE_SPV),
            p_code: ETC_DECODE_SPV.as_ptr(),
        };
        let mut shader_module = 0u64;
        if create_shader_module(device, &sm_ci, std::ptr::null(), &mut shader_module) != VK_SUCCESS {
            return None;
        }

        let bindings = [
            VkDescriptorSetLayoutBinding {
                binding: 0,
                descriptor_type: DESCRIPTOR_TYPE_STORAGE_BUFFER,
                descriptor_count: 1,
                stage_flags: SHADER_STAGE_COMPUTE_BIT,
                p_immutable_samplers: std::ptr::null(),
            },
            VkDescriptorSetLayoutBinding {
                binding: 1,
                descriptor_type: DESCRIPTOR_TYPE_STORAGE_BUFFER,
                descriptor_count: 1,
                stage_flags: SHADER_STAGE_COMPUTE_BIT,
                p_immutable_samplers: std::ptr::null(),
            },
        ];
        let dsl_ci = VkDescriptorSetLayoutCreateInfo {
            s_type: ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            _pad: 0,
            p_next: std::ptr::null(),
            flags: 0,
            binding_count: 2,
            p_bindings: bindings.as_ptr(),
        };
        let mut ds_layout = 0u64;
        if create_ds_layout(device, &dsl_ci, std::ptr::null(), &mut ds_layout) != VK_SUCCESS {
            destroy_shader_module(device, shader_module, std::ptr::null());
            return None;
        }

        let pc_range = VkPushConstantRange {
            stage_flags: SHADER_STAGE_COMPUTE_BIT,
            offset: 0,
            size: std::mem::size_of::<GpuPushConstants>() as u32,
        };
        let pl_ci = VkPipelineLayoutCreateInfo {
            s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
            _pad: 0,
            p_next: std::ptr::null(),
            flags: 0,
            set_layout_count: 1,
            p_set_layouts: &ds_layout,
            push_constant_range_count: 1,
            _pad2: 0,
            p_push_constant_ranges: &pc_range,
        };
        let mut pipeline_layout = 0u64;
        if create_pipeline_layout(device, &pl_ci, std::ptr::null(), &mut pipeline_layout) != VK_SUCCESS {
            destroy_ds_layout(device, ds_layout, std::ptr::null());
            destroy_shader_module(device, shader_module, std::ptr::null());
            return None;
        }

        let cp_ci = VkComputePipelineCreateInfo {
            s_type: ST_COMPUTE_PIPELINE_CREATE_INFO,
            _pad: 0,
            p_next: std::ptr::null(),
            flags: 0,
            _pad2: 0,
            stage: VkPipelineShaderStageCreateInfo {
                s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                _pad: 0,
                p_next: std::ptr::null(),
                flags: 0,
                stage: SHADER_STAGE_COMPUTE_BIT,
                module: shader_module,
                p_name: c"main".as_ptr(),
                p_specialization_info: std::ptr::null(),
            },
            layout: pipeline_layout,
            base_pipeline_handle: 0,
            base_pipeline_index: -1,
            _pad3: 0,
        };
        let mut pipeline = 0u64;
        if create_compute_pipelines(device, 0, 1, &cp_ci, std::ptr::null(), &mut pipeline) != VK_SUCCESS {
            destroy_pipeline_layout(device, pipeline_layout, std::ptr::null());
            destroy_ds_layout(device, ds_layout, std::ptr::null());
            destroy_shader_module(device, shader_module, std::ptr::null());
            return None;
        }

        Some(Self {
            shader_module,
            ds_layout,
            pipeline_layout,
            pipeline,
            compute_queue_families,
            destroy_shader_module,
            destroy_ds_layout,
            destroy_pipeline_layout,
            destroy_pipeline,
            create_ds_pool,
            destroy_ds_pool,
            reset_ds_pool,
            alloc_ds,
            update_ds,
            cmd_bind_pipeline,
            cmd_bind_ds,
            cmd_push_constants,
            cmd_dispatch,
            cmd_pipeline_barrier,
            free_gpu_chunks: Mutex::new(Vec::new()),
            free_ds_pools: Mutex::new(Vec::new()),
        })
    }

    fn destroy(&self, dev: &Dev) {
        let chunks: Vec<Chunk> = {
            let mut pool = self.free_gpu_chunks.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *pool)
        };
        for c in chunks {
            (dev.destroy_buffer)(dev.device, c.buffer, std::ptr::null());
            (dev.free_memory)(dev.device, c.memory, std::ptr::null());
        }
        let ds_pools: Vec<DsPool> = {
            let mut pool = self.free_ds_pools.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *pool)
        };
        for p in ds_pools {
            (self.destroy_ds_pool)(dev.device, p.pool, std::ptr::null());
        }
        (self.destroy_pipeline)(dev.device, self.pipeline, std::ptr::null());
        (self.destroy_pipeline_layout)(dev.device, self.pipeline_layout, std::ptr::null());
        (self.destroy_ds_layout)(dev.device, self.ds_layout, std::ptr::null());
        (self.destroy_shader_module)(dev.device, self.shader_module, std::ptr::null());
    }
}

struct Dev {
    device: usize,
    create_buffer: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32,
    destroy_buffer: extern "C" fn(usize, u64, *const c_void),
    buffer_requirements: extern "C" fn(usize, u64, *mut [u64; 3]),
    allocate_memory: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32,
    free_memory: extern "C" fn(usize, u64, *const c_void),
    bind_buffer_memory: extern "C" fn(usize, u64, u64, u64) -> i32,
    map_memory: extern "C" fn(usize, u64, u64, u64, u32, *mut *mut c_void) -> i32,
    copy_buffer_to_image: extern "C" fn(*mut c_void, u64, u64, i32, u32, *const c_void),
    memory_types: Vec<u32>,
    free_cpu_chunks: Mutex<Vec<Chunk>>,
    gpu: Option<GpuDecoder>,
}

static DEV: AtomicPtr<Dev> = AtomicPtr::new(std::ptr::null_mut());

pub fn device_created(pd: *mut c_void, device: *mut c_void) {
    if !emulate_for(pd) {
        return;
    }
    if !device_emulates(device) {
        emulating().push(device as usize);
        EMULATING_FAST.store(device as usize, Relaxed);
    }
    let gdpa = super::vulkan::host_instance_proc(c"vkGetDeviceProcAddr");
    let Some(gdpa) = as_fn::<extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>(gdpa as usize) else {
        unhandled(format_args!("no host vkGetDeviceProcAddr; ETC uploads cannot be decoded"));
        return;
    };
    let get = |n: &CStr| gdpa(device, n.as_ptr()) as usize;
    let memprops = super::vulkan::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties") as usize;
    let dev = (|| {
        let mut memory_types = Vec::new();
        let mp: extern "C" fn(*mut c_void, *mut c_void) = as_fn(memprops)?;
        let mut raw = [0u8; 520];
        mp(pd, raw.as_mut_ptr() as *mut c_void);
        let count = u32::from_ne_bytes(raw[0..4].try_into().ok()?);
        for i in 0..count.min(32) as usize {
            let o = 4 + i * 8;
            memory_types.push(u32::from_ne_bytes(raw[o..o + 4].try_into().ok()?));
        }
        let gpu = GpuDecoder::init(pd, device as usize, &get);
        Some(Dev {
            device: device as usize,
            create_buffer: as_fn(get(c"vkCreateBuffer"))?,
            destroy_buffer: as_fn(get(c"vkDestroyBuffer"))?,
            buffer_requirements: as_fn(get(c"vkGetBufferMemoryRequirements"))?,
            allocate_memory: as_fn(get(c"vkAllocateMemory"))?,
            free_memory: as_fn(get(c"vkFreeMemory"))?,
            bind_buffer_memory: as_fn(get(c"vkBindBufferMemory"))?,
            map_memory: as_fn(get(c"vkMapMemory"))?,
            copy_buffer_to_image: as_fn(get(c"vkCmdCopyBufferToImage"))?,
            memory_types,
            free_cpu_chunks: Mutex::new(Vec::new()),
            gpu,
        })
    })();
    match dev {
        Some(d) => {
            trace(format_args!(
                "device {device:p}: ETC2/EAC emulation active ({} memory types, gpu compute: {})",
                d.memory_types.len(),
                if d.gpu.is_some() { "on" } else { "off" }
            ));
            forget_device();
            DEV.store(Box::into_raw(Box::new(d)), Release);
        }
        None => unhandled(format_args!("device {device:p}: could not resolve the host calls staging needs")),
    }
}

fn active() -> Option<&'static Dev> {
    let p = DEV.load(Acquire);
    unsafe { p.as_ref() }
}

fn active_device(device: usize) -> Option<&'static Dev> {
    active().filter(|d| d.device == device)
}

fn forget_device() {
    let Some(dev) = active() else { return };
    let cmds: Vec<SharedCmd> = {
        let mut st = state_write();
        st.images.clear();
        st.buffers.clear();
        st.storage_buffers.clear();
        st.pool_compute.clear();
        st.cmd_pool.clear();
        st.pool_cmds.clear();
        st.compute_bound.clear();
        st.cmds.drain().map(|(_, c)| c).collect()
    };
    mapped_write().clear();
    LIVE_CMDS.store(0, Relaxed);
    LIVE_CPU_CMDS.store(0, Relaxed);
    LIVE_IMAGES.store(0, Relaxed);
    let n = cmds.len();
    for c in cmds {
        release_cmd(dev, c, false);
    }
    let pooled_cpu: Vec<Chunk> = {
        let mut pool = dev.free_cpu_chunks.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *pool)
    };
    for c in pooled_cpu {
        (dev.destroy_buffer)(dev.device, c.buffer, std::ptr::null());
        (dev.free_memory)(dev.device, c.memory, std::ptr::null());
    }
    if let Some(gpu) = dev.gpu.as_ref() {
        gpu.destroy(dev);
    }
    DEV.store(std::ptr::null_mut(), Release);
    trace(format_args!("device 0x{:x}: emulation state dropped, {n} command buffer(s) of staging freed", dev.device));
}

extern "C" fn destroy_device(device: *mut c_void, alloc: *const c_void) {
    if active_device(device as usize).is_some() {
        forget_device();
    }
    let mut list = emulating();
    list.retain(|&d| d != device as usize);
    EMULATING_FAST.store(list.last().copied().unwrap_or(0), Relaxed);
    drop(list);
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_DESTROY_DEVICE.load(Relaxed)) {
        f(device, alloc);
    }
}

#[derive(Clone, Copy)]
struct EmuImage {
    format: EtcFormat,
    etc: u32,
    substitute: u32,
    width: u32,
    height: u32,
}

struct Chunk {
    buffer: u64,
    memory: u64,
    mapped: usize,
    size: u64,
    used: u64,
}

struct Pending {
    memory: u64,
    mem_offset: u64,
    len: usize,
    hash: u64,
    decoded: bool,
    job: Job,
    work: Option<Arc<DecodeWork>>,
    error: Option<String>,
}

impl Pending {
    fn finish(&mut self) -> Result<(), String> {
        if let Some(work) = self.work.take() {
            if let Err(error) = DecodePool::get().wait(&work) {
                slice_mut(self.job.dst as *mut c_void, self.job.dst_len).fill(0);
                self.error = Some(error);
            } else {
                self.decoded = true;
                if let Some(hash) = work.snapshot_hash.get() {
                    self.hash = *hash;
                }
            }
        }
        self.error.clone().map_or(Ok(()), Err)
    }
}

#[derive(Clone, Copy)]
struct Job {
    format: EtcFormat,
    row_blocks: usize,
    rows_of_blocks: usize,
    layer_stride: usize,
    layers: usize,
    width: usize,
    height: usize,
    dst: usize,
    dst_len: usize,
}

#[derive(Default)]
struct CmdState {
    chunks: Vec<Chunk>,
    gpu_chunks: Vec<Chunk>,
    ds_pools: Vec<DsPool>,
    pending: Vec<Pending>,
    secondaries: Vec<usize>,
    has_cpu_work: bool,
    retired: bool,
}

type SharedCmd = Arc<Mutex<CmdState>>;

fn command(cmd: &SharedCmd) -> MutexGuard<'_, CmdState> {
    cmd.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct State {
    images: HashMap<u64, EmuImage>,
    buffers: HashMap<u64, (u64, u64)>,
    storage_buffers: HashSet<u64>,
    pool_compute: HashMap<u64, bool>,
    cmd_pool: HashMap<usize, u64>,
    pool_cmds: HashMap<u64, Vec<usize>>,
    compute_bound: HashSet<usize>,
    cmds: HashMap<usize, SharedCmd>,
}

fn state_lock() -> &'static RwLock<State> {
    static STATE: OnceLock<RwLock<State>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

fn state_read() -> RwLockReadGuard<'static, State> {
    state_lock().read().unwrap_or_else(|e| e.into_inner())
}

fn state_write() -> RwLockWriteGuard<'static, State> {
    state_lock().write().unwrap_or_else(|e| e.into_inner())
}

/// Separate read-mostly lock for active `vkMapMemory` ranges so snapshot copies
/// and submit-time hashes can run concurrently without blocking image/buffer/
/// command metadata, while `vkUnmapMemory` takes a write lock and waits for
/// in-flight readers before forwarding to the driver.
fn mapped_lock() -> &'static RwLock<HashMap<u64, (usize, u64, u64)>> {
    static MAPPED: OnceLock<RwLock<HashMap<u64, (usize, u64, u64)>>> = OnceLock::new();
    MAPPED.get_or_init(Default::default)
}

fn mapped_read() -> RwLockReadGuard<'static, HashMap<u64, (usize, u64, u64)>> {
    mapped_lock().read().unwrap_or_else(|e| e.into_inner())
}

fn mapped_write() -> RwLockWriteGuard<'static, HashMap<u64, (usize, u64, u64)>> {
    mapped_lock().write().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn state() -> RwLockWriteGuard<'static, State> {
    state_write()
}

static LIVE_CMDS: AtomicUsize = AtomicUsize::new(0);
static LIVE_CPU_CMDS: AtomicUsize = AtomicUsize::new(0);
static LIVE_IMAGES: AtomicUsize = AtomicUsize::new(0);

fn is_emulated(image: u64) -> Option<EmuImage> {
    if LIVE_IMAGES.load(Relaxed) == 0 {
        return None;
    }
    state_read().images.get(&image).copied()
}

/// A `VkDeviceImageMemoryRequirements` and the `VkImageCreateInfo` it points at,
/// copied with the format swapped for its substitute.
#[repr(C, align(8))]
struct SubstitutedImageInfo {
    info: [u8; 32],
    create: [u8; 88],
}

/// The copy, if `info` asks about an ETC2/EAC image. The memory requirements of
/// the image the driver will actually be given are the substitute's, and the
/// raw format would have the driver sizing a format it does not support.
fn substitute_image_info(info: *const c_void) -> Option<Box<SubstitutedImageInfo>> {
    if info.is_null() {
        return None;
    }
    let create: *const c_void = rd(info, 16);
    if create.is_null() {
        return None;
    }
    let (_, sub, _) = etc_format(rd(create, 24))?;
    let mut c = Box::new(SubstitutedImageInfo { info: rd(info, 0), create: rd(create, 0) });
    c.create[24..28].copy_from_slice(&sub.to_ne_bytes());
    let p = c.create.as_ptr() as usize;
    c.info[16..24].copy_from_slice(&p.to_ne_bytes());
    Some(c)
}

extern "C" fn get_device_image_requirements(device: *mut c_void, info: *const c_void, out: *mut c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut c_void)>(H_DEVICE_IMAGE_REQS.load(Relaxed))
    else {
        return;
    };
    match device_emulates(device).then(|| substitute_image_info(info)).flatten() {
        Some(c) => {
            trace(format_args!("vkGetDeviceImageMemoryRequirements: ETC2/EAC image sized as its substitute"));
            f(device, c.info.as_ptr() as *const c_void, out)
        }
        None => f(device, info, out),
    }
}

extern "C" fn get_device_image_sparse_requirements(
    device: *mut c_void,
    info: *const c_void,
    count: *mut u32,
    out: *mut c_void,
) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut u32, *mut c_void)>(
        H_DEVICE_IMAGE_SPARSE_REQS.load(Relaxed),
    ) else {
        return;
    };
    match device_emulates(device).then(|| substitute_image_info(info)).flatten() {
        Some(c) => f(device, c.info.as_ptr() as *const c_void, count, out),
        None => f(device, info, count, out),
    }
}

extern "C" fn create_image(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32>(H_CREATE_IMAGE.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() {
        return f(device, info, alloc, out);
    }
    let format: u32 = rd(info, 24);
    let Some((kind, sub, name)) = etc_format(format) else {
        return f(device, info, alloc, out);
    };
    let flags: u32 = rd(info, 16);
    let image_type: u32 = rd(info, 20);
    let extent: [u32; 3] = rd(info, 28);
    let (mips, layers, samples, tiling, usage): (u32, u32, u32, u32, u32) =
        (rd(info, 40), rd(info, 44), rd(info, 48), rd(info, 52), rd(info, 56));
    let describe = || {
        format!(
            "{name} type={image_type} {}x{}x{} mips={mips} layers={layers} samples={samples} tiling={tiling} usage=0x{usage:x} flags=0x{flags:x}",
            extent[0], extent[1], extent[2]
        )
    };
    if active_device(device as usize).is_none() {
        if device_emulates(device) {
            // The engine was told ETC2 works on this device and the state to
            // serve it is gone or never came up (`device_created` could not
            // resolve the staging calls, or a second emulating device replaced
            // this one's). Passing the raw format on is a driver that lacks it
            // being asked to create the image; failing here says so by name.
            unhandled(format_args!(
                "vkCreateImage({}): device {device:p} is emulating ETC2/EAC but has no emulation state; refused",
                describe()
            ));
            return VK_ERROR_FORMAT_NOT_SUPPORTED;
        }
        let rc = f(device, info, alloc, out);
        if crate::android::tracing() {
            trace(format_args!("vkCreateImage({}): passthrough rc={rc}", describe()));
        }
        return rc;
    }
    if !image_request_ok(tiling, usage, flags) {
        unhandled(format_args!("vkCreateImage({}): usage/tiling/flags beyond what is advertised", describe()));
    }
    if !chain_find(rd::<*const c_void>(info, 8), ST_IMAGE_FORMAT_LIST_CREATE_INFO).is_null() {
        unhandled(format_args!("vkCreateImage({}): VkImageFormatListCreateInfo is forwarded unchanged", describe()));
    }
    let mut copy: [u8; 88] = rd(info, 0);
    copy[24..28].copy_from_slice(&sub.to_ne_bytes());
    let rc = f(device, copy.as_ptr() as *const c_void, alloc, out);
    if rc == VK_SUCCESS && !out.is_null() {
        let image: u64 = rd(out as *const c_void, 0);
        state_write().images.insert(
            image,
            EmuImage { format: kind, etc: format, substitute: sub, width: extent[0], height: extent[1] },
        );
        LIVE_IMAGES.fetch_add(1, Relaxed);
        glcount::ETC_IMAGE_EMULATED.fetch_add(1, Relaxed);
        if crate::android::tracing() {
            trace(format_args!("vkCreateImage({}): EMULATED as format {sub}, image=0x{image:x}", describe()));
        }
    } else if crate::android::tracing() {
        trace(format_args!("vkCreateImage({}): substitute format {sub} FAILED rc={rc}", describe()));
    }
    rc
}

extern "C" fn destroy_image(device: *mut c_void, image: u64, alloc: *const c_void) {
    if LIVE_IMAGES.load(Relaxed) != 0 && state_write().images.remove(&image).is_some() {
        LIVE_IMAGES.fetch_sub(1, Relaxed);
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, *const c_void)>(H_DESTROY_IMAGE.load(Relaxed)) {
        f(device, image, alloc);
    }
}

extern "C" fn create_image_view(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32>(
        H_CREATE_IMAGE_VIEW.load(Relaxed),
    ) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() {
        return f(device, info, alloc, out);
    }
    let image: u64 = rd(info, 24);
    let Some(emu) = is_emulated(image) else {
        return f(device, info, alloc, out);
    };
    let view_format: u32 = rd(info, 36);
    let view_type: u32 = rd(info, 32);
    let sub = match etc_format(view_format) {
        Some((kind, sub, _)) if kind == emu.format => sub,
        _ => {
            unhandled(format_args!(
                "vkCreateImageView on emulated image 0x{image:x} (format {}) with view format {view_format}; forwarded unchanged",
                emu.etc
            ));
            return f(device, info, alloc, out);
        }
    };
    let mut copy: [u8; 80] = rd(info, 0);
    copy[36..40].copy_from_slice(&sub.to_ne_bytes());
    let rc = f(device, copy.as_ptr() as *const c_void, alloc, out);
    trace(format_args!(
        "vkCreateImageView(image=0x{image:x}, viewType={view_type}, format {view_format} -> {sub}): rc={rc}"
    ));
    rc
}

fn hash(data: &[u8]) -> u64 {
    let mut lanes = [
        0xcbf2_9ce4_8422_2325 ^ data.len() as u64,
        0x9e37_79b9_7f4a_7c15,
        0xd6e8_feb8_6659_fd93,
        0xa076_1d64_78bd_642f,
    ];
    let mut chunks = data.chunks_exact(32);
    for c in &mut chunks {
        for (lane, word) in lanes.iter_mut().zip(c.chunks_exact(8)) {
            let w = u64::from_le_bytes(word.try_into().unwrap());
            *lane = (*lane ^ w).wrapping_mul(0x0100_0000_01b3).rotate_left(29);
        }
    }
    let mut h = lanes.into_iter().fold(0u64, |h, lane| h.rotate_left(17) ^ lane);
    for &b in chunks.remainder() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

struct DecodeProgress {
    remaining: usize,
    error: Option<String>,
}

struct DecodeWork {
    job: Job,
    source: DecodeSource,
    snapshot_hash: OnceLock<u64>,
    band_rows: usize,
    bands_per_layer: usize,
    next: AtomicUsize,
    progress: Mutex<DecodeProgress>,
    finished: Condvar,
}

enum DecodeSource {
    #[cfg(test)]
    Borrowed(usize, usize),
    Snapshot(Box<[u8]>),
}

impl DecodeSource {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Snapshot(bytes) => bytes,
            // SAFETY: the synchronous pool caller joins every band before
            // releasing this borrow. Asynchronous jobs own snapshots instead.
            #[cfg(test)]
            Self::Borrowed(ptr, len) => unsafe { std::slice::from_raw_parts(*ptr as *const u8, *len) },
        }
    }
}

impl DecodeWork {
    fn claim(&self) -> Option<usize> {
        let band = self.next.fetch_add(1, Relaxed);
        (band < self.bands_per_layer * self.job.layers).then_some(band)
    }

    fn decode(&self, band: usize) -> Result<(), String> {
        let job = &self.job;
        let layer = band / self.bands_per_layer;
        let first = (band % self.bands_per_layer) * self.band_rows;
        let height = (job.height - first * 4).min(self.band_rows * 4);
        let offset = layer * job.layer_stride + first * job.row_blocks * job.format.block_bytes();
        let data = self.source.bytes().get(offset..).ok_or("band outside the source")?;
        let stride = job.width * job.format.texel_bytes();
        let start = (layer * job.height + first * 4) * stride;
        // SAFETY: claim assigns each band once; bands have disjoint output
        // rows. Pending jobs are joined before submit, staging release or
        // synchronous re-decode. Workers never acquire the global state lock.
        let dst = unsafe { std::slice::from_raw_parts_mut((job.dst as *mut u8).add(start), height * stride) };
        let surface = Surface {
            data,
            row_blocks: job.row_blocks,
            rows_of_blocks: job.rows_of_blocks.saturating_sub(first),
        };
        etc_decode::decode_region(job.format, &surface, job.width, height, dst)
    }

    fn perform(&self, band: usize) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if band == 0 {
                match &self.source {
                    DecodeSource::Snapshot(bytes) => { let _ = self.snapshot_hash.set(hash(bytes)); }
                    #[cfg(test)]
                    DecodeSource::Borrowed(..) => {}
                }
            }
            self.decode(band)
        }))
            .unwrap_or_else(|_| Err("decode worker panicked".into()));
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = result {
            progress.error.get_or_insert(error);
        }
        progress.remaining -= 1;
        if progress.remaining == 0 {
            self.finished.notify_all();
        }
    }
}

#[derive(Default)]
struct DecodeQueue {
    ready: Mutex<VecDeque<Arc<DecodeWork>>>,
    available: Condvar,
}

impl DecodeQueue {
    fn take(ready: &mut VecDeque<Arc<DecodeWork>>) -> Option<(Arc<DecodeWork>, usize)> {
        while let Some(work) = ready.pop_front() {
            if let Some(band) = work.claim() {
                // A large upload must not monopolise the queue ahead of
                // other command buffers.
                ready.push_back(Arc::clone(&work));
                return Some((work, band));
            }
        }
        None
    }
}

struct DecodePool {
    queue: Arc<DecodeQueue>,
    helpers: usize,
}

impl DecodePool {
    fn get() -> &'static Self {
        static POOL: OnceLock<DecodePool> = OnceLock::new();
        POOL.get_or_init(|| {
            let queue = Arc::new(DecodeQueue::default());
            let helpers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
            for i in 0..helpers {
                let queue = Arc::clone(&queue);
                let _ = std::thread::Builder::new().name(format!("ETC decode {i}")).spawn(move || loop {
                    let (work, band) = {
                        let mut ready = queue.ready.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(band) = DecodeQueue::take(&mut ready) {
                                break band;
                            }
                            ready = queue.available.wait(ready).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    work.perform(band);
                });
            }
            Self { queue, helpers }
        })
    }

    fn prepare(&self, job: &Job, source: DecodeSource) -> Arc<DecodeWork> {
        let block_rows = job.height.div_ceil(4);
        let max_bands = if job.width * job.height <= CPU_INLINE_TEXELS {
            1
        } else {
            self.helpers + 1
        };
        let band_rows = block_rows.div_ceil(max_bands).max(1);
        let bands_per_layer = block_rows.div_ceil(band_rows);
        Arc::new(DecodeWork {
            job: *job,
            source,
            snapshot_hash: OnceLock::new(),
            band_rows,
            bands_per_layer,
            next: AtomicUsize::new(0),
            progress: Mutex::new(DecodeProgress { remaining: bands_per_layer * job.layers, error: None }),
            finished: Condvar::new(),
        })
    }

    fn publish(&self, work: &Arc<DecodeWork>) {
        let total_bands = work.bands_per_layer * work.job.layers;
        self.queue.ready.lock().unwrap_or_else(|e| e.into_inner()).push_back(Arc::clone(work));
        if total_bands <= 1 {
            self.queue.available.notify_one();
        } else {
            self.queue.available.notify_all();
        }
    }

    fn enqueue(&self, job: &Job, snapshot: Box<[u8]>) -> Arc<DecodeWork> {
        let work = self.prepare(job, DecodeSource::Snapshot(snapshot));
        self.publish(&work);
        work
    }

    #[cfg(test)]
    fn run(&self, job: &Job, src: &[u8]) -> Result<(), String> {
        let work = self.prepare(job, DecodeSource::Borrowed(src.as_ptr() as usize, src.len()));
        // Claim before publication so the caller always contributes, even if
        // helpers are free. It also guarantees progress if thread creation fails.
        let first = work.claim().unwrap();
        self.publish(&work);
        work.perform(first);
        self.wait(&work)
    }

    fn wait(&self, work: &Arc<DecodeWork>) -> Result<(), String> {
        while let Some(band) = work.claim() {
            work.perform(band);
        }
        let mut progress = work.progress.lock().unwrap_or_else(|e| e.into_inner());
        while progress.remaining != 0 {
            progress = work.finished.wait(progress).unwrap_or_else(|e| e.into_inner());
        }
        // A queued Arc may outlive the borrowed buffers, but can only claim
        // exhausted work. Remove it here too in case no helper could start.
        self.queue.ready.lock().unwrap_or_else(|e| e.into_inner()).retain(|w| !Arc::ptr_eq(w, work));
        progress.error.clone().map_or(Ok(()), Err)
    }
}

#[cfg(test)]
fn run_job(job: &Job, src: &[u8]) -> Result<(), String> {
    validate_job(job, src)?;
    if job.width == 0 || job.height == 0 || job.layers == 0 {
        return Ok(());
    }
    let stride = job.width.checked_mul(job.format.texel_bytes()).ok_or("row size overflow")?;
    let per_layer = stride.checked_mul(job.height).ok_or("layer size overflow")?;
    let total = per_layer.checked_mul(job.layers).ok_or("output size overflow")?;
    if total > job.dst_len {
        return Err("destination is shorter than the job".into());
    }
    if job.width * job.height >= 256 * 256 {
        return DecodePool::get().run(job, src);
    }
    let dst = slice_mut(job.dst as *mut c_void, total);
    for (layer, out) in dst.chunks_mut(per_layer).enumerate() {
        let data = src.get(layer * job.layer_stride..).ok_or("layer outside the source")?;
        let surface = Surface { data, row_blocks: job.row_blocks, rows_of_blocks: job.rows_of_blocks };
        etc_decode::decode_region(job.format, &surface, job.width, job.height, out)?;
    }
    Ok(())
}

fn validate_job(job: &Job, src: &[u8]) -> Result<(), String> {
    if job.width == 0 || job.height == 0 || job.layers == 0 {
        return Ok(());
    }
    let bw = job.width.div_ceil(4);
    let bh = job.height.div_ceil(4);
    if bw > job.row_blocks || bh > job.rows_of_blocks {
        return Err("region exceeds the source block extent".into());
    }
    let need = (bh - 1).checked_mul(job.row_blocks).and_then(|n| n.checked_add(bw))
        .and_then(|n| n.checked_mul(job.format.block_bytes()))
        .and_then(|n| (job.layers - 1).checked_mul(job.layer_stride).and_then(|offset| offset.checked_add(n)))
        .ok_or("source size overflow")?;
    if src.len() < need {
        return Err("source is shorter than the job".into());
    }
    let total = job.width.checked_mul(job.height).and_then(|n| n.checked_mul(job.layers))
        .and_then(|n| n.checked_mul(job.format.texel_bytes())).ok_or("output size overflow")?;
    if job.dst_len < total {
        return Err("destination is shorter than the job".into());
    }
    Ok(())
}

fn source_bytes(
    mapped: &HashMap<u64, (usize, u64, u64)>,
    memory: u64,
    mem_offset: u64,
    len: usize,
) -> Result<&'static [u8], String> {
    let &(ptr, map_offset, map_size) =
        mapped.get(&memory).ok_or_else(|| format!("memory 0x{memory:x} is not mapped"))?;
    if mem_offset < map_offset
        || (map_size != WHOLE_SIZE && mem_offset + len as u64 > map_offset.saturating_add(map_size))
    {
        return Err(format!(
            "source range {mem_offset}+{len} lies outside the mapping {map_offset}+{map_size} of memory 0x{memory:x}"
        ));
    }
    Ok(slice((ptr + (mem_offset - map_offset) as usize) as *const c_void, len))
}

fn etc_format_code(format: EtcFormat) -> u32 {
    match format {
        EtcFormat::Rgb8 => 0,
        EtcFormat::Rgb8A1 => 1,
        EtcFormat::Rgba8 => 2,
        EtcFormat::R11 => 3,
        EtcFormat::R11Signed => 4,
        EtcFormat::Rg11 => 5,
        EtcFormat::Rg11Signed => 6,
    }
}

fn staging(dev: &Dev, cmd: &mut CmdState, len: u64) -> Result<(u64, u64, usize), String> {
    if let Some(c) = cmd.chunks.last_mut() {
        let at = c.used.div_ceil(STAGING_ALIGN) * STAGING_ALIGN;
        if at + len <= c.size {
            c.used = at + len;
            return Ok((c.buffer, at, c.mapped + at as usize));
        }
    }
    if len <= STAGING_CHUNK {
        let recycled = {
            let mut pool = dev.free_cpu_chunks.lock().unwrap_or_else(|e| e.into_inner());
            pool.pop()
        };
        if let Some(mut c) = recycled {
            if len <= c.size {
                let (buffer, mapped) = (c.buffer, c.mapped);
                c.used = len;
                cmd.chunks.push(c);
                return Ok((buffer, 0, mapped));
            }
            (dev.destroy_buffer)(dev.device, c.buffer, std::ptr::null());
            (dev.free_memory)(dev.device, c.memory, std::ptr::null());
        }
    }
    let size = len.max(STAGING_CHUNK);
    let mut bci = [0u8; 56];
    bci[0..4].copy_from_slice(&ST_BUFFER_CREATE_INFO.to_ne_bytes());
    bci[24..32].copy_from_slice(&size.to_ne_bytes());
    bci[32..36].copy_from_slice(&BUFFER_USAGE_TRANSFER_SRC.to_ne_bytes());
    let mut buffer = 0u64;
    let rc = (dev.create_buffer)(dev.device, bci.as_ptr() as *const c_void, std::ptr::null(), &mut buffer);
    if rc != VK_SUCCESS {
        return Err(format!("vkCreateBuffer({size}) rc={rc}"));
    }
    let mut req = [0u64; 3];
    (dev.buffer_requirements)(dev.device, buffer, &mut req);
    let bits = req[2] as u32;
    let wanted = MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT;
    let cached = wanted | MEMORY_HOST_CACHED;
    let pick = |req_flags: u32, avoid: u32| {
        dev.memory_types
            .iter()
            .enumerate()
            .find(|(i, &f)| bits & (1 << i) != 0 && f & req_flags == req_flags && f & avoid == 0)
            .map(|(i, _)| i as u32)
    };
    let Some(type_index) = pick(cached, MEMORY_DEVICE_LOCAL)
        .or_else(|| pick(cached, 0))
        .or_else(|| pick(wanted, MEMORY_DEVICE_LOCAL))
        .or_else(|| pick(wanted, 0))
    else {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        return Err("no host-visible coherent memory type for staging".into());
    };
    let mut mai = [0u8; 32];
    mai[0..4].copy_from_slice(&ST_MEMORY_ALLOCATE_INFO.to_ne_bytes());
    mai[16..24].copy_from_slice(&req[0].to_ne_bytes());
    mai[24..28].copy_from_slice(&type_index.to_ne_bytes());
    let mut memory = 0u64;
    let rc = (dev.allocate_memory)(dev.device, mai.as_ptr() as *const c_void, std::ptr::null(), &mut memory);
    if rc != VK_SUCCESS {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        return Err(format!("vkAllocateMemory({}) rc={rc}", req[0]));
    }
    let mut mapped = std::ptr::null_mut();
    let rc_bind = (dev.bind_buffer_memory)(dev.device, buffer, memory, 0);
    let rc_map = (dev.map_memory)(dev.device, memory, 0, WHOLE_SIZE, 0, &mut mapped);
    if rc_bind != VK_SUCCESS || rc_map != VK_SUCCESS || mapped.is_null() {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        (dev.free_memory)(dev.device, memory, std::ptr::null());
        return Err(format!("staging bind rc={rc_bind} map rc={rc_map}"));
    }
    cmd.chunks.push(Chunk { buffer, memory, mapped: mapped as usize, size, used: len });
    Ok((buffer, 0, mapped as usize))
}

fn gpu_staging(dev: &Dev, gpu: &GpuDecoder, cmd: &mut CmdState, len: u64) -> Result<(u64, u64), String> {
    if let Some(c) = cmd.gpu_chunks.last_mut() {
        let at = c.used.div_ceil(STAGING_ALIGN) * STAGING_ALIGN;
        if at + len <= c.size {
            c.used = at + len;
            return Ok((c.buffer, at));
        }
    }
    if len <= STAGING_CHUNK {
        let recycled = {
            let mut pool = gpu.free_gpu_chunks.lock().unwrap_or_else(|e| e.into_inner());
            pool.pop()
        };
        if let Some(mut c) = recycled {
            if len <= c.size {
                let buffer = c.buffer;
                c.used = len;
                cmd.gpu_chunks.push(c);
                return Ok((buffer, 0));
            }
            (dev.destroy_buffer)(dev.device, c.buffer, std::ptr::null());
            (dev.free_memory)(dev.device, c.memory, std::ptr::null());
        }
    }
    let size = len.max(STAGING_CHUNK);
    let mut bci = [0u8; 56];
    bci[0..4].copy_from_slice(&ST_BUFFER_CREATE_INFO.to_ne_bytes());
    bci[24..32].copy_from_slice(&size.to_ne_bytes());
    let usage = BUFFER_USAGE_TRANSFER_SRC | BUFFER_USAGE_STORAGE_BUFFER;
    bci[32..36].copy_from_slice(&usage.to_ne_bytes());
    let mut buffer = 0u64;
    let rc = (dev.create_buffer)(dev.device, bci.as_ptr() as *const c_void, std::ptr::null(), &mut buffer);
    if rc != VK_SUCCESS {
        return Err(format!("vkCreateBuffer(gpu scratch {size}) rc={rc}"));
    }
    let mut req = [0u64; 3];
    (dev.buffer_requirements)(dev.device, buffer, &mut req);
    let bits = req[2] as u32;
    let pick = |req_flags: u32| {
        dev.memory_types
            .iter()
            .enumerate()
            .find(|(i, &f)| bits & (1 << i) != 0 && f & req_flags == req_flags)
            .map(|(i, _)| i as u32)
    };
    let Some(type_index) = pick(MEMORY_DEVICE_LOCAL).or_else(|| pick(0)) else {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        return Err("no memory type for GPU scratch buffer".into());
    };
    let mut mai = [0u8; 32];
    mai[0..4].copy_from_slice(&ST_MEMORY_ALLOCATE_INFO.to_ne_bytes());
    mai[16..24].copy_from_slice(&req[0].to_ne_bytes());
    mai[24..28].copy_from_slice(&type_index.to_ne_bytes());
    let mut memory = 0u64;
    let rc = (dev.allocate_memory)(dev.device, mai.as_ptr() as *const c_void, std::ptr::null(), &mut memory);
    if rc != VK_SUCCESS {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        return Err(format!("vkAllocateMemory(gpu scratch {}) rc={rc}", req[0]));
    }
    let rc_bind = (dev.bind_buffer_memory)(dev.device, buffer, memory, 0);
    if rc_bind != VK_SUCCESS {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        (dev.free_memory)(dev.device, memory, std::ptr::null());
        return Err(format!("gpu scratch bind rc={rc_bind}"));
    }
    cmd.gpu_chunks.push(Chunk { buffer, memory, mapped: 0, size, used: len });
    Ok((buffer, 0))
}

fn alloc_ds(dev: &Dev, gpu: &GpuDecoder, cmd: &mut CmdState) -> Result<u64, String> {
    if let Some(p) = cmd.ds_pools.last_mut() {
        if p.used < p.cap {
            let ai = VkDescriptorSetAllocateInfo {
                s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
                _pad: 0,
                p_next: std::ptr::null(),
                descriptor_pool: p.pool,
                descriptor_set_count: 1,
                _pad2: 0,
                p_set_layouts: &gpu.ds_layout,
            };
            let mut ds = 0u64;
            if (gpu.alloc_ds)(dev.device, &ai, &mut ds) == VK_SUCCESS {
                p.used += 1;
                return Ok(ds);
            }
        }
    }
    let mut pool = {
        let mut free = gpu.free_ds_pools.lock().unwrap_or_else(|e| e.into_inner());
        free.pop()
    }
    .map(Ok)
    .unwrap_or_else(|| {
        let size = VkDescriptorPoolSize {
            ty: DESCRIPTOR_TYPE_STORAGE_BUFFER,
            descriptor_count: DS_POOL_SETS * 2,
        };
        let ci = VkDescriptorPoolCreateInfo {
            s_type: ST_DESCRIPTOR_POOL_CREATE_INFO,
            _pad: 0,
            p_next: std::ptr::null(),
            flags: 0,
            max_sets: DS_POOL_SETS,
            pool_size_count: 1,
            _pad2: 0,
            p_pool_sizes: &size,
        };
        let mut handle = 0u64;
        let rc = (gpu.create_ds_pool)(dev.device, &ci, std::ptr::null(), &mut handle);
        if rc != VK_SUCCESS {
            return Err(format!("vkCreateDescriptorPool rc={rc}"));
        }
        Ok(DsPool { pool: handle, used: 0, cap: DS_POOL_SETS })
    })?;
    let ai = VkDescriptorSetAllocateInfo {
        s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
        _pad: 0,
        p_next: std::ptr::null(),
        descriptor_pool: pool.pool,
        descriptor_set_count: 1,
        _pad2: 0,
        p_set_layouts: &gpu.ds_layout,
    };
    let mut ds = 0u64;
    let rc = (gpu.alloc_ds)(dev.device, &ai, &mut ds);
    if rc != VK_SUCCESS {
        (gpu.destroy_ds_pool)(dev.device, pool.pool, std::ptr::null());
        return Err(format!("vkAllocateDescriptorSets rc={rc}"));
    }
    pool.used = 1;
    cmd.ds_pools.push(pool);
    Ok(ds)
}

fn release_cmd(dev: &Dev, cmd: SharedCmd, recycle: bool) {
    let mut cmd = command(&cmd);
    cmd.retired = true;
    for mut pending in cmd.pending.drain(..) {
        if let Err(error) = pending.finish() {
            unhandled(format_args!("retiring command buffer: decode failed: {error}"));
        }
    }
    if cmd.has_cpu_work {
        cmd.has_cpu_work = false;
        LIVE_CPU_CMDS.fetch_sub(1, Relaxed);
    }
    for mut c in cmd.chunks.drain(..) {
        if recycle && c.size <= STAGING_CHUNK {
            c.used = 0;
            let mut pool = dev.free_cpu_chunks.lock().unwrap_or_else(|e| e.into_inner());
            if pool.len() < MAX_POOLED_CHUNKS {
                pool.push(c);
                continue;
            }
        }
        (dev.destroy_buffer)(dev.device, c.buffer, std::ptr::null());
        (dev.free_memory)(dev.device, c.memory, std::ptr::null());
    }
    if let Some(gpu) = dev.gpu.as_ref() {
        for mut c in cmd.gpu_chunks.drain(..) {
            if recycle && c.size <= STAGING_CHUNK {
                c.used = 0;
                let mut pool = gpu.free_gpu_chunks.lock().unwrap_or_else(|e| e.into_inner());
                if pool.len() < MAX_POOLED_CHUNKS {
                    pool.push(c);
                    continue;
                }
            }
            (dev.destroy_buffer)(dev.device, c.buffer, std::ptr::null());
            (dev.free_memory)(dev.device, c.memory, std::ptr::null());
        }
        for mut p in cmd.ds_pools.drain(..) {
            if recycle && (gpu.reset_ds_pool)(dev.device, p.pool, 0) == VK_SUCCESS {
                p.used = 0;
                let mut pool = gpu.free_ds_pools.lock().unwrap_or_else(|e| e.into_inner());
                if pool.len() < MAX_POOLED_DS_POOLS {
                    pool.push(p);
                    continue;
                }
            }
            (gpu.destroy_ds_pool)(dev.device, p.pool, std::ptr::null());
        }
    }
}

fn release_cmds(cmds: &[usize]) {
    if cmds.is_empty() {
        return;
    }
    if LIVE_CMDS.load(Relaxed) == 0 {
        if COMPUTE_BOUND_CMDS.load(Relaxed) != 0 {
            let mut st = state_write();
            for c in cmds {
                if st.compute_bound.remove(c) {
                    COMPUTE_BOUND_CMDS.fetch_sub(1, Relaxed);
                }
            }
        }
        return;
    }
    let Some(dev) = active() else { return };
    let mut released = Vec::new();
    {
        let mut st = state_write();
        for c in cmds {
            if st.compute_bound.remove(c) {
                COMPUTE_BOUND_CMDS.fetch_sub(1, Relaxed);
            }
            if let Some(s) = st.cmds.remove(c) {
                released.push(s);
            }
        }
    }
    if released.is_empty() {
        return;
    }
    LIVE_CMDS.fetch_sub(released.len(), Relaxed);
    if crate::android::tracing() {
        let bytes: u64 = released
            .iter()
            .map(|c| {
                let g = command(c);
                g.chunks.iter().chain(g.gpu_chunks.iter()).map(|c| c.size).sum::<u64>()
            })
            .sum();
        let regions: usize = released.iter().map(|c| command(c).pending.len()).sum();
        trace(format_args!(
            "staging released: {} command buffer(s), {regions} cpu region(s), {} KiB; {} still holding staging",
            released.len(),
            bytes / 1024,
            LIVE_CMDS.load(Relaxed)
        ));
    }
    for s in released {
        release_cmd(dev, s, true);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BufferImageCopy {
    buffer_offset: u64,
    buffer_row_length: u32,
    buffer_image_height: u32,
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
    image_offset: [i32; 3],
    image_extent: [u32; 3],
}

#[cfg(test)]
std::thread_local! {
    static SYNC_RECORD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn record_work(job: &Job, bytes: Box<[u8]>) -> Result<(Option<Arc<DecodeWork>>, bool, u64), String> {
    validate_job(job, &bytes)?;
    #[cfg(test)]
    if SYNC_RECORD.with(|mode| mode.get()) {
        let fingerprint = hash(&bytes);
        run_job(job, &bytes)?;
        return Ok((None, true, fingerprint));
    }
    if job.width == 0 || job.height == 0 {
        Ok((None, true, hash(&bytes)))
    } else {
        Ok((Some(DecodePool::get().prepare(job, DecodeSource::Snapshot(bytes))), false, 0))
    }
}

static COMPUTE_BOUND_CMDS: AtomicUsize = AtomicUsize::new(0);

fn can_gpu_decode(
    dev: &Dev,
    cmd: usize,
    src_buffer: u64,
    buffer_offset: u64,
    src_len: usize,
    layer_stride: usize,
    w: usize,
    h: usize,
    layers: usize,
) -> bool {
    if dev.gpu.is_none()
        || w == 0
        || h == 0
        || layers == 0
        || layers > 0x00FF_FFFF
        || buffer_offset % 4 != 0
        || layer_stride % 4 != 0
        || buffer_offset.checked_add(src_len as u64).is_none_or(|e| e > u32::MAX as u64)
    {
        return false;
    }
    let st = state_read();
    if !st.storage_buffers.contains(&src_buffer) || st.compute_bound.contains(&cmd) {
        return false;
    }
    st.cmd_pool
        .get(&cmd)
        .and_then(|pool| st.pool_compute.get(pool).copied())
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
fn emulate_copy_gpu(
    dev: &Dev,
    gpu: &GpuDecoder,
    entry: &mut CmdState,
    cmd: *mut c_void,
    src_buffer: u64,
    image: u64,
    fmt: EtcFormat,
    layout: i32,
    region: &BufferImageCopy,
    w: usize,
    h: usize,
    layers: usize,
    row_blocks: usize,
    layer_stride: usize,
    src_len: usize,
) -> Result<(), String> {
    let dst_row_words = match fmt {
        EtcFormat::R11 | EtcFormat::R11Signed => w.div_ceil(2),
        _ => w,
    };
    let dst_row_texels = match fmt {
        EtcFormat::R11 | EtcFormat::R11Signed => dst_row_words * 2,
        _ => w,
    };
    let dst_bytes = (dst_row_words as u64) * 4 * (h as u64) * (layers as u64);
    let (scratch_buf, scratch_off) = gpu_staging(dev, gpu, entry, dst_bytes)?;
    let ds = alloc_ds(dev, gpu, entry)?;

    let src_info = VkDescriptorBufferInfo { buffer: src_buffer, offset: 0, range: WHOLE_SIZE };
    let dst_info = VkDescriptorBufferInfo { buffer: scratch_buf, offset: 0, range: WHOLE_SIZE };
    let writes = [
        VkWriteDescriptorSet {
            s_type: ST_WRITE_DESCRIPTOR_SET,
            _pad: 0,
            p_next: std::ptr::null(),
            dst_set: ds,
            dst_binding: 0,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: DESCRIPTOR_TYPE_STORAGE_BUFFER,
            p_image_info: std::ptr::null(),
            p_buffer_info: &src_info,
            p_texel_buffer_view: std::ptr::null(),
        },
        VkWriteDescriptorSet {
            s_type: ST_WRITE_DESCRIPTOR_SET,
            _pad: 0,
            p_next: std::ptr::null(),
            dst_set: ds,
            dst_binding: 1,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: DESCRIPTOR_TYPE_STORAGE_BUFFER,
            p_image_info: std::ptr::null(),
            p_buffer_info: &dst_info,
            p_texel_buffer_view: std::ptr::null(),
        },
    ];
    (gpu.update_ds)(dev.device, writes.len() as u32, writes.as_ptr(), 0, std::ptr::null());

    // Make prior host or transfer writes to `src_buffer` visible to compute.
    let pre = VkBufferMemoryBarrier {
        s_type: ST_BUFFER_MEMORY_BARRIER,
        _pad: 0,
        p_next: std::ptr::null(),
        src_access_mask: ACCESS_HOST_WRITE_BIT | ACCESS_TRANSFER_WRITE_BIT,
        dst_access_mask: ACCESS_SHADER_READ_BIT,
        src_queue_family_index: QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: QUEUE_FAMILY_IGNORED,
        buffer: src_buffer,
        offset: region.buffer_offset,
        size: (src_len as u64).max(4),
    };
    (gpu.cmd_pipeline_barrier)(
        cmd,
        PIPELINE_STAGE_HOST_BIT | PIPELINE_STAGE_TRANSFER_BIT,
        PIPELINE_STAGE_COMPUTE_SHADER_BIT,
        0,
        0,
        std::ptr::null(),
        1,
        &pre,
        0,
        std::ptr::null(),
    );

    let pc = GpuPushConstants {
        src_byte_offset: region.buffer_offset as u32,
        dst_word_offset: (scratch_off / 4) as u32,
        width: w as u32,
        height: h as u32,
        row_blocks: row_blocks as u32,
        layer_stride: layer_stride as u32,
        dst_row_words: dst_row_words as u32,
        fmt_and_layers: etc_format_code(fmt) | ((layers as u32) << 8),
    };
    (gpu.cmd_bind_pipeline)(cmd, PIPELINE_BIND_POINT_COMPUTE, gpu.pipeline);
    (gpu.cmd_bind_ds)(cmd, PIPELINE_BIND_POINT_COMPUTE, gpu.pipeline_layout, 0, 1, &ds, 0, std::ptr::null());
    (gpu.cmd_push_constants)(
        cmd,
        gpu.pipeline_layout,
        SHADER_STAGE_COMPUTE_BIT,
        0,
        std::mem::size_of::<GpuPushConstants>() as u32,
        &pc as *const GpuPushConstants as *const c_void,
    );
    (gpu.cmd_dispatch)(
        cmd,
        (w.div_ceil(4).div_ceil(8)) as u32,
        (h.div_ceil(4).div_ceil(8)) as u32,
        layers as u32,
    );

    // Synchronise compute shader writes into scratch before copying to the image.
    let post = VkBufferMemoryBarrier {
        s_type: ST_BUFFER_MEMORY_BARRIER,
        _pad: 0,
        p_next: std::ptr::null(),
        src_access_mask: ACCESS_SHADER_WRITE_BIT,
        dst_access_mask: ACCESS_TRANSFER_READ_BIT,
        src_queue_family_index: QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: QUEUE_FAMILY_IGNORED,
        buffer: scratch_buf,
        offset: scratch_off,
        size: dst_bytes,
    };
    (gpu.cmd_pipeline_barrier)(
        cmd,
        PIPELINE_STAGE_COMPUTE_SHADER_BIT,
        PIPELINE_STAGE_TRANSFER_BIT,
        0,
        0,
        std::ptr::null(),
        1,
        &post,
        0,
        std::ptr::null(),
    );

    let out = BufferImageCopy {
        buffer_offset: scratch_off,
        buffer_row_length: if dst_row_texels == w { 0 } else { dst_row_texels as u32 },
        buffer_image_height: 0,
        ..*region
    };
    (dev.copy_buffer_to_image)(cmd, scratch_buf, image, layout, 1, &out as *const BufferImageCopy as *const c_void);
    glcount::ETC_REGION_DECODED.fetch_add(1, Relaxed);
    glcount::ETC_REGION_GPU.fetch_add(1, Relaxed);
    Ok(())
}

fn emulate_copy(
    dev: &Dev,
    cmd: *mut c_void,
    src_buffer: u64,
    image: u64,
    emu: EmuImage,
    layout: i32,
    region: &BufferImageCopy,
) -> Result<String, String> {
    let fmt = emu.format;
    let [w, h, d] = region.image_extent;
    if d != 1 {
        return Err(format!("depth {d} copies are not emulated"));
    }
    let (w, h) = (w as usize, h as usize);
    let row_texels = if region.buffer_row_length == 0 { w } else { region.buffer_row_length as usize };
    let img_texels = if region.buffer_image_height == 0 { h } else { region.buffer_image_height as usize };
    let row_blocks = row_texels.div_ceil(4);
    let rows_of_blocks = img_texels.div_ceil(4);
    let layer_stride = row_blocks * rows_of_blocks * fmt.block_bytes();
    let layers = region.layer_count.max(1) as usize;
    let last_layer = (h.div_ceil(4).saturating_sub(1) * row_blocks + w.div_ceil(4)) * fmt.block_bytes();
    let src_len = (layers - 1) * layer_stride + last_layer;
    let dst_len = w * h * layers * fmt.texel_bytes();

    let shared = {
        let existing = state_read().cmds.get(&(cmd as usize)).cloned();
        match existing {
            Some(c) => c,
            None => {
                let mut st = state_write();
                Arc::clone(st.cmds.entry(cmd as usize).or_insert_with(|| {
                    LIVE_CMDS.fetch_add(1, Relaxed);
                    SharedCmd::default()
                }))
            }
        }
    };
    let mut entry = command(&shared);
    if entry.retired {
        return Err("command buffer was retired during recording".into());
    }

    if can_gpu_decode(dev, cmd as usize, src_buffer, region.buffer_offset, src_len, layer_stride, w, h, layers) {
        if let Some(gpu) = dev.gpu.as_ref() {
            if emulate_copy_gpu(
                dev, gpu, &mut entry, cmd, src_buffer, image, fmt, layout, region,
                w, h, layers, row_blocks, layer_stride, src_len,
            ).is_ok() {
                return Ok(if crate::android::tracing() {
                    format!(
                        "gpu compute: src buffer 0x{src_buffer:x}+{} {src_len} B, rowLength={} imageHeight={} -> {dst_len} B",
                        region.buffer_offset, region.buffer_row_length, region.buffer_image_height
                    )
                } else {
                    String::new()
                });
            }
        }
    }

    let (buffer, offset, dst) = staging(dev, &mut entry, dst_len as u64)?;
    let job = Job { format: fmt, row_blocks, rows_of_blocks, layer_stride, layers, width: w, height: h, dst, dst_len };
    let (memory, bind_offset) = {
        let st = state_read();
        *st.buffers
            .get(&src_buffer)
            .ok_or_else(|| format!("source buffer 0x{src_buffer:x} has no recorded binding"))?
    };
    let mem_offset = bind_offset + region.buffer_offset;
    // Pinning a mapping does not prevent legal writes after recording. Taking
    // a shared read lock on `MAPPED` lets concurrent uploads copy in parallel
    // without blocking image/buffer metadata, while `vkUnmapMemory` takes a
    // write lock and waits for in-flight readers.
    let snapshot = {
        let mapped = mapped_read();
        source_bytes(&mapped, memory, mem_offset, src_len).map(|bytes| bytes.to_vec().into_boxed_slice())
    };
    let (work, decoded, h64, note) = match snapshot {
        Ok(bytes) => {
            let (work, decoded, hash) = record_work(&job, bytes)?;
            (work, decoded, hash, "prepared snapshot for decode")
        }
        Err(e) => {
            slice_mut(dst as *mut c_void, dst_len).fill(0);
            unhandled(format_args!("source not readable at record time ({e}); will retry at submit"));
            (None, false, 0, "deferred to submit")
        }
    };
    if !entry.has_cpu_work {
        entry.has_cpu_work = true;
        LIVE_CPU_CMDS.fetch_add(1, Relaxed);
    }
    entry.pending.push(Pending { memory, mem_offset, len: src_len, hash: h64, decoded, job, work: work.clone(), error: None });
    let out = BufferImageCopy { buffer_offset: offset, buffer_row_length: 0, buffer_image_height: 0, ..*region };
    (dev.copy_buffer_to_image)(cmd, buffer, image, layout, 1, &out as *const BufferImageCopy as *const c_void);
    if let Some(work) = work {
        DecodePool::get().publish(&work);
    }
    glcount::ETC_REGION_DECODED.fetch_add(1, Relaxed);
    Ok(if crate::android::tracing() {
        format!(
            "{note}: src buffer 0x{src_buffer:x} mem 0x{memory:x}+{mem_offset} {src_len} B, rowLength={} imageHeight={} -> staging 0x{buffer:x}+{offset} {dst_len} B",
            region.buffer_row_length, region.buffer_image_height
        )
    } else {
        String::new()
    })
}

type CopyBufferToImageFn = extern "C" fn(*mut c_void, u64, u64, i32, u32, *const c_void);

extern "C" fn cmd_copy_buffer_to_image(
    cmd: *mut c_void,
    src: u64,
    image: u64,
    layout: i32,
    count: u32,
    regions: *const c_void,
) {
    let Some(f) = as_fn::<CopyBufferToImageFn>(H_CMD_COPY_BUFFER_TO_IMAGE.load(Relaxed)) else { return };
    let (Some(emu), Some(dev)) = (is_emulated(image), active()) else {
        return f(cmd, src, image, layout, count, regions);
    };
    if count == 1 {
        let r: BufferImageCopy = rd(regions, 0);
        copy_regions(dev, cmd, src, image, emu, layout, std::slice::from_ref(&r), "vkCmdCopyBufferToImage");
        return;
    }
    let regions: Vec<BufferImageCopy> =
        (0..count as usize).map(|i| rd(regions, i * std::mem::size_of::<BufferImageCopy>())).collect();
    copy_regions(dev, cmd, src, image, emu, layout, &regions, "vkCmdCopyBufferToImage");
}

#[allow(clippy::too_many_arguments)]
fn copy_regions(
    dev: &Dev,
    cmd: *mut c_void,
    src: u64,
    image: u64,
    emu: EmuImage,
    layout: i32,
    regions: &[BufferImageCopy],
    via: &str,
) {
    let tracing = crate::android::tracing();
    for r in regions {
        let fmt_what = || {
            format!(
                "{via}(image=0x{image:x} {}x{} fmt={}->{} mip={} layers={}+{} offset={:?} extent={:?} bufferOffset={})",
                emu.width,
                emu.height,
                emu.etc,
                emu.substitute,
                r.mip_level,
                r.base_array_layer,
                r.layer_count,
                r.image_offset,
                r.image_extent,
                r.buffer_offset
            )
        };
        match emulate_copy(dev, cmd, src, image, emu, layout, r) {
            Ok(how) => {
                if tracing {
                    trace(format_args!("{}: {how}", fmt_what()));
                }
            }
            Err(e) => unhandled(format_args!("{}: region skipped, {e}", fmt_what())),
        }
    }
}

extern "C" fn cmd_copy_buffer_to_image2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_CMD_COPY_BUFFER_TO_IMAGE2.load(Relaxed))
    else {
        return;
    };
    if info.is_null() {
        return f(cmd, info);
    }
    let image: u64 = rd(info, 24);
    let (Some(emu), Some(dev)) = (is_emulated(image), active()) else {
        return f(cmd, info);
    };
    let src: u64 = rd(info, 16);
    let layout: i32 = rd(info, 32);
    let count: u32 = rd(info, 36);
    let p: *const c_void = rd(info, 40);
    let regions: Vec<BufferImageCopy> = (0..count as usize)
        .map(|i| {
            let r = (p as *const u8).wrapping_add(i * 72) as *const c_void;
            if !rd::<*const c_void>(r, 8).is_null() {
                unhandled(format_args!("vkCmdCopyBufferToImage2 region with a pNext chain; the chain is ignored"));
            }
            BufferImageCopy {
                buffer_offset: rd(r, 16),
                buffer_row_length: rd(r, 24),
                buffer_image_height: rd(r, 28),
                aspect_mask: rd(r, 32),
                mip_level: rd(r, 36),
                base_array_layer: rd(r, 40),
                layer_count: rd(r, 44),
                image_offset: rd(r, 48),
                image_extent: rd(r, 60),
            }
        })
        .collect();
    copy_regions(dev, cmd, src, image, emu, layout, &regions, "vkCmdCopyBufferToImage2");
}

extern "C" fn cmd_copy_image(
    cmd: *mut c_void,
    src: u64,
    src_layout: i32,
    dst: u64,
    dst_layout: i32,
    count: u32,
    regions: *const c_void,
) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, i32, u64, i32, u32, *const c_void)>(
        H_CMD_COPY_IMAGE.load(Relaxed),
    ) else {
        return;
    };
    if check_image_pair("vkCmdCopyImage", src, dst) {
        f(cmd, src, src_layout, dst, dst_layout, count, regions);
    }
}

extern "C" fn cmd_copy_image2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_CMD_COPY_IMAGE2.load(Relaxed)) else {
        return;
    };
    if info.is_null() || check_image_pair("vkCmdCopyImage2", rd(info, 16), rd(info, 32)) {
        f(cmd, info);
    }
}

fn check_image_pair(via: &str, src: u64, dst: u64) -> bool {
    let (s, d) = (is_emulated(src), is_emulated(dst));
    match (s, d) {
        (None, None) => true,
        (Some(a), Some(b)) if a.format == b.format => {
            trace(format_args!(
                "{via}(0x{src:x} -> 0x{dst:x}): both emulated as the same format, copied texel for texel"
            ));
            true
        }
        _ => {
            unhandled(format_args!(
                "{via}(0x{src:x} -> 0x{dst:x}): an emulated ETC image copied against a different format; skipped"
            ));
            false
        }
    }
}

extern "C" fn cmd_blit_image(
    cmd: *mut c_void,
    src: u64,
    src_layout: i32,
    dst: u64,
    dst_layout: i32,
    count: u32,
    regions: *const c_void,
    filter: i32,
) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, i32, u64, i32, u32, *const c_void, i32)>(
        H_CMD_BLIT_IMAGE.load(Relaxed),
    ) else {
        return;
    };
    note_blit("vkCmdBlitImage", src, dst);
    f(cmd, src, src_layout, dst, dst_layout, count, regions, filter);
}

extern "C" fn cmd_blit_image2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_CMD_BLIT_IMAGE2.load(Relaxed)) else {
        return;
    };
    if !info.is_null() {
        note_blit("vkCmdBlitImage2", rd(info, 16), rd(info, 32));
    }
    f(cmd, info);
}

fn note_blit(via: &str, src: u64, dst: u64) {
    if is_emulated(src).is_some() || is_emulated(dst).is_some() {
        unhandled(format_args!(
            "{via}(0x{src:x} -> 0x{dst:x}) touches an emulated ETC image; BLIT is not advertised, forwarded on the substitute"
        ));
    }
}

extern "C" fn cmd_copy_image_to_buffer(
    cmd: *mut c_void,
    image: u64,
    layout: i32,
    dst: u64,
    count: u32,
    regions: *const c_void,
) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, i32, u64, u32, *const c_void)>(
        H_CMD_COPY_IMAGE_TO_BUFFER.load(Relaxed),
    ) else {
        return;
    };
    if is_emulated(image).is_some() {
        unhandled(format_args!(
            "vkCmdCopyImageToBuffer from emulated ETC image 0x{image:x}: the substitute holds decoded texels, skipped"
        ));
        return;
    }
    f(cmd, image, layout, dst, count, regions);
}

extern "C" fn cmd_copy_image_to_buffer2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_CMD_COPY_IMAGE_TO_BUFFER2.load(Relaxed))
    else {
        return;
    };
    if !info.is_null() {
        let image: u64 = rd(info, 16);
        if is_emulated(image).is_some() {
            unhandled(format_args!(
                "vkCmdCopyImageToBuffer2 from emulated ETC image 0x{image:x}: the substitute holds decoded texels, skipped"
            ));
            return;
        }
    }
    f(cmd, info);
}

extern "C" fn create_buffer(
    device: *mut c_void,
    info: *const c_void,
    alloc: *const c_void,
    out: *mut u64,
) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32>(
        H_CREATE_BUFFER.load(Relaxed),
    ) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() || out.is_null() {
        return f(device, info, alloc, out);
    }
    let Some(dev) = active_device(device as usize) else {
        return f(device, info, alloc, out);
    };
    let usage: u32 = rd(info, 32);
    if dev.gpu.is_some() && usage & BUFFER_USAGE_TRANSFER_SRC != 0 {
        let mut added = false;
        let rc = if usage & BUFFER_USAGE_STORAGE_BUFFER == 0 {
            let mut copy: [u8; 56] = rd(info, 0);
            let patched = usage | BUFFER_USAGE_STORAGE_BUFFER;
            copy[32..36].copy_from_slice(&patched.to_ne_bytes());
            let rc = f(device, copy.as_ptr() as *const c_void, alloc, out);
            if rc == VK_SUCCESS {
                added = true;
                rc
            } else {
                // Fall back to the caller's exact usage flags if the driver rejects
                // STORAGE_BUFFER on this buffer (e.g. external memory constraints).
                f(device, info, alloc, out)
            }
        } else {
            added = true;
            f(device, info, alloc, out)
        };
        if rc == VK_SUCCESS && added {
            let buffer: u64 = rd(out as *const c_void, 0);
            state_write().storage_buffers.insert(buffer);
        }
        return rc;
    }
    f(device, info, alloc, out)
}

extern "C" fn bind_buffer_memory(device: *mut c_void, buffer: u64, memory: u64, offset: u64) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, u64, u64) -> i32>(H_BIND_BUFFER_MEMORY.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, buffer, memory, offset);
    if rc == VK_SUCCESS && active_device(device as usize).is_some() {
        state_write().buffers.insert(buffer, (memory, offset));
    }
    rc
}

extern "C" fn bind_buffer_memory2(device: *mut c_void, count: u32, infos: *const c_void) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u32, *const c_void) -> i32>(H_BIND_BUFFER_MEMORY2.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, count, infos);
    if rc == VK_SUCCESS && !infos.is_null() && active_device(device as usize).is_some() {
        let mut st = state_write();
        for i in 0..count as usize {
            let base = (infos as *const u8).wrapping_add(i * 40) as *const c_void;
            st.buffers.insert(rd(base, 16), (rd(base, 24), rd(base, 32)));
        }
    }
    rc
}

extern "C" fn destroy_buffer(device: *mut c_void, buffer: u64, alloc: *const c_void) {
    if active_device(device as usize).is_some() {
        let mut st = state_write();
        st.buffers.remove(&buffer);
        st.storage_buffers.remove(&buffer);
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, *const c_void)>(H_DESTROY_BUFFER.load(Relaxed)) {
        f(device, buffer, alloc);
    }
}

extern "C" fn map_memory(
    device: *mut c_void,
    memory: u64,
    offset: u64,
    size: u64,
    flags: u32,
    out: *mut *mut c_void,
) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, u64, u64, u32, *mut *mut c_void) -> i32>(
        H_MAP_MEMORY.load(Relaxed),
    ) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, memory, offset, size, flags, out);
    if rc == VK_SUCCESS && !out.is_null() && active_device(device as usize).is_some() {
        let p: usize = rd(out as *const c_void, 0);
        mapped_write().insert(memory, (p, offset, size));
    }
    rc
}

extern "C" fn map_memory2(device: *mut c_void, info: *const c_void, out: *mut *mut c_void) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32>(H_MAP_MEMORY2.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, info, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() && active_device(device as usize).is_some() {
        let p: usize = rd(out as *const c_void, 0);
        mapped_write().insert(rd(info, 24), (p, rd(info, 32), rd(info, 40)));
    }
    rc
}

extern "C" fn unmap_memory(device: *mut c_void, memory: u64) {
    if active_device(device as usize).is_some() {
        retire_mapping(memory);
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64)>(H_UNMAP_MEMORY.load(Relaxed)) {
        f(device, memory);
    }
}

extern "C" fn free_memory(device: *mut c_void, memory: u64, alloc: *const c_void) {
    if active_device(device as usize).is_some() {
        retire_mapping(memory);
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, *const c_void)>(H_FREE_MEMORY.load(Relaxed)) {
        f(device, memory, alloc);
    }
}

fn retire_mapping(memory: u64) {
    mapped_write().remove(&memory);
    if LIVE_CPU_CMDS.load(Relaxed) == 0 {
        return;
    }
    let cmds: Vec<_> = state_read().cmds.values().cloned().collect();
    for cmd in cmds {
        for p in command(&cmd).pending.iter_mut().filter(|p| p.memory == memory) {
            if let Err(error) = p.finish() {
                unhandled(format_args!("retiring mapping: decode failed: {error}"));
            }
        }
    }
}

fn check_command(c: usize, same: &mut usize, gone: &mut usize) -> Vec<usize> {
    let Some(shared) = state_read().cmds.get(&c).cloned() else { return Vec::new() };
    let mut cs = command(&shared);
    if cs.retired || (cs.pending.is_empty() && cs.secondaries.is_empty()) {
        return Vec::new();
    }
    for p in &mut cs.pending {
        if let Err(error) = p.finish() {
            unhandled(format_args!("submit: decode failed: {error}; texels cleared"));
            continue;
        }
        let snapshot = {
            let mapped = mapped_read();
            source_bytes(&mapped, p.memory, p.mem_offset, p.len).map(|bytes| {
                let h = hash(bytes);
                if p.decoded && h == p.hash { None } else { Some(bytes.to_vec().into_boxed_slice()) }
            })
        };
        let bytes = match snapshot {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                *same += 1;
                continue;
            }
            Err(e) => {
                *gone += 1;
                if !p.decoded {
                    unhandled(format_args!("submit: deferred ETC region still unreadable ({e}); texels stay zero"));
                    p.decoded = true;
                }
                continue;
            }
        };
        match validate_job(&p.job, &bytes) {
            Ok(()) => {
                if p.decoded {
                    glcount::ETC_LATE_WRITE.fetch_add(1, Relaxed);
                }
                trace(format_args!(
                    "submit: ETC region re-decoded ({}), mem 0x{:x}+{} {} B",
                    if p.decoded { "source changed after recording" } else { "deferred at record" },
                    p.memory,
                    p.mem_offset,
                    p.len
                ));
                if p.job.width == 0 || p.job.height == 0 {
                    p.decoded = true;
                    p.hash = hash(&bytes);
                } else {
                    p.work = Some(DecodePool::get().enqueue(&p.job, bytes));
                }
            }
            Err(e) => unhandled(format_args!("submit: decode failed: {e}")),
        }
    }
    for p in &mut cs.pending {
        if p.work.is_some() {
            if let Err(error) = p.finish() {
                unhandled(format_args!("submit: decode failed: {error}; texels cleared"));
            }
        }
    }
    cs.secondaries.clone()
}

fn check_pending(cmds: impl Iterator<Item = usize>) {
    if LIVE_CPU_CMDS.load(Relaxed) == 0 {
        return;
    }
    let (mut same, mut gone) = (0usize, 0usize);
    for c in cmds {
        let mut children = check_command(c, &mut same, &mut gone);
        if children.is_empty() {
            continue;
        }
        let mut visited = HashSet::from([c]);
        while let Some(child) = children.pop() {
            if visited.insert(child) {
                children.extend(check_command(child, &mut same, &mut gone));
            }
        }
    }
    if same + gone > 0 {
        trace(format_args!(
            "submit: {same} ETC region(s) verified unchanged since recording, {gone} whose source was no longer mapped"
        ));
    }
}

extern "C" fn cmd_execute_commands(cmd: *mut c_void, count: u32, secondaries: *const usize) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u32, *const usize)>(H_CMD_EXECUTE_COMMANDS.load(Relaxed)) else {
        return;
    };
    if LIVE_CPU_CMDS.load(Relaxed) != 0 && !secondaries.is_null() {
        let children: Vec<usize> = {
            let st = state_read();
            (0..count as usize)
                .map(|i| rd(secondaries as *const c_void, i * 8))
                .filter(|c| st.cmds.contains_key(c))
                .collect()
        };
        if !children.is_empty() {
            let primary = {
                let mut st = state_write();
                Arc::clone(st.cmds.entry(cmd as usize).or_insert_with(|| {
                    LIVE_CMDS.fetch_add(1, Relaxed);
                    SharedCmd::default()
                }))
            };
            let mut primary = command(&primary);
            if !primary.retired {
                if !primary.has_cpu_work {
                    primary.has_cpu_work = true;
                    LIVE_CPU_CMDS.fetch_add(1, Relaxed);
                }
                // Joining here would miss legal source writes between this
                // recording call and the eventual primary submission.
                primary.secondaries.extend(children);
            }
        }
    }
    f(cmd, count, secondaries);
}

extern "C" fn queue_submit(queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, u32, *const c_void, u64) -> i32>(H_QUEUE_SUBMIT.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if LIVE_CPU_CMDS.load(Relaxed) != 0 && !submits.is_null() {
        let all = (0..count as usize).flat_map(|i| {
            let s = (submits as *const u8).wrapping_add(i * 72) as *const c_void;
            let n: u32 = rd(s, 40);
            let p: *const c_void = rd(s, 48);
            (0..n as usize).map(move |j| rd::<usize>(p, j * 8))
        });
        check_pending(all);
    }
    f(queue, count, submits, fence)
}

extern "C" fn queue_submit2(queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, u32, *const c_void, u64) -> i32>(H_QUEUE_SUBMIT2.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if LIVE_CPU_CMDS.load(Relaxed) != 0 && !submits.is_null() {
        let all = (0..count as usize).flat_map(|i| {
            let s = (submits as *const u8).wrapping_add(i * 64) as *const c_void;
            let n: u32 = rd(s, 32);
            let p: *const c_void = rd(s, 40);
            (0..n as usize).map(move |j| rd::<usize>(p, j * 32 + 16))
        });
        check_pending(all);
    }
    f(queue, count, submits, fence)
}

extern "C" fn create_command_pool(
    device: *mut c_void,
    info: *const c_void,
    alloc: *const c_void,
    out: *mut u64,
) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32>(
        H_CREATE_COMMAND_POOL.load(Relaxed),
    ) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, info, alloc, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() {
        if let Some(dev) = active_device(device as usize) {
            let pool: u64 = rd(out as *const c_void, 0);
            let family: u32 = rd(info, 20);
            let compute = dev
                .gpu
                .as_ref()
                .and_then(|g| g.compute_queue_families.get(family as usize).copied())
                .unwrap_or(false);
            state_write().pool_compute.insert(pool, compute);
        }
    }
    rc
}

extern "C" fn cmd_bind_pipeline(cmd: *mut c_void, bind_point: u32, pipeline: u64) {
    if bind_point == PIPELINE_BIND_POINT_COMPUTE && active().is_some() {
        let mut st = state_write();
        if st.compute_bound.insert(cmd as usize) {
            COMPUTE_BOUND_CMDS.fetch_add(1, Relaxed);
        }
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u32, u64)>(H_CMD_BIND_PIPELINE.load(Relaxed)) {
        f(cmd, bind_point, pipeline);
    }
}

extern "C" fn allocate_command_buffers(device: *mut c_void, info: *const c_void, out: *mut usize) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut usize) -> i32>(H_ALLOCATE_COMMAND_BUFFERS.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, info, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() && active_device(device as usize).is_some() {
        let pool: u64 = rd(info, 16);
        let n: u32 = rd(info, 28);
        let mut st = state_write();
        let mut added = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            let c: usize = rd(out as *const c_void, i * 8);
            st.cmd_pool.insert(c, pool);
            added.push(c);
        }
        st.pool_cmds.entry(pool).or_default().extend(added);
    }
    rc
}

extern "C" fn begin_command_buffer(cmd: *mut c_void, info: *const c_void) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void) -> i32>(H_BEGIN_COMMAND_BUFFER.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    release_cmds(&[cmd as usize]);
    f(cmd, info)
}

extern "C" fn reset_command_buffer(cmd: *mut c_void, flags: u32) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u32) -> i32>(H_RESET_COMMAND_BUFFER.load(Relaxed)) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    release_cmds(&[cmd as usize]);
    f(cmd, flags)
}

extern "C" fn free_command_buffers(device: *mut c_void, pool: u64, count: u32, cmds: *const usize) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, u32, *const usize)>(H_FREE_COMMAND_BUFFERS.load(Relaxed))
    else {
        return;
    };
    if !cmds.is_null() && active_device(device as usize).is_some() {
        let list: Vec<usize> = (0..count as usize).map(|i| rd(cmds as *const c_void, i * 8)).collect();
        release_cmds(&list);
        let mut st = state_write();
        for c in &list {
            st.cmd_pool.remove(c);
        }
        if let Some(v) = st.pool_cmds.get_mut(&pool) {
            v.retain(|c| !list.contains(c));
        }
    }
    f(device, pool, count, cmds);
}

fn cmds_of_pool(pool: u64, forget: bool) -> Vec<usize> {
    let mut st = state_write();
    let list = if forget {
        st.pool_compute.remove(&pool);
        st.pool_cmds.remove(&pool).unwrap_or_else(|| {
            st.cmd_pool.iter().filter(|(_, &p)| p == pool).map(|(&c, _)| c).collect()
        })
    } else if let Some(v) = st.pool_cmds.get(&pool) {
        v.clone()
    } else {
        st.cmd_pool.iter().filter(|(_, &p)| p == pool).map(|(&c, _)| c).collect()
    };
    if forget {
        for c in &list {
            st.cmd_pool.remove(c);
        }
    }
    list
}

extern "C" fn reset_command_pool(device: *mut c_void, pool: u64, flags: u32) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, u32) -> i32>(H_RESET_COMMAND_POOL.load(Relaxed)) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if (LIVE_CMDS.load(Relaxed) != 0 || COMPUTE_BOUND_CMDS.load(Relaxed) != 0)
        && active_device(device as usize).is_some()
    {
        release_cmds(&cmds_of_pool(pool, false));
    }
    f(device, pool, flags)
}

extern "C" fn destroy_command_pool(device: *mut c_void, pool: u64, alloc: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, *const c_void)>(H_DESTROY_COMMAND_POOL.load(Relaxed)) else {
        return;
    };
    if active_device(device as usize).is_some() {
        release_cmds(&cmds_of_pool(pool, true));
    }
    f(device, pool, alloc);
}

#[cfg(test)]
mod tests {
    use super::*;

    static RETIRING: AtomicPtr<DecodeWork> = AtomicPtr::new(std::ptr::null_mut());
    static EARLY_FORWARD: AtomicUsize = AtomicUsize::new(0);

    fn check_retirement() {
        // SAFETY: the lifecycle test retains the Arc until the hook thread exits.
        if let Some(work) = unsafe { RETIRING.load(Acquire).as_ref() } {
            if work.progress.lock().unwrap().remaining != 0 {
                EARLY_FORWARD.fetch_add(1, Relaxed);
            }
        }
    }

    extern "C" fn fake_submit(_: *mut c_void, _: u32, _: *const c_void, _: u64) -> i32 {
        let cmds: Vec<_> = state().cmds.values().cloned().collect();
        if cmds.iter().any(|c| command(c).pending.iter().any(|p| p.work.is_some())) {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        VK_SUCCESS
    }

    static EXECUTE_CALLS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn fake_execute(_: *mut c_void, _: u32, _: *const usize) {
        EXECUTE_CALLS.fetch_add(1, Relaxed);
    }

    extern "C" fn fake_unmap(_: *mut c_void, _: u64) { check_retirement(); }
    extern "C" fn fake_free(_: *mut c_void, _: u64, _: *const c_void) { check_retirement(); }
    extern "C" fn fake_reset(_: *mut c_void, _: u32) -> i32 { check_retirement(); VK_SUCCESS }
    extern "C" fn fake_free_cmds(_: *mut c_void, _: u64, _: u32, _: *const usize) { check_retirement(); }
    extern "C" fn fake_reset_pool(_: *mut c_void, _: u64, _: u32) -> i32 { check_retirement(); VK_SUCCESS }
    extern "C" fn fake_destroy_device(_: *mut c_void, _: *const c_void) { check_retirement(); }

    struct CopyHarness {
        dev: Box<Dev>,
        src: Vec<u8>,
        dst: Vec<u8>,
        width: usize,
    }

    impl CopyHarness {
        fn new(width: usize) -> Self {
            Self::with_capacity(width, 1)
        }

        fn with_capacity(width: usize, copies: usize) -> Self {
            extern "C" fn create(_: usize, _: *const c_void, _: *const c_void, _: *mut u64) -> i32 { -1 }
            extern "C" fn destroy(_: usize, _: u64, _: *const c_void) { check_retirement(); }
            extern "C" fn requirements(_: usize, _: u64, _: *mut [u64; 3]) {}
            extern "C" fn bind(_: usize, _: u64, _: u64, _: u64) -> i32 { 0 }
            extern "C" fn map(_: usize, _: u64, _: u64, _: u64, _: u32, _: *mut *mut c_void) -> i32 { -1 }
            extern "C" fn copy(_: *mut c_void, _: u64, _: u64, _: i32, _: u32, _: *const c_void) {}
            let dev = Box::new(Dev {
                device: 0x8001, create_buffer: create, destroy_buffer: destroy,
                buffer_requirements: requirements, allocate_memory: create, free_memory: destroy,
                bind_buffer_memory: bind, map_memory: map, copy_buffer_to_image: copy,
                memory_types: vec![MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT],
                free_cpu_chunks: Mutex::new(Vec::new()),
                gpu: None,
            });
            let mut seed = 0x1234_5678u32;
            let src = (0..width * width).map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            }).collect::<Vec<_>>();
            let mut dst = vec![0xa5; width * width * 4 * copies];
            {
                let mut st = state();
                assert!(st.cmds.is_empty());
                st.buffers.insert(1, (2, 0));
                st.cmd_pool.insert(3, 7);
                st.cmds.insert(3, Arc::new(Mutex::new(CmdState {
                    chunks: vec![Chunk { buffer: 4, memory: 5, mapped: dst.as_mut_ptr() as usize, size: dst.len() as u64, used: 0 }],
                    ..CmdState::default()
                })));
            }
            mapped_write().insert(2, (src.as_ptr() as usize, 0, src.len() as u64));
            LIVE_CMDS.store(1, Relaxed);
            LIVE_CPU_CMDS.store(0, Relaxed);
            DEV.store(&*dev as *const Dev as *mut Dev, Release);
            Self { dev, src, dst, width }
        }

        fn record(&self) -> Result<String, String> {
            self.record_on(3)
        }

        fn record_on(&self, cmd: usize) -> Result<String, String> {
            let w = self.width as u32;
            let region = BufferImageCopy {
                buffer_offset: 0, buffer_row_length: 0, buffer_image_height: 0,
                aspect_mask: 1, mip_level: 0, base_array_layer: 0, layer_count: 1,
                image_offset: [0; 3], image_extent: [w, w, 1],
            };
            let image = EmuImage { format: EtcFormat::Rgba8, etc: 151, substitute: 37, width: w, height: w };
            emulate_copy(&self.dev, cmd as *mut c_void, 1, 6, image, 0, &region)
        }

        fn split_commands(&self, count: usize) {
            let mut st = state();
            st.cmds.clear();
            let size = self.dst.len() / count;
            for i in 0..count {
                st.cmds.insert(3 + i, Arc::new(Mutex::new(CmdState {
                    chunks: vec![Chunk { buffer: 4 + i as u64, memory: 100 + i as u64,
                        mapped: self.dst.as_ptr() as usize + i * size, size: size as u64, used: 0 }],
                    ..CmdState::default()
                })));
                st.cmd_pool.insert(3 + i, 7);
            }
            LIVE_CMDS.store(count, Relaxed);
        }

        fn rewind(&self) {
            let cmds: Vec<_> = state().cmds.values().cloned().collect();
            for shared in cmds {
                let mut cmd = command(&shared);
                for mut p in cmd.pending.drain(..) {
                    p.finish().unwrap();
                }
                cmd.chunks[0].used = 0;
            }
        }

        fn expected(&self) -> Vec<u8> {
            let mut dst = vec![0; self.dst.len()];
            let job = job_for(self.width, self.width, 1, &mut dst);
            run_job(&job, &self.src).unwrap();
            dst
        }

        fn submit(&self, second: bool) -> i32 {
            let mut submit = [0u64; 9];
            if second {
                let info = [0usize, 0, 3, 0];
                submit[4] = 1;
                submit[5] = info.as_ptr() as u64;
                H_QUEUE_SUBMIT2.store(fake_submit as *const () as usize, Relaxed);
                queue_submit2(std::ptr::null_mut(), 1, submit.as_ptr() as *const c_void, 0)
            } else {
                let cmds = [3usize];
                submit[5] = 1;
                submit[6] = cmds.as_ptr() as u64;
                H_QUEUE_SUBMIT.store(fake_submit as *const () as usize, Relaxed);
                queue_submit(std::ptr::null_mut(), 1, submit.as_ptr() as *const c_void, 0)
            }
        }
    }

    impl Drop for CopyHarness {
        fn drop(&mut self) {
            forget_device();
        }
    }

    #[test]
    fn secondary_uploads_are_joined_and_rechecked_when_the_primary_is_submitted() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for name in [&b"vkCmdExecuteCommands"[..], &b"vkCmdExecuteCommandsKHR"[..]] {
            for second in [false, true] {
                for mode in 0..3 {
                    let mut harness = CopyHarness::new(256);
                    {
                        let mut st = state();
                        let secondary = st.cmds.remove(&3).unwrap();
                        st.cmds.insert(4, secondary);
                        st.cmd_pool.insert(4, 7);
                    }
                    harness.record_on(4).unwrap();
                    {
                        let shared = state().cmds[&4].clone();
                        let mut cmd = command(&shared);
                        let pending = &mut cmd.pending[0];
                        pending.finish().unwrap();
                        pending.decoded = false;
                        pending.hash = 0;
                        let source = if mode == 2 { Vec::new() } else { harness.src.clone() };
                        // Leave the replacement unpublished so only the submit
                        // walker can make progress on this secondary's work.
                        pending.work = Some(DecodePool::get().prepare(&pending.job, DecodeSource::Snapshot(source.into_boxed_slice())));
                    }
                    let hooked = hook(name, fake_execute as *const () as *mut c_void, std::ptr::null_mut()).unwrap();
                    let execute: extern "C" fn(*mut c_void, u32, *const usize) = as_fn(hooked as usize).unwrap();
                    let calls = EXECUTE_CALLS.load(Relaxed);
                    execute(5usize as *mut c_void, 1, [4usize].as_ptr());
                    execute(3usize as *mut c_void, 2, [5usize, 5].as_ptr());
                    assert_eq!(EXECUTE_CALLS.load(Relaxed), calls + 2);
                    assert!(command(&state().cmds[&4]).pending[0].work.is_some());
                    if mode == 1 {
                        harness.src[0] ^= 0x80;
                    }
                    let errors = glcount::ETC_UNHANDLED.load(Relaxed);
                    let late = glcount::ETC_LATE_WRITE.load(Relaxed);
                    assert_eq!(harness.submit(second), VK_SUCCESS);
                    assert!(command(&state().cmds[&4]).pending[0].work.is_none());
                    if mode == 2 {
                        assert_eq!(glcount::ETC_UNHANDLED.load(Relaxed), errors + 1);
                        assert!(harness.dst.iter().all(|&b| b == 0));
                    } else {
                        assert_eq!(glcount::ETC_UNHANDLED.load(Relaxed), errors);
                        assert_eq!(glcount::ETC_LATE_WRITE.load(Relaxed), late + u64::from(mode == 1));
                        assert_eq!(harness.dst, harness.expected());
                    }
                }
            }
        }
    }

    #[test]
    fn empty_execution_and_primary_reset_do_not_retain_secondary_links() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let harness = CopyHarness::new(256);
        {
            let mut st = state();
            let secondary = st.cmds.remove(&3).unwrap();
            st.cmds.insert(4, secondary);
        }
        harness.record_on(4).unwrap();
        H_CMD_EXECUTE_COMMANDS.store(fake_execute as *const () as usize, Relaxed);
        cmd_execute_commands(3usize as *mut c_void, 0, std::ptr::null());
        cmd_execute_commands(3usize as *mut c_void, 1, [99usize].as_ptr());
        assert!(!state().cmds.contains_key(&3));
        cmd_execute_commands(3usize as *mut c_void, 1, [4usize].as_ptr());
        assert_eq!(command(&state().cmds[&3]).secondaries, [4]);
        H_RESET_COMMAND_BUFFER.store(fake_reset as *const () as usize, Relaxed);
        assert_eq!(reset_command_buffer(3usize as *mut c_void, 0), VK_SUCCESS);
        check_pending(std::iter::once(3));
        assert!(command(&state().cmds[&4]).pending[0].work.is_some());
    }

    #[test]
    fn recorded_jobs_finish_before_both_submit_variants_and_recheck_late_writes() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for second in [false, true] {
            let mut harness = CopyHarness::new(256);
            harness.record().unwrap();
            assert!(command(&state().cmds[&3]).pending[0].work.is_some());
            assert_eq!(harness.submit(second), VK_SUCCESS);
            assert_eq!(harness.dst, harness.expected());
            harness.rewind();
            harness.record().unwrap();
            let late = glcount::ETC_LATE_WRITE.load(Relaxed);
            harness.src[0] ^= 0x80;
            assert_eq!(harness.submit(second), VK_SUCCESS);
            assert_eq!(glcount::ETC_LATE_WRITE.load(Relaxed), late + 1);
            assert_eq!(harness.dst, harness.expected());
        }
    }

    #[test]
    fn async_failures_are_reported_at_submit_and_never_upload_uninitialised_texels() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let harness = CopyHarness::new(256);
        harness.record().unwrap();
        {
            let shared = state().cmds[&3].clone();
            let mut cmd = command(&shared);
            let pending = &mut cmd.pending[0];
            pending.finish().unwrap();
            pending.decoded = false;
            pending.work = Some(DecodePool::get().prepare(&pending.job, DecodeSource::Snapshot(Box::new([]))));
        }
        let errors = glcount::ETC_UNHANDLED.load(Relaxed);
        assert_eq!(harness.submit(false), VK_SUCCESS);
        assert_eq!(glcount::ETC_UNHANDLED.load(Relaxed), errors + 1);
        assert!(harness.dst.iter().all(|&b| b == 0));
        assert!(command(&state().cmds[&3]).pending[0].error.is_some());
        assert_eq!(harness.submit(true), VK_SUCCESS);
        assert_eq!(glcount::ETC_UNHANDLED.load(Relaxed), errors + 2);
    }

    #[test]
    fn memory_and_command_retirement_join_inflight_jobs_before_forwarding() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        H_UNMAP_MEMORY.store(fake_unmap as *const () as usize, Relaxed);
        H_FREE_MEMORY.store(fake_free as *const () as usize, Relaxed);
        H_RESET_COMMAND_BUFFER.store(fake_reset as *const () as usize, Relaxed);
        H_FREE_COMMAND_BUFFERS.store(fake_free_cmds as *const () as usize, Relaxed);
        H_RESET_COMMAND_POOL.store(fake_reset_pool as *const () as usize, Relaxed);
        H_DESTROY_COMMAND_POOL.store(fake_free as *const () as usize, Relaxed);
        H_DESTROY_DEVICE.store(fake_destroy_device as *const () as usize, Relaxed);
        for operation in 0..7 {
            let harness = CopyHarness::new(256);
            harness.record().unwrap();
            let work = {
                let shared = state().cmds[&3].clone();
                let mut cmd = command(&shared);
                let p = &mut cmd.pending[0];
                p.finish().unwrap();
                let work = DecodePool::get().prepare(&p.job, DecodeSource::Snapshot(harness.src.clone().into_boxed_slice()));
                p.work = Some(Arc::clone(&work));
                p.decoded = false;
                work
            };
            // Hold one claimed band outside the queue. The retiring caller must
            // help the others, but cannot finish until this band is released.
            let band = work.claim().unwrap();
            RETIRING.store(Arc::as_ptr(&work) as *mut DecodeWork, Release);
            EARLY_FORWARD.store(0, Relaxed);
            let (started, start) = std::sync::mpsc::channel();
            let (done, finished) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                started.send(()).unwrap();
                let device = 0x8001usize as *mut c_void;
                match operation {
                    0 => unmap_memory(device, 2),
                    1 => free_memory(device, 2, std::ptr::null()),
                    2 => { reset_command_buffer(3usize as *mut c_void, 0); }
                    3 => free_command_buffers(device, 7, 1, [3usize].as_ptr()),
                    4 => { reset_command_pool(device, 7, 0); }
                    5 => destroy_command_pool(device, 7, std::ptr::null()),
                    _ => destroy_device(device, std::ptr::null()),
                }
                done.send(()).unwrap();
            });
            start.recv().unwrap();
            let early = finished.recv_timeout(std::time::Duration::from_millis(20));
            let (probe, state_ready) = std::sync::mpsc::channel();
            let metadata = std::thread::spawn(move || {
                drop(state());
                probe.send(()).unwrap();
            });
            let unlocked = state_ready.recv_timeout(std::time::Duration::from_millis(100));
            work.perform(band);
            thread.join().unwrap();
            metadata.join().unwrap();
            RETIRING.store(std::ptr::null_mut(), Release);
            assert!(matches!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout)), "operation {operation}");
            finished.recv().unwrap();
            assert_eq!(EARLY_FORWARD.load(Relaxed), 0, "operation {operation}");
            assert!(unlocked.is_ok(), "operation {operation} held state() while waiting");
            assert_eq!(work.progress.lock().unwrap().remaining, 0);
            assert_eq!(harness.dst, harness.expected());
            if operation < 2 {
                assert!(!mapped_read().contains_key(&2));
                assert_eq!(harness.submit(false), VK_SUCCESS);
            } else {
                assert!(!state().cmds.contains_key(&3));
            }
        }
    }

    #[test]
    fn small_empty_and_unreadable_recordings_keep_their_fallbacks() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut harness = CopyHarness::new(64);
        harness.record().unwrap();
        assert!(command(&state().cmds[&3]).pending[0].work.is_some());
        harness.submit(false);
        assert_eq!(harness.dst, harness.expected());
        harness.rewind();
        let invalid = BufferImageCopy {
            buffer_offset: 0, buffer_row_length: 4, buffer_image_height: 0,
            aspect_mask: 1, mip_level: 0, base_array_layer: 0, layer_count: 1,
            image_offset: [0; 3], image_extent: [64, 64, 1],
        };
        let image = EmuImage { format: EtcFormat::Rgba8, etc: 151, substitute: 37, width: 64, height: 64 };
        assert!(emulate_copy(&harness.dev, 3usize as *mut c_void, 1, 6, image, 0, &invalid).is_err());
        assert!(command(&state().cmds[&3]).pending.is_empty());
        harness.rewind();
        mapped_write().remove(&2);
        harness.record().unwrap();
        assert_eq!(harness.submit(false), VK_SUCCESS);
        assert!(harness.dst.iter().all(|&b| b == 0));
        harness.rewind();
        harness.width = 0;
        assert!(harness.record().is_ok());
        assert_eq!(harness.submit(false), VK_SUCCESS);
    }

    #[test]
    #[ignore]
    fn microbench_bursts() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for (width, jobs) in [(128, 64), (512, 16)] {
            for commands in [1, 8] {
                let harness = CopyHarness::with_capacity(width, jobs);
                harness.split_commands(commands);
                let expected = harness.expected();
                for sync in [true, false] {
                    let mut samples = Vec::new();
                    for _ in 0..6 {
                        SYNC_RECORD.with(|mode| mode.set(sync));
                        let start = std::time::Instant::now();
                        for i in 0..jobs {
                            std::hint::black_box(harness.record_on(3 + i % commands).unwrap());
                        }
                        let hook = start.elapsed().as_secs_f64();
                        SYNC_RECORD.with(|mode| mode.set(false));
                        check_pending(3..3 + commands);
                        samples.push((hook, start.elapsed().as_secs_f64()));
                        for image in harness.dst.chunks_exact(width * width * 4) {
                            assert_eq!(image, &expected[..width * width * 4]);
                        }
                        harness.rewind();
                    }
                    samples.remove(0);
                    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
                    println!("burst {jobs}x{width}x{width} cmds={commands} sync={sync}: hook {:.3} ms; all-done {:.3} ms", samples[2].0 * 1000.0, samples[2].1 * 1000.0);
                }
            }
        }
    }

    #[test]
    fn multiple_recording_threads_keep_command_staging_independent() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let harness = CopyHarness::with_capacity(128, 32);
        harness.split_commands(8);
        std::thread::scope(|scope| {
            for cmd in 3..11 {
                let harness = &harness;
                scope.spawn(move || {
                    for _ in 0..4 {
                        harness.record_on(cmd).unwrap();
                    }
                });
            }
        });
        check_pending(3..11);
        let expected = harness.expected();
        for image in harness.dst.chunks_exact(128 * 128 * 4) {
            assert_eq!(image, &expected[..128 * 128 * 4]);
        }
    }

    #[test]
    fn queue_claims_bands_from_different_jobs_before_the_first_finishes() {
        let pool = DecodePool { queue: Arc::new(DecodeQueue::default()), helpers: 8 };
        let mut a = vec![0; 128 * 128 * 4];
        let mut b = a.clone();
        let wa = pool.prepare(&job_for(128, 128, 1, &mut a), DecodeSource::Snapshot(vec![0; 128 * 128].into_boxed_slice()));
        let wb = pool.prepare(&job_for(128, 128, 1, &mut b), DecodeSource::Snapshot(vec![0; 128 * 128].into_boxed_slice()));
        let mut ready = VecDeque::from([wa.clone(), wb.clone()]);
        let (first, a_band) = DecodeQueue::take(&mut ready).unwrap();
        let (second, b_band) = DecodeQueue::take(&mut ready).unwrap();
        assert!(Arc::ptr_eq(&first, &wa));
        assert!(Arc::ptr_eq(&second, &wb));
        second.perform(b_band);
        first.perform(a_band);
        pool.wait(&wa).unwrap();
        pool.wait(&wb).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    #[ignore]
    fn microbench_record_submit() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for width in [64, 128, 256, 1024] {
            let harness = CopyHarness::new(width);
            harness.record().unwrap();
            check_pending(std::iter::once(3));
            harness.rewind();
            let mut samples = Vec::new();
            for _ in 0..5 {
                let (mut record, mut total) = (0.0, 0.0);
                for _ in 0..30 {
                    let start = std::time::Instant::now();
                    std::hint::black_box(harness.record().unwrap());
                    record += start.elapsed().as_secs_f64();
                    check_pending(std::iter::once(3));
                    total += start.elapsed().as_secs_f64();
                    harness.rewind();
                }
                samples.push((record / 30.0, total / 30.0));
            }
            samples.sort_by(|a, b| a.0.total_cmp(&b.0));
            println!("record {width}x{width} RGBA8: {:.3} ms; record+submit: {:.3} ms", samples[2].0 * 1000.0, samples[2].1 * 1000.0);
            assert_eq!(harness.dst, harness.expected());
            if width <= 128 {
                let mut dst = vec![0; harness.dst.len()];
                let job = job_for(width, width, 1, &mut dst);
                for queued in [false, true] {
                    let mut samples = Vec::new();
                    for _ in 0..5 {
                        let start = std::time::Instant::now();
                        for _ in 0..30 {
                            if queued {
                                let snapshot = harness.src.clone().into_boxed_slice();
                                let work = DecodePool::get().enqueue(&job, snapshot);
                                DecodePool::get().wait(&work).unwrap();
                            } else {
                                std::hint::black_box(hash(&harness.src));
                                run_job(&job, &harness.src).unwrap();
                            }
                            std::hint::black_box(&dst);
                        }
                        samples.push(start.elapsed().as_secs_f64() / 30.0);
                    }
                    samples.sort_by(f64::total_cmp);
                    println!("{width}x{width} CPU control queued={queued}: {:.3} ms", samples[2] * 1000.0);
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn microbench_upload() {
        use std::hint::black_box;
        use std::time::Instant;

        for len in [4096usize, 4 * 1024 * 1024 + 31] {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let iterations = (128 * 1024 * 1024 / len).max(1);
            let mut samples = Vec::new();
            for _ in 0..5 {
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(hash(black_box(&data)));
                }
                samples.push(start.elapsed().as_secs_f64());
            }
            samples.sort_by(f64::total_cmp);
            println!("hash {len} B: {:.3} GB/s", len as f64 * iterations as f64 / samples[2] / 1e9);
        }
        for width in [128usize, 256, 1024] {
            let height = width;
            let mut seed = 0x1234_5678u32;
            let src: Vec<u8> = (0..width * height).map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            }).collect();
            let mut dst = vec![0; width * height * 4];
            let job = job_for(width, height, 1, &mut dst);
            run_job(&job, &src).unwrap();
            let mut samples = Vec::new();
            for _ in 0..5 {
                let start = Instant::now();
                for _ in 0..30 {
                    run_job(black_box(&job), black_box(&src)).unwrap();
                    black_box(&dst);
                }
                samples.push(start.elapsed().as_secs_f64() / 30.0);
            }
            samples.sort_by(f64::total_cmp);
            println!("upload {width}x{height} RGBA8: {:.3} ms/image", samples[2] * 1000.0);
        }
    }

    #[test]
    fn substitutes_keep_srgb_and_signedness() {
        let table: Vec<(u32, u32)> = (147..=156).map(|f| (f, etc_format(f).unwrap().1)).collect();
        assert_eq!(
            table,
            vec![(147, 37), (148, 43), (149, 37), (150, 43), (151, 37), (152, 43), (153, 70), (154, 71), (155, 77), (156, 78)]
        );
        assert!(etc_format(146).is_none());
        assert!(etc_format(157).is_none());
    }

    #[test]
    fn only_sampling_and_transfers_are_emulated() {
        assert!(image_request_ok(IMAGE_TILING_OPTIMAL, 0x7, 0));
        assert!(image_request_ok(IMAGE_TILING_OPTIMAL, 0x4, IMAGE_CREATE_CUBE_COMPATIBLE));
        assert!(!image_request_ok(1, 0x4, 0));
        assert!(!image_request_ok(IMAGE_TILING_OPTIMAL, 0x10, 0));
        assert!(!image_request_ok(IMAGE_TILING_OPTIMAL, 0x8, 0));
        assert!(!image_request_ok(IMAGE_TILING_OPTIMAL, 0x4, 0x80));
    }

    fn job_for(width: usize, height: usize, layers: usize, dst: &mut [u8]) -> Job {
        let row_blocks = width.div_ceil(4);
        let rows_of_blocks = height.div_ceil(4);
        Job {
            format: EtcFormat::Rgba8,
            row_blocks,
            rows_of_blocks,
            layer_stride: row_blocks * rows_of_blocks * 16,
            layers,
            width,
            height,
            dst: dst.as_mut_ptr() as usize,
            dst_len: dst.len(),
        }
    }

    #[test]
    fn a_large_region_decoded_in_bands_matches_one_pass() {
        let (w, h, layers): (usize, usize, usize) = (514, 262, 2);
        let blocks = w.div_ceil(4) * h.div_ceil(4) * layers;
        let mut seed = 0x1234_5678_u32;
        let src: Vec<u8> = (0..blocks * 16)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        let mut banded = vec![0u8; w * h * 4 * layers];
        let job = job_for(w, h, layers, &mut banded);
        run_job(&job, &src).unwrap();
        let mut single = vec![0u8; w * h * 4 * layers];
        for layer in 0..layers {
            let surface = Surface {
                data: &src[layer * job.layer_stride..],
                row_blocks: job.row_blocks,
                rows_of_blocks: job.rows_of_blocks,
            };
            etc_decode::decode_region(
                EtcFormat::Rgba8,
                &surface,
                w,
                h,
                &mut single[layer * w * h * 4..(layer + 1) * w * h * 4],
            )
            .unwrap();
        }
        assert!(banded == single);
    }

    fn reference_hash(data: &[u8]) -> u64 {
        let mut lanes = [
            0xcbf2_9ce4_8422_2325 ^ data.len() as u64,
            0x9e37_79b9_7f4a_7c15,
            0xd6e8_feb8_6659_fd93,
            0xa076_1d64_78bd_642f,
        ];
        let whole = data.len() / 32 * 32;
        let mut i = 0;
        while i < whole {
            for lane in 0..4 {
                let mut w = 0u64;
                for k in 0..8 {
                    w |= u64::from(data[i + lane * 8 + k]) << (8 * k);
                }
                lanes[lane] = (lanes[lane] ^ w).wrapping_mul(0x0100_0000_01b3).rotate_left(29);
            }
            i += 32;
        }
        let mut h = lanes[0].rotate_left(51) ^ lanes[1].rotate_left(34) ^ lanes[2].rotate_left(17) ^ lanes[3];
        for &b in &data[whole..] {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        h
    }

    #[test]
    fn hash_matches_the_reference_across_every_remainder_length() {
        let data: Vec<u8> = (0..70u32).map(|i| (i * 37 + 11) as u8).collect();
        for len in 0..data.len() {
            assert_eq!(hash(&data[..len]), reference_hash(&data[..len]), "length {len}");
        }
    }

    #[test]
    fn hashing_notices_each_lane_byte_and_tail_bit() {
        for len in [1, 7, 8, 31, 32, 33, 63, 64, 65, 127] {
            let mut data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let original = hash(&data);
            for i in 0..len {
                for bit in 0..8 {
                    data[i] ^= 1 << bit;
                    assert_ne!(hash(&data), original, "length {len}, byte {i}, bit {bit}");
                    data[i] ^= 1 << bit;
                }
            }
        }
    }

    #[test]
    fn concurrent_jobs_match_serial_decoding() {
        let barrier = Arc::new(std::sync::Barrier::new(12));
        let threads: Vec<_> = (0..12).map(|i| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let (w, h, layers) = (257usize, 259usize, 2usize);
                let src: Vec<u8> = (0..w.div_ceil(4) * h.div_ceil(4) * 16 * layers)
                    .map(|j| (j * 37 + i * 17) as u8).collect();
                let mut dst = vec![0; w * h * 4 * layers];
                let job = job_for(w, h, layers, &mut dst);
                for _ in 0..3 {
                    barrier.wait();
                    run_job(&job, &src).unwrap();
                }
                let mut expected = vec![0; w * h * 4];
                for (i, layer) in dst.chunks(expected.len()).enumerate() {
                    let surface = Surface { data: &src[i * job.layer_stride..], row_blocks: job.row_blocks, rows_of_blocks: job.rows_of_blocks };
                    etc_decode::decode_region(job.format, &surface, w, h, &mut expected).unwrap();
                    assert_eq!(layer, expected);
                }
            })
        }).collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn pool_errors_finish_all_bands_and_leave_it_usable() {
        let mut dst = vec![0; 256 * 256 * 4];
        let mut job = job_for(256, 256, 1, &mut dst);
        assert!(run_job(&job, &[0; 16]).is_err());
        job.dst_len -= 1;
        assert!(run_job(&job, &vec![0; 256 * 256]).is_err());
        job.dst_len += 1;
        run_job(&job, &vec![0; 256 * 256]).unwrap();
        job.height = 0;
        run_job(&job, &[]).unwrap();
        job.height = 256;
        job.layers = 0;
        run_job(&job, &[]).unwrap();
    }

    #[test]
    fn caller_can_finish_without_any_helpers() {
        let pool = DecodePool { queue: Arc::new(DecodeQueue::default()), helpers: 8 };
        let mut dst = vec![0; 256 * 256 * 4];
        let job = job_for(256, 256, 1, &mut dst);
        pool.run(&job, &vec![0; 256 * 256]).unwrap();
        assert!(pool.queue.ready.lock().unwrap().is_empty());
    }

    #[test]
    fn emulation_is_decided_by_the_missing_feature_or_the_test_switch() {
        assert!(should_emulate(false, false));
        assert!(!should_emulate(true, false));
        assert!(should_emulate(true, true));
        assert!(should_emulate(false, true));
    }

    fn word(buf: &mut [u64], byte: usize, v: u32) {
        let at = byte / 8;
        let shift = (byte % 8) * 8;
        buf[at] = (buf[at] & !(0xffff_ffffu64 << shift)) | (u64::from(v) << shift);
    }

    fn word_at(buf: &[u64], byte: usize) -> u32 {
        (buf[byte / 8] >> ((byte % 8) * 8)) as u32
    }

    #[test]
    fn a_device_image_query_for_an_etc_image_is_sized_as_the_substitute() {
        let mut create = [0u64; 11];
        word(&mut create, 0, 14);
        word(&mut create, 24, 147);
        let mut info = [0u64; 4];
        info[2] = create.as_ptr() as u64;

        let sub = substitute_image_info(info.as_ptr() as *const c_void).expect("an ETC2 format is substituted");
        assert_eq!(rd::<u32>(sub.create.as_ptr() as *const c_void, 24), 37);
        assert_eq!(rd::<usize>(sub.info.as_ptr() as *const c_void, 16), sub.create.as_ptr() as usize);
        assert_eq!(word_at(&create, 24), 147, "the engine's own create info is not modified");

        word(&mut create, 24, 37);
        assert!(substitute_image_info(info.as_ptr() as *const c_void).is_none());
        info[2] = 0;
        assert!(substitute_image_info(info.as_ptr() as *const c_void).is_none());
        assert!(substitute_image_info(std::ptr::null()).is_none());
    }

    /// A `VkDeviceCreateInfo` whose chain is `[other node] -> Features2`, with
    /// the ETC2 feature enabled in the Features2.
    fn chain_with_features2_second() -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        let mut other = vec![0u64; 2];
        word(&mut other, 0, 1_000_000_001);
        let mut f2 = vec![0u64; 30];
        word(&mut f2, 0, ST_PHYSICAL_DEVICE_FEATURES_2);
        word(&mut f2, 16 + FEATURE_ETC2 * 4, 1);
        other[1] = f2.as_ptr() as u64;
        let mut info = vec![0u64; 9];
        word(&mut info, 0, 3);
        info[1] = other.as_ptr() as u64;
        (info, other, f2)
    }

    #[test]
    fn features2_behind_another_node_is_cleared_for_the_call_and_put_back() {
        let (info, _other, f2) = chain_with_features2_second();
        let at = 16 + FEATURE_ETC2 * 4;
        let stripped = strip_with(true, info.as_ptr() as *const c_void).expect("an ETC2 feature in the chain is stripped");
        assert_eq!(word_at(&f2, at), 0, "the driver must not see the feature enabled");
        assert_eq!(stripped.as_ptr(info.as_ptr() as *const c_void), info.as_ptr() as *const c_void);
        drop(stripped);
        assert_eq!(word_at(&f2, at), 1, "the engine's structure is restored");
    }

    #[test]
    fn features2_first_in_the_chain_is_copied_and_the_original_left_alone() {
        let mut f2 = vec![0u64; 30];
        word(&mut f2, 0, ST_PHYSICAL_DEVICE_FEATURES_2);
        word(&mut f2, 16 + FEATURE_ETC2 * 4, 1);
        let mut info = [0u64; 9];
        info[1] = f2.as_ptr() as u64;
        let original = info.as_ptr() as *const c_void;
        let stripped = strip_with(true, original).expect("stripped");
        assert_eq!(word_at(&f2, 16 + FEATURE_ETC2 * 4), 1, "no write to the caller's structure");
        let given = stripped.as_ptr(original);
        assert_ne!(given, original);
        let copied_head: *const c_void = rd(given, 8);
        assert_eq!(rd::<u32>(copied_head, 16 + FEATURE_ETC2 * 4), 0);
        assert!(strip_with(false, original).is_none(), "nothing is stripped when not emulating");
    }

    // The hooks keep their host function in process-wide statics, so the tests
    // that install one take turns.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static FAKE_CREATE_CALLS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn fake_create_image(_: *mut c_void, _: *const c_void, _: *const c_void, _: *mut u64) -> i32 {
        FAKE_CREATE_CALLS.fetch_add(1, Relaxed);
        VK_SUCCESS
    }

    #[test]
    fn an_etc_image_on_an_emulating_device_without_state_is_refused_not_forwarded() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        H_CREATE_IMAGE.store(fake_create_image as *const () as usize, Relaxed);
        let mut create = [0u64; 11];
        word(&mut create, 24, 147);
        word(&mut create, 40, 1);
        let info = create.as_ptr() as *const c_void;
        let (emulating_device, native_device) = (0x6001usize as *mut c_void, 0x6002usize as *mut c_void);
        emulating().push(emulating_device as usize);
        let unhandled_before = glcount::ETC_UNHANDLED.load(Relaxed);
        let calls_before = FAKE_CREATE_CALLS.load(Relaxed);

        let mut out = 0u64;
        let rc = create_image(emulating_device, info, std::ptr::null(), &mut out);
        assert_eq!(rc, VK_ERROR_FORMAT_NOT_SUPPORTED);
        assert_eq!(FAKE_CREATE_CALLS.load(Relaxed), calls_before, "the driver was never asked");
        assert_eq!(glcount::ETC_UNHANDLED.load(Relaxed), unhandled_before + 1);

        let rc = create_image(native_device, info, std::ptr::null(), &mut out);
        assert_eq!(rc, VK_SUCCESS);
        assert_eq!(FAKE_CREATE_CALLS.load(Relaxed), calls_before + 1, "a native device is passed straight through");
        emulating().retain(|&d| d != emulating_device as usize);
    }

    #[test]
    fn device_level_hooks_are_installed_only_on_an_emulating_device() {
        let host = 0x10usize as *mut c_void;
        let (emulating_device, native_device) = (0x7001usize as *mut c_void, 0x7002usize as *mut c_void);
        emulating().push(emulating_device as usize);
        for name in [
            &b"vkGetDeviceImageMemoryRequirements"[..],
            b"vkGetDeviceImageMemoryRequirementsKHR",
            b"vkGetDeviceImageSparseMemoryRequirements",
            b"vkResetCommandPool",
        ] {
            assert!(hook(name, host, native_device).is_none(), "{}", String::from_utf8_lossy(name));
            assert!(hook(name, host, emulating_device).is_some(), "{}", String::from_utf8_lossy(name));
            assert!(hook(name, host, std::ptr::null_mut()).is_some(), "instance-level lookups stay hooked");
        }
        assert!(hook(b"vkNotAnEtcCall", host, emulating_device).is_none());
        emulating().retain(|&d| d != emulating_device as usize);
    }

    #[test]
    fn hashing_notices_a_single_changed_byte() {
        let mut data = vec![7u8; 4099];
        let before = hash(&data);
        data[4098] = 8;
        assert_ne!(before, hash(&data));
        data[4098] = 7;
        data[0] = 6;
        assert_ne!(before, hash(&data));
    }

    #[test]
    fn staging_chunks_are_recycled_across_command_buffer_resets() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static DESTROY_CALLS: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn create(_: usize, _: *const c_void, _: *const c_void, _: *mut u64) -> i32 { -1 }
        extern "C" fn destroy(_: usize, _: u64, _: *const c_void) { DESTROY_CALLS.fetch_add(1, Relaxed); }
        extern "C" fn requirements(_: usize, _: u64, _: *mut [u64; 3]) {}
        extern "C" fn bind(_: usize, _: u64, _: u64, _: u64) -> i32 { 0 }
        extern "C" fn map(_: usize, _: u64, _: u64, _: u64, _: u32, _: *mut *mut c_void) -> i32 { -1 }
        extern "C" fn copy(_: *mut c_void, _: u64, _: u64, _: i32, _: u32, _: *const c_void) {}

        let dev = Box::new(Dev {
            device: 0x8009,
            create_buffer: create,
            destroy_buffer: destroy,
            buffer_requirements: requirements,
            allocate_memory: create,
            free_memory: destroy,
            bind_buffer_memory: bind,
            map_memory: map,
            copy_buffer_to_image: copy,
            memory_types: vec![MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT | MEMORY_HOST_CACHED],
            free_cpu_chunks: Mutex::new(Vec::new()),
            gpu: None,
        });
        let mut backing = vec![0u8; 4096];
        let ptr = backing.as_mut_ptr() as usize;
        state().cmds.insert(10, Arc::new(Mutex::new(CmdState {
            chunks: vec![Chunk { buffer: 42, memory: 43, mapped: ptr, size: 4096, used: 1024 }],
            ..CmdState::default()
        })));
        LIVE_CMDS.store(1, Relaxed);
        DEV.store(&*dev as *const Dev as *mut Dev, Release);

        let before = DESTROY_CALLS.load(Relaxed);
        release_cmds(&[10]);
        assert_eq!(DESTROY_CALLS.load(Relaxed), before, "standard chunk must be pooled rather than destroyed");
        assert_eq!(dev.free_cpu_chunks.lock().unwrap().len(), 1);

        let mut next = CmdState::default();
        let (buf, off, mapped) = staging(&dev, &mut next, 512).unwrap();
        assert_eq!((buf, off, mapped), (42, 0, ptr));
        assert_eq!(dev.free_cpu_chunks.lock().unwrap().len(), 0);

        // Return to pool, then verify forget_device destroys pooled chunks cleanly.
        state().cmds.insert(11, Arc::new(Mutex::new(next)));
        LIVE_CMDS.store(1, Relaxed);
        release_cmds(&[11]);
        assert_eq!(dev.free_cpu_chunks.lock().unwrap().len(), 1);
        forget_device();
        assert_eq!(DESTROY_CALLS.load(Relaxed), before + 2, "forget_device destroys buffer + memory");
    }

    struct HeadlessVk {
        instance: *mut c_void,
        device: *mut c_void,
        queue: *mut c_void,
        cmd_pool: u64,
        cmd: *mut c_void,
        gpu: GpuDecoder,
        mem_types: Vec<u32>,
        destroy_instance: extern "C" fn(*mut c_void, *const c_void),
        destroy_device: extern "C" fn(*mut c_void, *const c_void),
        create_buffer: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32,
        destroy_buffer: extern "C" fn(usize, u64, *const c_void),
        buffer_reqs: extern "C" fn(usize, u64, *mut [u64; 3]),
        alloc_mem: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32,
        free_mem: extern "C" fn(usize, u64, *const c_void),
        bind_buf: extern "C" fn(usize, u64, u64, u64) -> i32,
        map_mem: extern "C" fn(usize, u64, u64, u64, u32, *mut *mut c_void) -> i32,
        unmap_mem: extern "C" fn(usize, u64),
        destroy_cmd_pool: extern "C" fn(usize, u64, *const c_void),
        begin_cmd: extern "C" fn(*mut c_void, *const c_void) -> i32,
        end_cmd: extern "C" fn(*mut c_void) -> i32,
        reset_cmd: extern "C" fn(*mut c_void, u32) -> i32,
        queue_submit: extern "C" fn(*mut c_void, u32, *const c_void, u64) -> i32,
        queue_wait_idle: extern "C" fn(*mut c_void) -> i32,
    }

    impl HeadlessVk {
        fn open() -> Option<Self> {
            extern "C" {
                fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
                fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
            }
            const RTLD_NOW: i32 = 2;
            let lib = unsafe { dlopen(c"libvulkan.so.1".as_ptr(), RTLD_NOW) };
            if lib.is_null() {
                return None;
            }
            let gipa: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void =
                as_fn(unsafe { dlsym(lib, c"vkGetInstanceProcAddr".as_ptr()) } as usize)?;
            let create_inst: extern "C" fn(*const c_void, *const c_void, *mut *mut c_void) -> i32 =
                as_fn(gipa(std::ptr::null_mut(), c"vkCreateInstance".as_ptr()) as usize)?;
            let mut ici = [0u8; 64];
            ici[0..4].copy_from_slice(&1u32.to_ne_bytes()); // VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO
            let mut instance = std::ptr::null_mut();
            if create_inst(ici.as_ptr() as *const c_void, std::ptr::null(), &mut instance) != VK_SUCCESS {
                return None;
            }
            let destroy_instance: extern "C" fn(*mut c_void, *const c_void) =
                as_fn(gipa(instance, c"vkDestroyInstance".as_ptr()) as usize)?;
            let enum_pds: extern "C" fn(*mut c_void, *mut u32, *mut *mut c_void) -> i32 =
                as_fn(gipa(instance, c"vkEnumeratePhysicalDevices".as_ptr()) as usize)?;
            let mut pd_count = 0u32;
            if enum_pds(instance, &mut pd_count, std::ptr::null_mut()) != VK_SUCCESS || pd_count == 0 {
                destroy_instance(instance, std::ptr::null());
                return None;
            }
            let mut pds = vec![std::ptr::null_mut(); pd_count as usize];
            if enum_pds(instance, &mut pd_count, pds.as_mut_ptr()) != VK_SUCCESS {
                destroy_instance(instance, std::ptr::null());
                return None;
            }
            let pd = pds[0];
            let qf_fn: extern "C" fn(*mut c_void, *mut u32, *mut VkQueueFamilyProperties) =
                as_fn(gipa(instance, c"vkGetPhysicalDeviceQueueFamilyProperties".as_ptr()) as usize)?;
            let mut qf_count = 0u32;
            qf_fn(pd, &mut qf_count, std::ptr::null_mut());
            let mut qfs = vec![VkQueueFamilyProperties::default(); qf_count as usize];
            qf_fn(pd, &mut qf_count, qfs.as_mut_ptr());
            let queue_family = qfs.iter().position(|q| q.queue_flags & QUEUE_COMPUTE_BIT != 0)? as u32;

            let prio = 1.0f32;
            #[repr(C)]
            struct QueueCi { s_type: u32, _p: u32, next: *const c_void, flags: u32, family: u32, count: u32, _p2: u32, prios: *const f32 }
            let qci = QueueCi { s_type: 2, _p: 0, next: std::ptr::null(), flags: 0, family: queue_family, count: 1, _p2: 0, prios: &prio };
            #[repr(C)]
            struct DevCi {
                s_type: u32, _p: u32, next: *const c_void, flags: u32, q_count: u32,
                p_qs: *const QueueCi, l_count: u32, _p2: u32, pp_l: *const *const c_char,
                e_count: u32, _p3: u32, pp_e: *const *const c_char, p_feats: *const c_void,
            }
            let dci = DevCi {
                s_type: 3, _p: 0, next: std::ptr::null(), flags: 0, q_count: 1, p_qs: &qci,
                l_count: 0, _p2: 0, pp_l: std::ptr::null(), e_count: 0, _p3: 0, pp_e: std::ptr::null(),
                p_feats: std::ptr::null(),
            };
            let create_dev: extern "C" fn(*mut c_void, *const DevCi, *const c_void, *mut *mut c_void) -> i32 =
                as_fn(gipa(instance, c"vkCreateDevice".as_ptr()) as usize)?;
            let mut device = std::ptr::null_mut();
            if create_dev(pd, &dci, std::ptr::null(), &mut device) != VK_SUCCESS {
                destroy_instance(instance, std::ptr::null());
                return None;
            }
            let gdpa: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void =
                as_fn(gipa(instance, c"vkGetDeviceProcAddr".as_ptr()) as usize)?;
            let get = |n: &CStr| gdpa(device, n.as_ptr()) as usize;

            let mut mem_types = Vec::new();
            let mp: extern "C" fn(*mut c_void, *mut c_void) =
                as_fn(gipa(instance, c"vkGetPhysicalDeviceMemoryProperties".as_ptr()) as usize)?;
            let mut raw = [0u8; 520];
            mp(pd, raw.as_mut_ptr() as *mut c_void);
            let mcount = u32::from_ne_bytes(raw[0..4].try_into().ok()?);
            for i in 0..mcount.min(32) as usize {
                let o = 4 + i * 8;
                mem_types.push(u32::from_ne_bytes(raw[o..o + 4].try_into().ok()?));
            }

            // Initialize GpuDecoder directly using our real SPIR-V shader.
            let create_shader_module: extern "C" fn(usize, *const VkShaderModuleCreateInfo, *const c_void, *mut u64) -> i32 =
                as_fn(get(c"vkCreateShaderModule"))?;
            let destroy_shader_module = as_fn(get(c"vkDestroyShaderModule"))?;
            let create_ds_layout: extern "C" fn(usize, *const VkDescriptorSetLayoutCreateInfo, *const c_void, *mut u64) -> i32 =
                as_fn(get(c"vkCreateDescriptorSetLayout"))?;
            let destroy_ds_layout = as_fn(get(c"vkDestroyDescriptorSetLayout"))?;
            let create_pipeline_layout: extern "C" fn(usize, *const VkPipelineLayoutCreateInfo, *const c_void, *mut u64) -> i32 =
                as_fn(get(c"vkCreatePipelineLayout"))?;
            let destroy_pipeline_layout = as_fn(get(c"vkDestroyPipelineLayout"))?;
            let create_compute_pipelines: extern "C" fn(usize, u64, u32, *const VkComputePipelineCreateInfo, *const c_void, *mut u64) -> i32 =
                as_fn(get(c"vkCreateComputePipelines"))?;
            let destroy_pipeline = as_fn(get(c"vkDestroyPipeline"))?;

            let mut shader_module = 0u64;
            let sm_ci = VkShaderModuleCreateInfo {
                s_type: ST_SHADER_MODULE_CREATE_INFO, _pad: 0, p_next: std::ptr::null(),
                flags: 0, _pad2: 0, code_size: std::mem::size_of_val(ETC_DECODE_SPV),
                p_code: ETC_DECODE_SPV.as_ptr(),
            };
            assert_eq!(create_shader_module(device as usize, &sm_ci, std::ptr::null(), &mut shader_module), VK_SUCCESS);

            let bindings = [
                VkDescriptorSetLayoutBinding { binding: 0, descriptor_type: DESCRIPTOR_TYPE_STORAGE_BUFFER, descriptor_count: 1, stage_flags: SHADER_STAGE_COMPUTE_BIT, p_immutable_samplers: std::ptr::null() },
                VkDescriptorSetLayoutBinding { binding: 1, descriptor_type: DESCRIPTOR_TYPE_STORAGE_BUFFER, descriptor_count: 1, stage_flags: SHADER_STAGE_COMPUTE_BIT, p_immutable_samplers: std::ptr::null() },
            ];
            let dsl_ci = VkDescriptorSetLayoutCreateInfo { s_type: ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, _pad: 0, p_next: std::ptr::null(), flags: 0, binding_count: 2, p_bindings: bindings.as_ptr() };
            let mut ds_layout = 0u64;
            assert_eq!(create_ds_layout(device as usize, &dsl_ci, std::ptr::null(), &mut ds_layout), VK_SUCCESS);

            let pc_range = VkPushConstantRange { stage_flags: SHADER_STAGE_COMPUTE_BIT, offset: 0, size: std::mem::size_of::<GpuPushConstants>() as u32 };
            let pl_ci = VkPipelineLayoutCreateInfo { s_type: ST_PIPELINE_LAYOUT_CREATE_INFO, _pad: 0, p_next: std::ptr::null(), flags: 0, set_layout_count: 1, p_set_layouts: &ds_layout, push_constant_range_count: 1, _pad2: 0, p_push_constant_ranges: &pc_range };
            let mut pipeline_layout = 0u64;
            assert_eq!(create_pipeline_layout(device as usize, &pl_ci, std::ptr::null(), &mut pipeline_layout), VK_SUCCESS);

            let cp_ci = VkComputePipelineCreateInfo {
                s_type: ST_COMPUTE_PIPELINE_CREATE_INFO, _pad: 0, p_next: std::ptr::null(), flags: 0, _pad2: 0,
                stage: VkPipelineShaderStageCreateInfo { s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO, _pad: 0, p_next: std::ptr::null(), flags: 0, stage: SHADER_STAGE_COMPUTE_BIT, module: shader_module, p_name: c"main".as_ptr(), p_specialization_info: std::ptr::null() },
                layout: pipeline_layout, base_pipeline_handle: 0, base_pipeline_index: -1, _pad3: 0,
            };
            let mut pipeline = 0u64;
            assert_eq!(create_compute_pipelines(device as usize, 0, 1, &cp_ci, std::ptr::null(), &mut pipeline), VK_SUCCESS);

            let gpu = GpuDecoder {
                shader_module, ds_layout, pipeline_layout, pipeline,
                compute_queue_families: qfs.iter().map(|q| q.queue_flags & QUEUE_COMPUTE_BIT != 0).collect(),
                destroy_shader_module, destroy_ds_layout, destroy_pipeline_layout, destroy_pipeline,
                create_ds_pool: as_fn(get(c"vkCreateDescriptorPool"))?,
                destroy_ds_pool: as_fn(get(c"vkDestroyDescriptorPool"))?,
                reset_ds_pool: as_fn(get(c"vkResetDescriptorPool"))?,
                alloc_ds: as_fn(get(c"vkAllocateDescriptorSets"))?,
                update_ds: as_fn(get(c"vkUpdateDescriptorSets"))?,
                cmd_bind_pipeline: as_fn(get(c"vkCmdBindPipeline"))?,
                cmd_bind_ds: as_fn(get(c"vkCmdBindDescriptorSets"))?,
                cmd_push_constants: as_fn(get(c"vkCmdPushConstants"))?,
                cmd_dispatch: as_fn(get(c"vkCmdDispatch"))?,
                cmd_pipeline_barrier: as_fn(get(c"vkCmdPipelineBarrier"))?,
                free_gpu_chunks: Mutex::new(Vec::new()),
                free_ds_pools: Mutex::new(Vec::new()),
            };

            let get_queue: extern "C" fn(*mut c_void, u32, u32, *mut *mut c_void) = as_fn(get(c"vkGetDeviceQueue"))?;
            let mut queue = std::ptr::null_mut();
            get_queue(device, queue_family, 0, &mut queue);

            let create_cmd_pool: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32 = as_fn(get(c"vkCreateCommandPool"))?;
            let mut cp = [0u8; 24];
            cp[0..4].copy_from_slice(&39u32.to_ne_bytes()); // VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO
            cp[16..20].copy_from_slice(&2u32.to_ne_bytes()); // RESET_COMMAND_BUFFER_BIT
            cp[20..24].copy_from_slice(&queue_family.to_ne_bytes());
            let mut cmd_pool = 0u64;
            assert_eq!(create_cmd_pool(device as usize, cp.as_ptr() as *const c_void, std::ptr::null(), &mut cmd_pool), VK_SUCCESS);

            let alloc_cmds: extern "C" fn(usize, *const c_void, *mut *mut c_void) -> i32 = as_fn(get(c"vkAllocateCommandBuffers"))?;
            let mut cbai = [0u8; 32];
            cbai[0..4].copy_from_slice(&40u32.to_ne_bytes()); // VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO
            cbai[16..24].copy_from_slice(&cmd_pool.to_ne_bytes());
            cbai[28..32].copy_from_slice(&1u32.to_ne_bytes());
            let mut cmd = std::ptr::null_mut();
            assert_eq!(alloc_cmds(device as usize, cbai.as_ptr() as *const c_void, &mut cmd), VK_SUCCESS);

            Some(Self {
                instance, device, queue, cmd_pool, cmd, gpu, mem_types,
                destroy_instance,
                destroy_device: as_fn(get(c"vkDestroyDevice"))?,
                create_buffer: as_fn(get(c"vkCreateBuffer"))?,
                destroy_buffer: as_fn(get(c"vkDestroyBuffer"))?,
                buffer_reqs: as_fn(get(c"vkGetBufferMemoryRequirements"))?,
                alloc_mem: as_fn(get(c"vkAllocateMemory"))?,
                free_mem: as_fn(get(c"vkFreeMemory"))?,
                bind_buf: as_fn(get(c"vkBindBufferMemory"))?,
                map_mem: as_fn(get(c"vkMapMemory"))?,
                unmap_mem: as_fn(get(c"vkUnmapMemory"))?,
                destroy_cmd_pool: as_fn(get(c"vkDestroyCommandPool"))?,
                begin_cmd: as_fn(get(c"vkBeginCommandBuffer"))?,
                end_cmd: as_fn(get(c"vkEndCommandBuffer"))?,
                reset_cmd: as_fn(get(c"vkResetCommandBuffer"))?,
                queue_submit: as_fn(get(c"vkQueueSubmit"))?,
                queue_wait_idle: as_fn(get(c"vkQueueWaitIdle"))?,
            })
        }

        fn alloc_host_ssbo(&self, size: u64) -> (u64, u64, *mut u8) {
            let mut bci = [0u8; 56];
            bci[0..4].copy_from_slice(&ST_BUFFER_CREATE_INFO.to_ne_bytes());
            bci[24..32].copy_from_slice(&size.to_ne_bytes());
            let usage = BUFFER_USAGE_TRANSFER_SRC | BUFFER_USAGE_STORAGE_BUFFER;
            bci[32..36].copy_from_slice(&usage.to_ne_bytes());
            let mut buf = 0u64;
            assert_eq!((self.create_buffer)(self.device as usize, bci.as_ptr() as *const c_void, std::ptr::null(), &mut buf), VK_SUCCESS);
            let mut req = [0u64; 3];
            (self.buffer_reqs)(self.device as usize, buf, &mut req);
            let bits = req[2] as u32;
            let wanted = MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT;
            let cached = wanted | MEMORY_HOST_CACHED;
            let type_idx = self.mem_types.iter().enumerate()
                .find(|(i, &f)| bits & (1 << i) != 0 && f & cached == cached)
                .or_else(|| self.mem_types.iter().enumerate().find(|(i, &f)| bits & (1 << i) != 0 && f & wanted == wanted))
                .unwrap().0 as u32;
            let mut mai = [0u8; 32];
            mai[0..4].copy_from_slice(&ST_MEMORY_ALLOCATE_INFO.to_ne_bytes());
            mai[16..24].copy_from_slice(&req[0].to_ne_bytes());
            mai[24..28].copy_from_slice(&type_idx.to_ne_bytes());
            let mut mem = 0u64;
            assert_eq!((self.alloc_mem)(self.device as usize, mai.as_ptr() as *const c_void, std::ptr::null(), &mut mem), VK_SUCCESS);
            assert_eq!((self.bind_buf)(self.device as usize, buf, mem, 0), VK_SUCCESS);
            let mut mapped = std::ptr::null_mut();
            assert_eq!((self.map_mem)(self.device as usize, mem, 0, WHOLE_SIZE, 0, &mut mapped), VK_SUCCESS);
            (buf, mem, mapped as *mut u8)
        }

        fn dispatch_decode(
            &self,
            src_buf: u64,
            dst_buf: u64,
            ds: u64,
            pc: &GpuPushConstants,
            w: usize,
            h: usize,
            layers: usize,
        ) {
            let src_info = VkDescriptorBufferInfo { buffer: src_buf, offset: 0, range: WHOLE_SIZE };
            let dst_info = VkDescriptorBufferInfo { buffer: dst_buf, offset: 0, range: WHOLE_SIZE };
            let writes = [
                VkWriteDescriptorSet { s_type: ST_WRITE_DESCRIPTOR_SET, _pad: 0, p_next: std::ptr::null(), dst_set: ds, dst_binding: 0, dst_array_element: 0, descriptor_count: 1, descriptor_type: DESCRIPTOR_TYPE_STORAGE_BUFFER, p_image_info: std::ptr::null(), p_buffer_info: &src_info, p_texel_buffer_view: std::ptr::null() },
                VkWriteDescriptorSet { s_type: ST_WRITE_DESCRIPTOR_SET, _pad: 0, p_next: std::ptr::null(), dst_set: ds, dst_binding: 1, dst_array_element: 0, descriptor_count: 1, descriptor_type: DESCRIPTOR_TYPE_STORAGE_BUFFER, p_image_info: std::ptr::null(), p_buffer_info: &dst_info, p_texel_buffer_view: std::ptr::null() },
            ];
            (self.gpu.update_ds)(self.device as usize, 2, writes.as_ptr(), 0, std::ptr::null());

            assert_eq!((self.reset_cmd)(self.cmd, 0), VK_SUCCESS);
            let mut bi = [0u8; 32];
            bi[0..4].copy_from_slice(&42u32.to_ne_bytes()); // VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO
            assert_eq!((self.begin_cmd)(self.cmd, bi.as_ptr() as *const c_void), VK_SUCCESS);
            (self.gpu.cmd_bind_pipeline)(self.cmd, PIPELINE_BIND_POINT_COMPUTE, self.gpu.pipeline);
            (self.gpu.cmd_bind_ds)(self.cmd, PIPELINE_BIND_POINT_COMPUTE, self.gpu.pipeline_layout, 0, 1, &ds, 0, std::ptr::null());
            (self.gpu.cmd_push_constants)(self.cmd, self.gpu.pipeline_layout, SHADER_STAGE_COMPUTE_BIT, 0, std::mem::size_of::<GpuPushConstants>() as u32, pc as *const GpuPushConstants as *const c_void);
            (self.gpu.cmd_dispatch)(self.cmd, w.div_ceil(4).div_ceil(8) as u32, h.div_ceil(4).div_ceil(8) as u32, layers as u32);
            let post = VkBufferMemoryBarrier {
                s_type: ST_BUFFER_MEMORY_BARRIER, _pad: 0, p_next: std::ptr::null(),
                src_access_mask: ACCESS_SHADER_WRITE_BIT, dst_access_mask: 0x2000 /* HOST_READ_BIT */,
                src_queue_family_index: QUEUE_FAMILY_IGNORED, dst_queue_family_index: QUEUE_FAMILY_IGNORED,
                buffer: dst_buf, offset: 0, size: WHOLE_SIZE,
            };
            (self.gpu.cmd_pipeline_barrier)(self.cmd, PIPELINE_STAGE_COMPUTE_SHADER_BIT, PIPELINE_STAGE_HOST_BIT, 0, 0, std::ptr::null(), 1, &post, 0, std::ptr::null());
            assert_eq!((self.end_cmd)(self.cmd), VK_SUCCESS);

            let mut si = [0u64; 9];
            si[0] = 4; // VK_STRUCTURE_TYPE_SUBMIT_INFO
            si[5] = 1;
            let cmd_val = self.cmd as usize;
            si[6] = &cmd_val as *const usize as u64;
            assert_eq!((self.queue_submit)(self.queue, 1, si.as_ptr() as *const c_void, 0), VK_SUCCESS);
            assert_eq!((self.queue_wait_idle)(self.queue), VK_SUCCESS);
        }
    }

    impl Drop for HeadlessVk {
        fn drop(&mut self) {
            (self.queue_wait_idle)(self.queue);
            (self.gpu.destroy_pipeline)(self.device as usize, self.gpu.pipeline, std::ptr::null());
            (self.gpu.destroy_pipeline_layout)(self.device as usize, self.gpu.pipeline_layout, std::ptr::null());
            (self.gpu.destroy_ds_layout)(self.device as usize, self.gpu.ds_layout, std::ptr::null());
            (self.gpu.destroy_shader_module)(self.device as usize, self.gpu.shader_module, std::ptr::null());
            (self.destroy_cmd_pool)(self.device as usize, self.cmd_pool, std::ptr::null());
            (self.destroy_device)(self.device, std::ptr::null());
            (self.destroy_instance)(self.instance, std::ptr::null());
        }
    }

    #[test]
    fn gpu_compute_decoder_matches_cpu_bit_for_bit_across_all_formats() {
        let Some(vk) = HeadlessVk::open() else {
            eprintln!("[test] no Vulkan compute device available; skipping GPU compute verification");
            return;
        };
        let pool_size = VkDescriptorPoolSize { ty: DESCRIPTOR_TYPE_STORAGE_BUFFER, descriptor_count: 16 };
        let pool_ci = VkDescriptorPoolCreateInfo {
            s_type: ST_DESCRIPTOR_POOL_CREATE_INFO, _pad: 0, p_next: std::ptr::null(),
            flags: 0, max_sets: 8, pool_size_count: 1, _pad2: 0, p_pool_sizes: &pool_size,
        };
        let mut ds_pool = 0u64;
        assert_eq!((vk.gpu.create_ds_pool)(vk.device as usize, &pool_ci, std::ptr::null(), &mut ds_pool), VK_SUCCESS);
        let ds_ai = VkDescriptorSetAllocateInfo {
            s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO, _pad: 0, p_next: std::ptr::null(),
            descriptor_pool: ds_pool, descriptor_set_count: 1, _pad2: 0, p_set_layouts: &vk.gpu.ds_layout,
        };
        let mut ds = 0u64;
        assert_eq!((vk.gpu.alloc_ds)(vk.device as usize, &ds_ai, &mut ds), VK_SUCCESS);

        let (src_buf, src_mem, src_ptr) = vk.alloc_host_ssbo(2 << 20);
        let (dst_buf, dst_mem, dst_ptr) = vk.alloc_host_ssbo(4 << 20);

        let formats = [
            EtcFormat::Rgb8,
            EtcFormat::Rgb8A1,
            EtcFormat::Rgba8,
            EtcFormat::R11,
            EtcFormat::R11Signed,
            EtcFormat::Rg11,
            EtcFormat::Rg11Signed,
        ];
        // Exercise multiple-of-4, cropped odd-width (tests R11 row-word padding),
        // padded row_blocks, non-zero byte offset, and multi-layer arrays.
        let shapes = [
            (64usize, 64usize, 16usize, 1usize, 0usize),
            (37usize, 29usize, 12usize, 3usize, 64usize),
            (1usize, 1usize, 1usize, 2usize, 16usize),
        ];

        let mut seed = 0x51a7_9921u32;
        for fmt in formats {
            for (w, h, row_blocks, layers, src_off) in shapes {
                let rows_of_blocks = h.div_ceil(4) + 1;
                let layer_stride = row_blocks * rows_of_blocks * fmt.block_bytes();
                let src_len = layers * layer_stride;
                let mut src_bytes = vec![0u8; src_off + src_len];
                for b in &mut src_bytes[src_off..] {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    *b = (seed >> 24) as u8;
                }
                unsafe { std::ptr::copy_nonoverlapping(src_bytes.as_ptr(), src_ptr, src_bytes.len()) };

                let dst_row_words = match fmt {
                    EtcFormat::R11 | EtcFormat::R11Signed => w.div_ceil(2),
                    _ => w,
                };
                let dst_word_off = 16usize;
                let dst_bytes = dst_row_words * 4 * h * layers;
                unsafe { std::ptr::write_bytes(dst_ptr, 0xCC, dst_word_off * 4 + dst_bytes) };

                let pc = GpuPushConstants {
                    src_byte_offset: src_off as u32,
                    dst_word_offset: dst_word_off as u32,
                    width: w as u32,
                    height: h as u32,
                    row_blocks: row_blocks as u32,
                    layer_stride: layer_stride as u32,
                    dst_row_words: dst_row_words as u32,
                    fmt_and_layers: etc_format_code(fmt) | ((layers as u32) << 8),
                };
                vk.dispatch_decode(src_buf, dst_buf, ds, &pc, w, h, layers);

                let tb = fmt.texel_bytes();
                let mut cpu_layer = vec![0u8; w * h * tb];
                let gpu_out = unsafe {
                    std::slice::from_raw_parts(dst_ptr.add(dst_word_off * 4), dst_bytes)
                };
                for layer in 0..layers {
                    let surface = Surface {
                        data: &src_bytes[src_off + layer * layer_stride..],
                        row_blocks,
                        rows_of_blocks,
                    };
                    etc_decode::decode_region(fmt, &surface, w, h, &mut cpu_layer).unwrap();
                    let gpu_row_bytes = dst_row_words * 4;
                    let cpu_row_bytes = w * tb;
                    for y in 0..h {
                        let g_start = (layer * h + y) * gpu_row_bytes;
                        let c_start = y * cpu_row_bytes;
                        assert_eq!(
                            &gpu_out[g_start..g_start + cpu_row_bytes],
                            &cpu_layer[c_start..c_start + cpu_row_bytes],
                            "GPU vs CPU mismatch for {fmt:?} {w}x{h} layer {layer} row {y}"
                        );
                    }
                }
            }
        }

        (vk.unmap_mem)(vk.device as usize, src_mem);
        (vk.unmap_mem)(vk.device as usize, dst_mem);
        (vk.destroy_buffer)(vk.device as usize, src_buf, std::ptr::null());
        (vk.free_mem)(vk.device as usize, src_mem, std::ptr::null());
        (vk.destroy_buffer)(vk.device as usize, dst_buf, std::ptr::null());
        (vk.free_mem)(vk.device as usize, dst_mem, std::ptr::null());
        (vk.gpu.destroy_ds_pool)(vk.device as usize, ds_pool, std::ptr::null());
    }

    #[test]
    #[ignore]
    fn microbench_gpu_vs_cpu_decode() {
        let Some(vk) = HeadlessVk::open() else {
            println!("no Vulkan compute device available");
            return;
        };
        let pool_size = VkDescriptorPoolSize { ty: DESCRIPTOR_TYPE_STORAGE_BUFFER, descriptor_count: 16 };
        let pool_ci = VkDescriptorPoolCreateInfo {
            s_type: ST_DESCRIPTOR_POOL_CREATE_INFO, _pad: 0, p_next: std::ptr::null(),
            flags: 0, max_sets: 8, pool_size_count: 1, _pad2: 0, p_pool_sizes: &pool_size,
        };
        let mut ds_pool = 0u64;
        assert_eq!((vk.gpu.create_ds_pool)(vk.device as usize, &pool_ci, std::ptr::null(), &mut ds_pool), VK_SUCCESS);
        let ds_ai = VkDescriptorSetAllocateInfo {
            s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO, _pad: 0, p_next: std::ptr::null(),
            descriptor_pool: ds_pool, descriptor_set_count: 1, _pad2: 0, p_set_layouts: &vk.gpu.ds_layout,
        };
        let mut ds = 0u64;
        assert_eq!((vk.gpu.alloc_ds)(vk.device as usize, &ds_ai, &mut ds), VK_SUCCESS);

        let (src_buf, src_mem, src_ptr) = vk.alloc_host_ssbo(2 << 20);
        // Device-local scratch output (identical to production GPU path).
        let mut bci = [0u8; 56];
        bci[0..4].copy_from_slice(&ST_BUFFER_CREATE_INFO.to_ne_bytes());
        bci[24..32].copy_from_slice(&(4u64 << 20).to_ne_bytes());
        bci[32..36].copy_from_slice(&(BUFFER_USAGE_TRANSFER_SRC | BUFFER_USAGE_STORAGE_BUFFER).to_ne_bytes());
        let mut dst_buf = 0u64;
        assert_eq!((vk.create_buffer)(vk.device as usize, bci.as_ptr() as *const c_void, std::ptr::null(), &mut dst_buf), VK_SUCCESS);
        let mut req = [0u64; 3];
        (vk.buffer_reqs)(vk.device as usize, dst_buf, &mut req);
        let bits = req[2] as u32;
        let dev_type = vk.mem_types.iter().enumerate()
            .find(|(i, &f)| bits & (1 << i) != 0 && f & MEMORY_DEVICE_LOCAL != 0)
            .unwrap().0 as u32;
        let mut mai = [0u8; 32];
        mai[0..4].copy_from_slice(&ST_MEMORY_ALLOCATE_INFO.to_ne_bytes());
        mai[16..24].copy_from_slice(&req[0].to_ne_bytes());
        mai[24..28].copy_from_slice(&dev_type.to_ne_bytes());
        let mut dst_mem = 0u64;
        assert_eq!((vk.alloc_mem)(vk.device as usize, mai.as_ptr() as *const c_void, std::ptr::null(), &mut dst_mem), VK_SUCCESS);
        assert_eq!((vk.bind_buf)(vk.device as usize, dst_buf, dst_mem, 0), VK_SUCCESS);

        let (w, h) = (1024usize, 1024usize);
        let row_blocks = w / 4;
        let mut seed = 0x1234_5678u32;
        for fmt in [EtcFormat::Rgb8, EtcFormat::Rgba8, EtcFormat::Rg11Signed] {
            let src_len = row_blocks * (h / 4) * fmt.block_bytes();
            let src_bytes: Vec<u8> = (0..src_len).map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            }).collect();
            unsafe { std::ptr::copy_nonoverlapping(src_bytes.as_ptr(), src_ptr, src_bytes.len()) };

            let pc = GpuPushConstants {
                src_byte_offset: 0,
                dst_word_offset: 0,
                width: w as u32,
                height: h as u32,
                row_blocks: row_blocks as u32,
                layer_stride: src_len as u32,
                dst_row_words: w as u32,
                fmt_and_layers: etc_format_code(fmt) | (1u32 << 8),
            };
            // Warm up
            vk.dispatch_decode(src_buf, dst_buf, ds, &pc, w, h, 1);

            let mut gpu_samples = Vec::new();
            for _ in 0..5 {
                let t0 = std::time::Instant::now();
                for _ in 0..20 {
                    vk.dispatch_decode(src_buf, dst_buf, ds, &pc, w, h, 1);
                }
                gpu_samples.push(t0.elapsed().as_secs_f64() / 20.0 * 1000.0);
            }
            gpu_samples.sort_by(f64::total_cmp);

            let mut cpu_dst = vec![0u8; w * h * fmt.texel_bytes()];
            let surface = Surface { data: &src_bytes, row_blocks, rows_of_blocks: h / 4 };
            etc_decode::decode_region(fmt, &surface, w, h, &mut cpu_dst).unwrap();
            let mut cpu_samples = Vec::new();
            for _ in 0..5 {
                let t0 = std::time::Instant::now();
                for _ in 0..10 {
                    etc_decode::decode_region(fmt, &surface, w, h, &mut cpu_dst).unwrap();
                    std::hint::black_box(&cpu_dst);
                }
                cpu_samples.push(t0.elapsed().as_secs_f64() / 10.0 * 1000.0);
            }
            cpu_samples.sort_by(f64::total_cmp);
            println!(
                "1024x1024 {fmt:?}: CPU serial={:.3} ms, GPU compute (incl. submit+waitIdle)={:.3} ms ({:.1}x faster)",
                cpu_samples[2],
                gpu_samples[2],
                cpu_samples[2] / gpu_samples[2]
            );
        }

        (vk.unmap_mem)(vk.device as usize, src_mem);
        (vk.destroy_buffer)(vk.device as usize, src_buf, std::ptr::null());
        (vk.free_mem)(vk.device as usize, src_mem, std::ptr::null());
        (vk.destroy_buffer)(vk.device as usize, dst_buf, std::ptr::null());
        (vk.free_mem)(vk.device as usize, dst_mem, std::ptr::null());
        (vk.gpu.destroy_ds_pool)(vk.device as usize, ds_pool, std::ptr::null());
    }
}

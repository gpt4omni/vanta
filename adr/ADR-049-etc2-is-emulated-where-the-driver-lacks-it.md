---
title: "ADR-049: ETC2/EAC is emulated where the driver lacks the feature"
---
**Status:** accepted
**Supersedes:** the "nothing is translated" half of [ADR-042](/adr/ADR-042-texture-format-query-observability); its counters and the test-only mask stand
**Related:** [ADR-001](/adr/ADR-001-in-process-hooking), [ADR-042](/adr/ADR-042-texture-format-query-observability), [ADR-046](/adr/ADR-046-nvidia-is-gated-on-the-vendor-id)

## Context

ADR-042 measured the engine's per-format ETC2 queries on Intel, found them
unchanged by masking, and concluded that the premise "NVIDIA lacks ETC2, so
something is missing" did not hold. That conclusion was drawn from the wrong
signal. The mask only ever touched `vkGetPhysicalDeviceFormatProperties`. The
engine decides whether to use ETC from the `textureCompressionETC2` device
feature, which the mask never changed, so an unchanged caps line said nothing
about the feature.

A tester on an RTX 5070 supplied the missing data point. NVIDIA reports
`textureCompressionETC2` false, the engine's caps line reads `ETC1 0 ETC2 0`,
and ETC1 built-ins such as the plastic detail texture go missing. With the
emulation below the same line reads `ETC1 1 ETC2 1`. ADR-042 named that exact
observation as what would change its mind.

## Decision

On a physical device whose `textureCompressionETC2` is false, Cordial answers
as though it were true and serves the engine's ETC2 and EAC images itself:

- the feature is reported set in `vkGetPhysicalDeviceFeatures` and
  `Features2`, and cleared again in `vkCreateDevice` so the driver is never
  asked to enable what it lacks;
- format and image-format queries for the ten ETC2/EAC formats are answered
  from the matching RGBA8, R16 or RG16 format, for sampling and transfer only;
- `vkCreateImage` and `vkCreateImageView` use that substitute format, as does
  `vkGetDeviceImageMemoryRequirements` and its sparse variant, so the image
  the driver sizes and creates is the one that is used;
- `vkCmdCopyBufferToImage(2)` prefers a device-side Vulkan 1.0 compute shader
  (`etc_decode_gpu.comp`, precompiled SPIR-V in `etc_decode_spv.rs`) whenever
  the command buffer belongs to a compute-capable queue family, the source
  upload buffer carries `VK_BUFFER_USAGE_STORAGE_BUFFER_BIT` (ORed in at
  `vkCreateBuffer` on `TRANSFER_SRC` buffers), and the command buffer has not
  bound a compute pipeline of its own. The shader decodes straight from the
  engine's upload buffer into a pooled `DEVICE_LOCAL` scratch buffer, emits a
  compute-to-transfer buffer barrier, and forwards `vkCmdCopyBufferToImage`
  to the driver. The destination `VkImage` stays pure `SAMPLED | TRANSFER_DST`
  so driver texture compression (such as NVIDIA DCC) is preserved.
- Where the GPU compute path cannot be used (transfer-only queue family,
  compute pipeline already bound on the command buffer, or
  `VANTA_NO_GPU_ETC_DECODE=1`), `vkCmdCopyBufferToImage(2)` falls back to
  pooled host-visible staging (`MEMORY_HOST_CACHED` preferred) and queues
  pipelined CPU decoding from an owned compressed-byte snapshot. Submit joins
  the work and checks for writes since recording before the driver may execute
  the recorded staging copy.

The CPU decoder is `etc_decode.rs`, the GPU compute shader is
`etc_decode_gpu.comp` / `etc_decode_spv.rs`, and the shim is `vulkan_etc.rs`.

**The gate is the driver lacking the feature, and not the vendor id.** ADR-046
gates the other NVIDIA behaviour on `vendorID == 0x10DE` because those
workarounds are about one vendor's driver. This one is about a capability bit.
A non-NVIDIA driver without ETC2 (a software rasteriser, an older mobile-class
part) has the identical problem and should be served the same way, and an
NVIDIA driver that grows the feature should not be shimmed. The vendor id
would be right for the first reason and wrong for the second. It also means
the `CORDIAL_FORCE_GPU_VENDOR` switch does not turn this on.

**Device-level hooks are installed on a device only if its physical device is
emulating.** An Intel or AMD device resolves the driver's own function. The
instance-level lookup cannot know which device a pointer will be used on, so
it stays hooked, and each hook forwards straight through for a device that is
not emulating. With `CORDIAL_ANDROID_TRACE` set every device is hooked so that
the passthroughs are traced.

## Controls

- `VANTA_NO_GPU_ETC_DECODE=1` disables the GPU compute shader path and forces
  the pooled CPU fallback, for A/B comparison or driver workarounds.
- `CORDIAL_NO_ETC_EMULATION=1` turns it off; the engine then sees `ETC1 0
  ETC2 0` on NVIDIA again.
- `CORDIAL_FORCE_ETC_EMULATION=1` turns it on for a driver that has ETC2
  natively, so that native and decoded output can be compared on one machine
  without NVIDIA hardware (the same family as `CORDIAL_FORCE_GPU_VENDOR`). It
  is named in `cordial-run --help` and set by no packaging script. If both are
  set, `NO` wins.

## Failure behaviour

If the engine was told ETC2 works and the state to serve it is missing, Cordial
fails by name rather than handing the driver a raw ETC format. A `vkCreateImage`
with an ETC2/EAC format on a device that is emulating but has no emulation
state (the host calls staging needs could not be resolved, or a second device
replaced the first) returns `VK_ERROR_FORMAT_NOT_SUPPORTED` and counts into
`ETC_UNHANDLED`. `CORDIAL_COUNT_GL=1` prints that counter, and a non-zero value
after joining a game is the thing to report.

A `VkPhysicalDeviceFeatures2` that is not the first node of the
`vkCreateDevice` chain cannot be copied, because the nodes ahead of it are
structures this code has no reason to size. Its one feature word is cleared in
place for the duration of the call and restored when the call returns. The
alternative, passing the feature through, is a driver that lacks it refusing to
create the device and a client that never starts.

## What is deliberately not handled

- `vkCmdBlitImage*` on an emulated image, `vkCmdCopyImageToBuffer*` from one,
  copies of an emulated image to a differently formatted one, and 3D copies
  are refused or counted rather than translated. The engine was not seen to
  make them, and `ETC_UNHANDLED` says so if that changes.
- An ETC2 format inside a `VkImageFormatListCreateInfo` is forwarded
  unchanged and counted.
- Metadata lookup, copying compressed source bytes and submit-time hashing
  still take the shared state lock. Decoding and completion waits do not.
  Staging allocation and retirement use a per-command-buffer lock. Metadata
  contention, extra snapshot memory and the pool's competition with engine
  workers have not been measured in game.
- Only consecutive identical compressed blocks are memoised. No wider block
  cache was added: the synthetic workloads below do not establish a realistic
  hit rate that would justify its extra lookup and storage.

## CPU fast path and VM microbenchmarks

Full interior blocks write four row slices directly into staging, rather than
copying individual texels through a second block buffer. Cropped edge blocks
retain uncropped texels in scratch. Identical consecutive 8/16-byte blocks
reuse the preceding decoded result, including across cropped edges. ETC colour
palettes are clamped once per block/subblock, not once per pixel.

The first fast path used a lazily-started persistent pool for regions of at
least 256x256 texels, with `available_parallelism().min(8)` helpers cached at
initialisation. The synchronous caller claimed bands as well. The pipeline
below replaces synchronous recording and removes the size threshold in
production. The old synchronous CPU-job path remains only as a test control.
Tests compare concurrent multi-layer jobs against serial decoding and exercise
progress with no helpers at all.

Submit validation still reads every compressed source byte. Its hash now uses
four independent lanes over 32-byte strides and a byte-wise tail; hashes are
ephemeral record/submit state, not persisted digests. Tests change every bit in
each lane and the tail. Both submit wrappers visit command buffers with lazy
iterators instead of allocating temporary vectors.

The following table records the first fast path, before pipelining. These are
**CPU microbenchmarks on this VM, not in-game measurements**: eight
reported Intel Xeon vCPUs under KVM, Ubuntu 24.04 build container, Rust 1.99.1,
release optimisation with `CARGO_PROFILE_RELEASE_DEBUG=0`, no extra target CPU
flags. The baseline is `66a212e` (benchmark-only changes over `d694229`). Each
entry is the median of five batches after warm-up. Decoder batches contain ten
1024x1024 images; CPU job batches contain thirty images with warm helpers;
hash batches read approximately 128 MiB. Inputs are deterministic synthetic
compressed bytes: independent blocks, one repeated block, or runs of eight.
The CPU job benchmark does not include Vulkan calls, staging allocation,
submit hashing or contention with engine workers.

| VM microbenchmark | Before | After |
| --- | ---: | ---: |
| RGB8 random, ms/image | 14.919 | 2.109 |
| RGB8 flat, ms/image | 5.442 | 0.791 |
| RGB8 runs, ms/image | 7.154 | 0.920 |
| RGBA8 random, ms/image | 15.123 | 2.763 |
| RGBA8 flat, ms/image | 5.740 | 0.853 |
| RGBA8 runs, ms/image | 7.278 | 1.016 |
| RG11 signed random, ms/image | 12.594 | 11.143 |
| RG11 signed flat, ms/image | 5.131 | 0.791 |
| RG11 signed runs, ms/image | 6.218 | 2.134 |
| Hash 4,096 bytes, GB/s | 6.742 | 24.302 |
| Hash 4,194,335 bytes, GB/s | 6.328 | 21.497 |
| CPU job 128x128 RGBA8, ms/image | 0.219 | 0.039 |
| CPU job 256x256 RGBA8, ms/image | 0.380 | 0.058 |
| CPU job 1024x1024 RGBA8, ms/image | 3.278 | 0.661 |

Reproduce without loading an engine:

```sh
CARGO_PROFILE_RELEASE_DEBUG=0 cargo test --release -p vanta-runtime --lib microbench -- --ignored --nocapture --test-threads=1
```

The existing independent decoder vectors pass, alongside per-pixel palette
references, padded/cropped region comparisons and repeated-edge cases for all
seven formats. **INFERRED:** less CPU work and no per-layer thread creation may
reduce texture-upload stalls in game. No game was run; neither the target
NVIDIA machine nor an in-game frame-rate or latency improvement was measured.

## Pipelined recording and submit

Every non-empty readable region is asynchronous, including small regions.
Recording takes an owned snapshot under `state()` while the mapping is live,
then releases that lock before preparing or publishing work. The driver copy
is recorded before publication. Workers compute the snapshot hash and decode
disjoint row bands. They never take `state()` or a command-buffer lock.
The queue rotates jobs after each band claim, so uploads from different
regions and command buffers can occupy helpers concurrently; it does not wait
for one whole image to finish before scheduling another.

Command state is `Arc<Mutex<CmdState>>`. A caller clones its handle under
`state()` and drops the global guard before locking that command. Submit joins
recorded jobs, taking unclaimed bands itself, then checks the live source hash.
Changed or previously unreadable sources get new owned snapshots and queued
decodes; all such work is joined before forwarding to the driver. There is no
production synchronous size fallback. A serial test control remains for the
measurements below.

`vkCmdExecuteCommands` records primary-to-secondary links for command buffers
with emulated uploads, including nested execution. Submit walks those links
with duplicate/cycle protection, joining and rechecking each reachable region
without holding `state()` or another command lock. Joining only at execute
recording would miss source writes made afterwards. Reset/free drops the
primary's links with its command state; secondary staging keeps its existing
retirement rules.

Unmap/free remove the mapping under `state()` and clone the relevant command
handles, then join affected jobs without that lock before forwarding. No
worker reads the original mapping. Begin/reset/free of a command buffer,
pool reset/destruction and device destruction detach command handles under
`state()`, then join their pending jobs under their individual command locks
before freeing staging. The per-command lock prevents submit or retirement
from freeing an output while a join/re-decode is using it. No path takes a
command lock or waits for decoding while holding `state()`.

Input/shape errors detectable at record still refuse the region before the
driver copy. Unexpected worker errors/panics are retained until submit and
reported through `ETC_UNHANDLED`, without inventing a Vulkan submit failure.
Unlike the old record-time skip, a recorded copy cannot be retracted: after
joining every band, that region is cleared to zero before execution. Errors
remain reportable if an earlier unmap already joined the failed work.

Tests cover both submit variants, late writes, unreadable/empty/malformed
regions, error reporting, eight concurrent recording threads, interleaved job
claims, and all seven memory/command/device retirement hooks. A deliberately
held band verifies retirement cannot forward early and that another thread
can still acquire `state()` while retirement waits.

### VM recording and burst measurements

Same release/container/toolchain as above; no game or Vulkan driver executes.
The harness invokes the real `emulate_copy` and pending-submit logic, using
mock driver functions and preallocated coherent staging. It excludes driver
costs, initial staging allocation, GPU execution and engine contention. Single
images compare against measurement-only baseline `4774ce7`. Five warmed batches
contain thirty record/immediate-submit pairs each; the table reports the median
hook sample and its paired total. No unrelated engine work fills the gap.

| RGBA8 size | Record before, ms | Record after, ms | Record+submit before, ms | Record+submit after, ms |
| --- | ---: | ---: | ---: | ---: |
| 128x128 | 0.040 | 0.008 | 0.041 | 0.032 |
| 256x256 | 0.057 | 0.011 | 0.060 | 0.069 |
| 1024x1024 | 0.666 | 0.083 | 0.715 | 0.828 |

Recording is cheaper, but isolated 256/1024 images immediately submitted cost
more in total: ownership needs a compressed-byte copy and dispatch/join. This
does not establish an isolated-image throughput win.

Bursts compare the same current hook with a test-only synchronous control
(serial decode below 256x256, caller plus pool above it) against queued work.
Both modes retain snapshot ownership and hashing. Five warmed bursts follow
one discarded warm-up. The caller records every region before submitting;
the eight-command case distributes regions round-robin. Hook time is elapsed
time inside the calling thread's recording loop, not aggregate worker CPU.

| Burst / command buffers | Sync hook, ms | Async hook, ms | Sync all-done, ms | Async all-done, ms |
| --- | ---: | ---: | ---: | ---: |
| 64 x 128x128 / 1 | 2.792 | 0.167 | 2.835 | 0.520 |
| 64 x 128x128 / 8 | 2.831 | 0.150 | 2.875 | 0.705 |
| 16 x 512x512 / 1 | 3.645 | 0.213 | 3.817 | 1.991 |
| 16 x 512x512 / 8 | 3.459 | 0.204 | 3.636 | 2.114 |

The small-job burst supports removing the recording threshold. An isolated
64x64 CPU control was slower queued (0.017 versus 0.011 ms), but no synchronous
production exception is kept: this is not evidence that bursts of small jobs
should block the caller. These synthetic bursts reuse one deterministic random
source per size, not a captured texture workload. **INFERRED:** overlapping
decode with engine recording may reduce upload stalls. In-game throughput,
pacing and target NVIDIA behaviour remain unverified.

## EAC palettes: a second VM measurement

Signed EAC R11/RG11 precompute eight final values per block, including the
multiplier-zero rule and signed -128 base fixup. Selectors index little-endian
palettes directly into four output rows. Alpha blocks also clamp eight palette
entries instead of sixteen pixels. Unsigned R11/RG11 instead dispatch at
`decode_region` to the complete old implementation: `etc_decode_legacy.rs` is
pinned verbatim, apart from its provenance comment and trailing blank line,
to main commit `c5a9e3727b635c29bee8e16584f7e006c533249c`, without its tests.
Keeping the whole region path avoids relying on arithmetic-only restoration
having the same optimisation behaviour. Other formats in that frozen module
are retained to preserve the implementation, but are not dispatched to it.

The original separate-run unsigned measurements (1.110 ms R11 and 1.832 ms
RG11) were not enough to distinguish VM variation from a regression. A
same-binary, three-round interleaved comparison resolved that: the old region
path took 1.038/1.042/1.041 ms for R11 against 1.300/1.295/1.295 ms with only
unsigned per-pixel arithmetic restored. RG11 was 1.683/1.693/1.661 ms against
2.177/2.199/2.158 ms. The regression was real in that session, not merely a
reason to dismiss the earlier baseline as noise. The full-region dispatch
replaces that attempt; signed palettes remain enabled.

The following replacement table is **same-session CPU microbenchmarking on
this VM, not in-game results**, with the same container/toolchain as above.
Each entry is the median of three round medians. Every round has seven batches
of ten old/new image pairs, alternating AB/BA order per pair, after warm-up.
Both timed paths use the same source and destination allocations. Deterministic
random inputs match the existing image benchmark generator. Outputs are checked
against the pinned decoder before and after timing.

| Random 1024x1024 EAC image, ms | Pinned old region | Selected region |
| --- | ---: | ---: |
| R11 unsigned | 1.032 | 1.032 |
| R11 signed | 5.529 | 2.023 |
| RG11 unsigned | 1.697 | 1.684 |
| RG11 signed | 10.718 | 3.627 |

The unsigned paths now tie within run variation, rather than carrying a
measured regression. Full per-round numbers, old/selected in milliseconds:

| Format | Round 1 | Round 2 | Round 3 |
| --- | ---: | ---: | ---: |
| R11 unsigned | 1.032 / 1.034 | 1.031 / 1.031 | 1.034 / 1.032 |
| R11 signed | 5.529 / 2.026 | 5.546 / 2.017 | 5.519 / 2.023 |
| RG11 unsigned | 1.648 / 1.662 | 1.711 / 1.684 | 1.697 / 1.701 |
| RG11 signed | 10.718 / 3.627 | 10.645 / 3.593 | 12.423 / 3.933 |

Reproduce the paired comparison without an engine or Vulkan driver:

```sh
CARGO_PROFILE_RELEASE_DEBUG=0 cargo test --release -p vanta-runtime --lib microbench_eac_interleaved -- --ignored --nocapture --test-threads=1
```

The old per-pixel R11 reference remains in tests: 65,536 base/table/multiplier
combinations with random selectors and second channels compare signed and
unsigned R11/RG11 output. Independent vectors, padded/cropped regions and
repeated-edge cases still run; an additional test compares unsigned dispatch
with the pinned path for empty, edge and malformed source/destination cases.
**INFERRED:** signed EAC uploads may use less CPU in game. No game was run.

## GPU compute shader decoder and staging pooling

Decoding ETC2/EAC on the CPU required taking a snapshot of compressed source
bytes, running worker threads against host-visible staging memory, hashing
every source byte at submit time, and blocking `vkQueueSubmit` until every
decoded band finished. In addition, the CPU repeated-block memoisation copied
back from the destination staging buffer (`dst.copy_within`), which turns into
uncached PCIe/Write-Combined reads when the driver returns non-cached staging
memory, and each command-buffer reset destroyed and reallocated its 8 MiB
staging buffers.

Three changes address this:

1. **Vulkan 1.0 compute shader path (`etc_decode_gpu.comp`):** decodes all
   seven `EtcFormat`s (`Rgb8`, `Rgb8A1`, `Rgba8`, `R11`, `R11Signed`, `Rg11`,
   `Rg11Signed`) on the GPU using `local_size_x = 8, local_size_y = 8,
   local_size_z = 1` and zero Vulkan extensions. `vkCreateBuffer` adds
   `VK_BUFFER_USAGE_STORAGE_BUFFER_BIT` to `TRANSFER_SRC` buffers (falling
   back to the unmodified flags if the driver refuses). `vkCreateCommandPool`
   records queue-family compute capability, and `vkCmdBindPipeline` tracks
   whether the command buffer has bound a compute pipeline so the shim never
   clobbers engine compute state. `emulate_copy_gpu()` records compute
   dispatch -> `VkBufferMemoryBarrier` -> `vkCmdCopyBufferToImage` with zero
   CPU decode, zero snapshot copy, zero submit-time hashing, and zero submit
   blocking. Counts into `ETC_REGION_GPU` under `VANTA_COUNT_GL=1`.
2. **Staging and descriptor pooling + lock split:** both GPU `DEVICE_LOCAL`
   scratch chunks and CPU host-visible staging chunks are returned to bounded
   per-device free lists on command-buffer reset/free instead of calling
   `vkDestroyBuffer`/`vkFreeMemory` every frame. Descriptor pools are likewise
   recycled with `vkResetDescriptorPool`. Host mappings live in their own
   `RwLock` separate from `State` (`RwLock`), and `LIVE_CPU_CMDS` lets
   `vkQueueSubmit*`, `vkCmdExecuteCommands`, and `retire_mapping` return
   immediately without locking when no CPU-decoded command buffers are active.
3. **CPU fallback Write-Combined readback fix:** both `etc_decode.rs` and
   `etc_decode_legacy.rs` cache the previous decoded 4x4 block in a 64-byte
   stack buffer instead of reading back from `dst`, and CPU staging allocation
   prefers `MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT | MEMORY_HOST_CACHED`.

### Hardware Vulkan compute verification and microbenchmarks

`gpu_compute_decoder_matches_cpu_bit_for_bit_across_all_formats` runs the real
SPIR-V compute pipeline against the CPU decoder across all seven formats,
multiple-of-4 and cropped odd widths (including odd-width `R11`/`R11Signed`
row-word padding), padded `row_blocks`, non-zero source byte offsets, and
multi-layer arrays. It passed bit-exact on an NVIDIA GeForce GTX 1080 Ti
(driver `580.178.04`, Intel Core i9-11900, 16 threads, release build with
`CARGO_PROFILE_RELEASE_DEBUG=0`).

Isolated 1024x1024 random-block microbenchmark on the same GTX 1080 Ti (median
of five batches after warm-up; GPU timing includes descriptor update, command
buffer reset/record, `vkQueueSubmit`, and `vkQueueWaitIdle` per image):

| 1024x1024 format | CPU serial, ms | GPU compute (incl. submit+waitIdle), ms | Speedup |
| --- | ---: | ---: | ---: |
| RGB8 | 2.699 | 0.136 | 19.9x |
| RGBA8 | 3.402 | 0.272 | 12.5x |
| RG11 signed | 3.900 | 0.158 | 24.7x |

Reproduce without loading an engine:

```sh
CARGO_PROFILE_RELEASE_DEBUG=0 cargo test --release -p vanta-runtime --lib microbench_gpu_vs_cpu_decode -- --ignored --nocapture --test-threads=1
```

**INFERRED:** offloading ETC2/EAC decoding to GPU compute and pooling staging
buffers should substantially reduce texture-streaming CPU spikes and submit
stalls on GPUs without native ETC2 (such as NVIDIA). No game was run; in-game
frame rate and frame-time percentiles remain unmeasured.

## Not an ADR-001 matter

Nothing here reads or writes the engine's memory or code. It is the same kind
of thing ADR-042 describes: the engine calls a function pointer Cordial handed
it, and Cordial answers at that boundary. What changed is that the answer is no
longer a pass-through, which ADR-042 said it would not be until the measurement
justified it.

## Provenance of the decoder

`etc_decode.rs` implements the ETC2 and EAC block formats as written in the
Khronos Data Format Specification (the "ETC2 Compressed Texture Image Formats"
section). It adds no third-party decoder to the dependency tree. The
intensity, distance and EAC modifier tables were read against the
specification by the reviewer from memory and not diffed against the
document. The test vectors in its `VECTORS` table, one per block mode, carry an
expected output that the test describes as produced by an independent decoder;
which decoder that was and how the vectors were generated has not been recorded
in the pull request, and is for the author to add here. Until it is, "matches
the specification" rests on the tables and the vectors agreeing with each
other, and has not been checked against real hardware output. Intel with
`CORDIAL_FORCE_ETC_EMULATION=1` against native decoding is the first such
check available without NVIDIA.

## Not yet established

- That real NVIDIA output looks right: a before and after of the plastic
  detail texture on the RTX 5070, and `CORDIAL_COUNT_GL=1` after joining a
  game with `ETC_UNHANDLED` in it, are the author's to supply.
- Staging memory for a real join. Each command buffer that uploads ETC data
  holds at least one 8 MiB staging chunk until it is reset or freed; the peak
  across a join has not been measured.
- Compatibility with a driver that lacks ETC2 and is not NVIDIA's. The code
  does not depend on the vendor, and no such driver has run it.
- In-game effects of the CPU fast path, real texture repetition rates, cold
  pool startup cost and contention/oversubscription against engine workers.
  The VM microbenchmarks above do not establish any of these.

# VR environment switches

Every `VANTA_*` variable the VR mode added, one line each. All default off
or to the measured choice; most exist as the control for one measurement in
[`dynarmic-design.md`](dynarmic-design.md) or [`play-button.md`](play-button.md),
and the section that used each is named where it helps. Nobody playing needs any
of them. `vanta-run --help` lists the translator's own beside the rest.

## Choosing what runs

| Variable | What it does |
|---|---|
| `XR_RUNTIME_JSON` | Not Vanta's: the OpenXR loader's own. The launcher sets it for a VR launch from Settings → VR → Runtime; set it by hand to pick a runtime for `vanta-run`. |
| `VANTA_VR_DEVICE` | `vanta-run --guest-arm64` sets it to 1 so `InitParams.isVrDevice` is true, which makes the menu load the Quest's own `Maquettes.rbxl` rather than `Mobile.rbxl`; 0 is the control. |
| `VANTA_DEVICE_PROFILE` | Not new, but `--guest-arm64` defaults it to `meta-quest`; `pc-windows-11` is the control that left `VREnabled` false. |
| `VANTA_NO_OPENXR=1` | No host OpenXR loader: every OpenXR call answers as on a machine without one. |
| `VANTA_NO_XR_MIRROR=1` | No left-eye mirror in the desktop window. |
| `VANTA_NO_MAQUETTES_INIT=1` | Does not call `initMaquettesSDK` after `StartApp` under `--app-bridge`. |
| `VANTA_NO_GAME_LAUNCH=1` | Does not answer `Game.launch`, so a Play press or deep link does not join (play-button.md). |
| `VANTA_NO_GAME_LIFECYCLE=1` | Does not play `ExperienceSession`'s start and end around a game. |
| `VANTA_NO_APP_RESTART=1` | Does not restart the Lua app after a Java-side leave; the menu then stays frozen. |

## Controls for fixes below the engine

| Variable | What it does |
|---|---|
| `VANTA_NO_VK_LIVE=1` | Forwards a second destroy of an image view or swapchain to the driver; NVIDIA faults on it during a rejoin. |
| `VANTA_NO_XR_PIN=1` | Lets the OpenXR loader unload the runtime with its instance; the rejoin then calls into unmapped memory. |
| `VANTA_XR_SWAPCHAIN_AS_ASKED=1` | Creates the eye swapchain in the engine's UNORM format instead of its sRGB twin (design §9.7). |
| `VANTA_XR_HAPTIC_COALESCE=0` | Forwards every haptic call, including repeats of a vibration still playing. |
| `VANTA_XR_HAPTIC_HOLD_MS=<n>` | Answers identical haptic repeats within `n` ms in the bridge; a diagnostic, off by default. |

## The translator

| Variable | What it does |
|---|---|
| `VANTA_GUEST_MONITOR=global` | dynarmic's exact exclusive monitor instead of inline compare-and-swap (design §9.3). |
| `VANTA_GUEST_CODE_CACHE_MIB=<n>` | Each Jit's code cache size. |
| `VANTA_GUEST_UNSAFE_FP=nan,recip,fma` | dynarmic's inexact floating-point optimisations; measured no gain (§9.8). |
| `VANTA_GUEST_TLS_KEYS=host` | `pthread_getspecific`/`setspecific` through host stubs rather than guest code (§9.8). |
| `VANTA_GUEST_STRING=host` | Short `memcpy`/`memmove`/`memset`/`memcmp` through host stubs rather than guest code (§9.9). |
| `VANTA_GUEST_CLOCK=host` | `clock_gettime(CLOCK_MONOTONIC)` through a host stub rather than the guest counter (§9.9). |
| `VANTA_GUEST_OMIT=<a,b>` | Leaves those imports unanswered. |
| `VANTA_GUEST_THREADS=0` | Every guest `pthread_create` fails with `EAGAIN`. |

## Logs

| Variable | What it does |
|---|---|
| `VANTA_XR_FRAME_LOG=<file>` | One line per `xrEndFrame`: time since the first, and the predicted display period, in ns. The frame-rate instrument behind §9.5--9.9. |
| `VANTA_XR_INPUT_LOG=1` | Action bindings, state and pose changes, events, the interaction profile and each haptic call. |
| `VANTA_GUEST_PROFILE=1` | Host-side time of guest-to-host calls. |
| `VANTA_GUEST_JNI_LOG=1` | Each Java method the guest engine calls. |
| `VANTA_GUEST_SVC_LOG=1` | Every raw `svc #0` the engine makes; the first of each number is logged anyway (§9.6). |
| `VANTA_GUEST_TRACE_EXEC=1` | Every call that makes guest memory executable or touches executable memory (§9.10). |
| `VANTA_GUEST_TRACE_THREADS=1` | A line per guest thread created. |
| `VANTA_GUEST_TRACE_SVC=1` | With `--jni-onload`, every stub call from `JNI_OnLoad` on. |
| `VANTA_TRACE_DLSYM=1` | Not new; now also covers the guest's `dlopen`/`dlsym`. |
| `VANTA_TRACE_AUDIO=1` | Not new; now also counts non-silent frames and the slowest callback of the guest's AAudio streams. |
| `VANTA_TIME_CTORS=1` | Times `libroblox.so`'s constructors apart from linking, on the native path; needs `patches/0003`. |

## Tests and tools

| Variable | What it does |
|---|---|
| `VANTA_TEST_QUEST_APK=<file>` | A Quest APK for `vanta-update`'s import tests, which skip without it. |
| `VANTA_VK_INCLUDE=<dir>` | The Vulkan headers the layout-gate tests compile against; `/usr/include` by default. |
| `VANTA_GUEST_LLSC_THREADS`, `VANTA_GUEST_LLSC_ITERS` | Size of `vanta-guest`'s exclusive-load/store stress test. |
| `VANTA_M3_SCENARIO` | Set by `vanta-guest`'s M3 test for the child process it runs; not for setting by hand. |
| `VANTA_DYNARMIC_DIR` | A CMake variable `crates/vanta-guest/build.rs` passes, not an environment variable. |

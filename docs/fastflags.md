# Changing FastFlags

<!-- description: Set Roblox FastFlags in Vanta, see which layer wins, and find the flags and keys Vanta itself reads. -->
<!-- icon: flag -->

FastFlags are Roblox's internal tuning switches. Vanta lets you override any of them, per profile, from a file or from **Settings → FastFlags**.

## Set a flag

<!-- steps -->

### Open the editor

**Settings → FastFlags** is a text editor for your profile's `flags.json`. Paste a list, or type one, and press **Apply**.

### Write a flat object

```json
{
  "DFIntTaskSchedulerTargetFps": 144,
  "FIntTaskSchedulerAutoThreadLimit": 8,
  "FFlagDebugGraphicsDisableVulkan": false
}
```

Values can be booleans, numbers or strings. Roblox stores them all as strings and Vanta normalises JSON booleans (`true`/`false` → `"True"`/`"False"`) and whole floats (`144.0` → `"144"`) while leaving `FLog`/`DFLog` and `FString`/`DFString` values intact. The format is Bloxstrap's, so a list from a video or a Discord message pastes in as it is.

### Start Roblox again

The editor says it applies the next time you press Roblox. A `DF` flag can also change in a game that is already running; see [When a change takes effect](#when-a-change-takes-effect).

<!-- /steps -->

To edit the file yourself, it is one file per profile:

<!-- tabs -->

### Native install

```text
~/.local/share/vanta/profiles/<profile>/flags.json
```

### Flatpak

```text
~/.var/app/io.github.gpt4omni.Vanta/data/vanta/profiles/<profile>/flags.json
```

`INFERRED` from how Flatpak remaps `XDG_DATA_HOME`; not checked against an installed package. The editor prints the real path under the text box.

<!-- /tabs -->

`VANTA_FLAGS` points the client at another file. It makes one file serve every profile, so it is for experiments.

> [!WARNING]
> A name the engine does not know is accepted and ignored, silently. An invented flag looks exactly like a working one. If a flag seems to do nothing, check that it exists:
>
> ```bash
> strings ~/.cache/vanta/lib/x86_64/libroblox.so | grep -x DebugGraphicsDisableVulkan
> ```
>
> The file carries the `FFlag`/`FInt`/`FString` prefix; the engine's own table stores the name without one, which is why the `grep` drops it.

To choose a graphics backend, use **Settings → General → Graphics**, not a flag. Vanta decides that before the engine starts.

## When a change takes effect

| Flag family | When it is read |
|---|---|
| `FFlag`, `FInt`, `FString` | Once, at startup. Relaunch to change them. |
| `DFFlag`, `DFInt`, `DFString` | At startup, and again while the game runs: Roblox re-fetches its settings about every two minutes and Vanta hands the engine your overrides again straight after. |

A plugin loaded part-way through a session cannot change a startup flag, whatever it writes.

Vanta puts your overrides back after each Roblox refresh, whatever the family — watching both `[FLog::DynamicFastVariableReloader]` and `[DFLog::FlagCache]` lines, plus a one-shot 5-second post-startup settle re-apply (cancelled if a refresh line already ran) so `DF*` overrides survive Roblox's initial 2–4 second fetch too. Periodic post-refresh re-applies send a minimal delta containing only your overridden flags rather than re-sending the entire cached settings document, and live edits can trigger an immediate reload via the `reload_flags` live control action. Each re-apply prints a `[reapply]` line with what it cost.

`VANTA_NO_FLAG_REDELIVERY=1` in the client's environment turns the re-apply off. It exists to confirm that a flag reverts without it.

Why: [ADR-051](adr/ADR-051-overrides-are-reapplied-after-the-engines-refresh.md).

## Render distance with `DFIntDebugFRMQualityLevelOverride`

Roblox's Android client settings document ships `"FFlagFRMLockstepOverridesEffective": "True"`. When `DFIntDebugFRMQualityLevelOverride` is set to a quality level in `1..=21`, `FRM::getDistanceSquared()` ignores the render-distance slider and locks view distance to the quality level's table entry (`500` studs at QL `1`).

To prevent quality overrides from collapsing render distance, whenever your resolved flags set `DFIntDebugFRMQualityLevelOverride` to an integer in `1..=21` and nothing explicitly sets `FFlagFRMLockstepOverridesEffective`, Vanta automatically injects `"FFlagFRMLockstepOverridesEffective": "False"` and logs a note. Setting `"FFlagFRMLockstepOverridesEffective": "True"` explicitly in your `flags.json` still wins over the automatic injection.

## Which layer wins

Flags come from several sources and each owns its own file. From lowest precedence to highest:

| Layer | Source | Notes |
|---|---|---|
| Roblox's settings | The client-settings document from Roblox | The base everything else is merged into. |
| Vanta's default | Built in | `FFlagUserLaunchedWithBloxstrap`, plus automatic `FFlagFRMLockstepOverridesEffective=False` when `DFIntDebugFRMQualityLevelOverride` is in `1..=21`. |
| Graphics optimisation | **Settings → General → Graphics** (`VANTA_PERFORMANCE`) | `balanced` (default), `throughput`, `latency`, and `speed`. |
| Plugins | `<plugin>/flags.json`, alphabetical | A plugin switched off contributes nothing. |
| Frame rate limit | **Settings → General → Graphics** | Sets `DFIntTaskSchedulerTargetFps`. Beats a plugin that sets the same flag. |
| Your flags | `<profile>/flags.json` | Always wins. |

A plugin never writes to your file. Removing a plugin removes its flags, and when two layers set the same flag the log names both:

```text
flags: FIntTaskSchedulerAutoThreadLimit = 8 from user
       (overrides plugin:fps-tweaks=4, plugin:net-tuner=16)
```

Two plugins that disagree are both named. The later one wins so the outcome is deterministic.

Your flags live in the profile, so a flag you set while testing on one account is not still set on the account you play. A file left at the old `~/.config/vanta/flags.json` is moved into the first profile that looks for one ([ADR-013](adr/ADR-013-per-profile-configuration.md)). The layering itself: [ADR-005](adr/ADR-005-flag-service.md).

## Flags and keys Vanta reads

Besides Roblox's own flags, these names mean something to Vanta. The `Vanta` ones are never sent to Roblox.

| Name | Values | What it does |
|---|---|---|
| `FFlagUserLaunchedWithBloxstrap` | `True` (default) | Tells games the launcher speaks BloxstrapRPC, which Discord presence needs. Set `"False"` to retract the claim; games can then tell they are not under a launcher that implements it. It is not an engine flag. That a game then answers yes is `INFERRED`. |
| `DFIntTaskSchedulerTargetFps` | whole number | The engine's own frame target. **Settings → General → Graphics → Frame rate limit** sets it for you. |
| `VantaFrameRateLimit` | `display`, or a number of fps | The Frame rate limit row, as a key a plugin can set. The row beats a plugin; your `flags.json` beats the row. |
| `VantaPresentMode` | `off`, `auto`, `mailbox`, `immediate`, `uncapped`, `fifo`, `fifo-relaxed` | Vulkan present mode. The **Frame pacing** row sets it. A mode the driver does not offer leaves the engine's own choice. |
| `VantaGraphicsBackend` | `automatic`, `vulkan`, `gles` | Which graphics backend to offer the engine. The **Graphics** row beats a plugin's request. |
| `VantaDeviceProfile` | `pc-windows-11`, `roblox-app`, `android-tablet`, `meta-quest` | Which device Vanta says it is. **Has no effect from `flags.json` today**; the Graphics optimisation row sets it, through `VANTA_DEVICE_PROFILE`. `INFERRED` from reading the code: nothing outside the flag module calls the reader. |

Graphics optimisation (`VANTA_PERFORMANCE`) sets these flags, sized to your physical core count (`workers`):

<details>
<summary>What Balanced, Throughput, Latency, and Speed set</summary>

**Balanced** (`VANTA_PERFORMANCE=balanced`, default) — moderate desktop multi-core scaling and Vulkan pooling:

| Flag | Value |
|---|---|
| `FIntTaskSchedulerThreadMin` | `0` |
| `FIntTaskSchedulerAsyncTasksMinimumThreadCount` | `min(workers, 2)` |
| `FIntTaskSchedulerAutoThreadLimit` | `min(workers, 8)` |
| `FIntSmoothClusterTaskQueueMaxParallelTasks` | `ceil(workers / 2)` |
| `FIntSmoothClusterTaskQueueMsPerCycle` | `4` |
| `FIntOcclusionWorkerThreadCount` | `ceil(workers / 4)` |
| `FFlagMovePrerenderV2` | `True` |
| `DFIntSimMidPhaseContactPipelineBatchSize` | `64` |
| `FFlagGraphicsVulkanExtendedDynamicState` | `True` |
| `FFlagVulkanAllocatorPoolSmallOnly` | `False` |
| `FIntVulkanAllocatorKeepEmptyPages` | `8` |

**Throughput** (`VANTA_PERFORMANCE=throughput`) — full physical-core scaling for desktops with cores to spare:

| Flag | Value |
|---|---|
| `FIntTaskSchedulerThreadMin` | `0` |
| `FIntTaskSchedulerAsyncTasksMinimumThreadCount` | `min(workers, 3)` |
| `FIntTaskSchedulerAutoThreadLimit` | `workers` |
| `FIntSmoothClusterTaskQueueMaxParallelTasks` | `workers` |
| `FIntSmoothClusterTaskQueueMsPerCycle` | `8` |
| `FIntOcclusionWorkerThreadCount` | `ceil(workers / 2)` |
| `FFlagMovePrerenderV2` | `True` |
| `FFlagAnimatorParallelManagerUseLiveTaskSchedulerConcurrency` | `True` |
| `FFlagHumanoidParallelManagerUseLiveTaskSchedulerConcurrency` | `True` |
| `DFIntSimMidPhaseContactPipelineBatchSize` | `128` |
| `FFlagGraphicsVulkanExtendedDynamicState` | `True` |
| `FFlagVulkanAllocatorPoolSmallOnly` | `False` |
| `FIntVulkanAllocatorKeepEmptyPages` | `12` |

**Latency** (`VANTA_PERFORMANCE=latency`) — lower thread counts and a smaller physics batch to reduce contention and jitter:

| Flag | Value |
|---|---|
| `FIntTaskSchedulerThreadMin` | `0` |
| `FIntTaskSchedulerAsyncTasksMinimumThreadCount` | `1` |
| `FIntTaskSchedulerAutoThreadLimit` | `clamp(workers, 1, 4)` |
| `FIntSmoothClusterTaskQueueMaxParallelTasks` | `1` |
| `FIntSmoothClusterTaskQueueMsPerCycle` | `2` |
| `FIntOcclusionWorkerThreadCount` | `1` |
| `FFlagMovePrerenderV2` | `True` |
| `DFIntSimMidPhaseContactPipelineBatchSize` | `32` |
| `FFlagGraphicsVulkanExtendedDynamicState` | `True` |
| `FFlagVulkanAllocatorPoolSmallOnly` | `False` |
| `FIntVulkanAllocatorKeepEmptyPages` | `4` |

**Speed** (`VANTA_PERFORMANCE=speed`) — `libroblox.so`-verified weak/older-CPU optimisation bundle. Also defaults looper zero-timeout poll coalescing (`VANTA_POLL_COALESCE_US`) to `500` µs (instead of `250` µs) when unset to save busy-poll CPU cycles on 2-core CPUs. All flags sit in the `performance mode` layer so overrides in your `flags.json` still win:

- **CPU / TaskScheduler:** `FIntTaskSchedulerThreadMin=0`, `FIntTaskSchedulerAutoThreadLimit=clamp(workers, 1, 4)`, `FIntTaskSchedulerAsyncTasksMinimumThreadCount=1`, `FIntSmoothClusterTaskQueueMaxParallelTasks=1`, `FIntSmoothClusterTaskQueueMsPerCycle=1`, `FIntOcclusionWorkerThreadCount=1`, `DFIntDebugVoxelJobManagerMaxThreads=1`, `DFIntVoxelJobManagerDMJobTimeMicroSeconds=1000`, `DFIntVoxelJobManagerRenderThreadJobTimeMicroSeconds=1000`, `DFIntSimMidPhaseContactPipelineBatchSize=32`, `FFlagMovePrerenderV2=True`, `FFlagAnimatorParallelManagerUseLiveTaskSchedulerConcurrency=True`, `FFlagHumanoidParallelManagerUseLiveTaskSchedulerConcurrency=True`.
- **FRM Quality Lock (with render distance preserved) & Post-FX / Shadows / Lighting:** `DFIntDebugFRMQualityLevelOverride=1`, `FFlagFRMLockstepOverridesEffective=False`, `FIntDebugFRMOptionalMSAALevelOverride=0`, `FIntBloomFrmCutoff=22`, `FFlagFRMDisableCloudsAtLowQL=True`, `DFFlagRenderConstantFogAtLowQL=True`, `DFIntRenderConstantFogAtLowQLQLCutoff=21`, `DFIntRenderPostFxBasePixelCount=480000`, `FFlagRenderShadowResolutionScaling=True`, `FStringRenderShadowResolutionQualityByRam=1024,3,2048,3,4096,3`, `FFlagRenderAllocateShadowMapResourcesOnDemand=True`, `FIntTM2ShadowMapMaxMips=6`, `FIntTM2EnvMapMaxMips=6`, `FIntEnvMapRaycastCount=4`, `FIntEnvMapMaxRayLength=64`, `FFlagRenderWaterDemoteSHQ=True`.
- **Textures / Grass / Terrain / Particles / Audio / Vulkan:** `DFFlagDebugForceAnisoOff=True`, `DFIntRenderClampRoughnessMax=1`, `FIntDebugTextureManagerSkipMips=1`, `DFIntDynamicTextureManagerPrepareBudgetMicroSeconds=500`, `FIntFRMMinGrassDistance=0`, `FIntFRMMaxGrassDistance=0`, `FIntRenderGrassDensityMax=0`, `FIntRenderGrassDensityMin=0`, `DFFlagFRMUseParticleCountBudget=True`, `DFFlagFRMUseGlobalParticleCountBudget=True`, `DFIntFRMParticleBudgetMinLerp=64`, `DFIntFRMGlobalParticleBudgetMinLerp=256`, `FFlagRenderParticlesCapNonVisibleEmission=True`, `FFlagRenderParticlesNonVisibleSimBudget=True`, `DFIntRenderParticlesNonVisibleSimBudgetMicros=100`, `DFIntParticlesFramesBetweenNonVisibleUpdates=12`, `DFIntTerrainLodActive=4`, `DFIntTerrainLodLevels=2`, `DFIntTerrainPow2LodBiasPercent=50`, `FFlagAudioEngineThrottleSoundServiceStep=True`, `FIntAudioEngineAudibilityDepthLimit=6`, `FFlagGraphicsVulkanExtendedDynamicState=True`, `FFlagVulkanAllocatorPoolSmallOnly=False`, `FIntVulkanAllocatorKeepEmptyPages=4`.

</details>

## Raise the frame rate

The Android client has no frame-rate row in its own menu, so there are two separate levers in Vanta's Settings.

| What you want | Where |
|---|---|
| Stop drawing being pinned to your display's refresh | **Settings → General → Graphics → Frame pacing**. Mailbox is the default. The FPS Flex plugin pulls the same lever, so use one or the other. |
| Raise the engine's own target frame rate and keep it there | **Settings → General → Graphics → Frame rate limit** |

Neither replaces the other. Frame pacing on FIFO caps you at your panel's rate whatever the limit says. Frame pacing applies at the next launch.

**Frame rate limit** offers Display refresh, or 90, 120, 144, 165, 240, 360 or 480. Display refresh sets the engine's target to one and a half times your monitor's rate when that is above 60 Hz and the present mode is MAILBOX or IMMEDIATE (the default), to the rate itself under FIFO, and to nothing on a 60 Hz monitor. The extra half is because the engine's limiter lands short of its target, and under MAILBOX a frame finished early costs nothing on screen; see [present-headroom.md](analysis/present-headroom.md). A new cap takes effect in a running game at once. Going back to Display refresh from a cap takes up to two minutes on a 60 Hz monitor, because the engine does not unset a flag that Roblox's settings leave out.

For a number the row does not offer, set `DFIntTaskSchedulerTargetFps` in the FastFlags editor; your own flags beat the row. Whether the engine honours a target above 240 has not been measured here: 360 and 480 are `INFERRED` to work until someone with a monitor that fast says otherwise.

> [!WARNING]
> Do not pick a cap above your display's refresh. On a 60 Hz output a cap of 90 presented about 31 frames a second and 240 about 36, against 57 to 60 with nothing set. That is one headless environment, and on a fast monitor the opposite holds: on a 240 Hz output with an NVIDIA GPU, `DFIntTaskSchedulerTargetFps=240` presented 216-232 frames a second against a flat 59.9 with the engine's own target (measured 2026-10-03). A contributor reported that the engine stopped at 240 when asked for 1000; that was not reproduced, and 360 and 480 are offered so it can be checked.

## Import a list from another launcher

**Settings → FastFlags → Import…** reads a Bloxstrap or Fishstrap `ClientAppSettings.json`, or Sober's `config.json` (only its `fflags` object), and merges the flags into this profile's. From a terminal:

```bash
vanta --import-flags ClientAppSettings.json     # or - for standard input
vanta --import-flags --sober                    # finds Sober's own config
vanta --import-flags list.json --profile NAME --replace
```

Flags already set are kept unless the list sets them again. `--replace` starts from empty instead. Values are checked by prefix: `FFlag` takes `True` or `False`, `FInt` a whole number, and `FLog` anything, because a log channel takes either a number or a severity and Vanta cannot tell which one a channel wants.

An entry the check refuses is skipped and named, and the rest are imported. A name with no FastFlag prefix is imported and listed, so a typo shows. The Sober path outside the Flatpak (`~/.config/sober/config.json`) is `INFERRED`; only the Flatpak's has been seen.

## Log channels: use a severity name

A `FLog` or `DFLog` channel takes a bare number (`"6"`) or a severity name (`"Verbose"`, `"Warning,6"`), and a number given to a channel that wants a name does not fail: the channel goes silent, so an empty log is not evidence the channel is quiet. **When you do not know which a channel takes, write `"Verbose"`.** Across 177 channels the engine logged under it in a signed-out launch of 2.738.0.1397, every one kept logging with it, and 66 of them (`FLogNativeDM` among them) went quiet when given a bare number.

Look a channel up in [`docs/analysis/flog-channel-shapes.tsv`](https://github.com/gpt4omni/vanta/blob/main/docs/analysis/flog-channel-shapes.tsv): `either` kept logging under both, `severity-only` went quiet under a number, and `unscored` is the 644 the landing page never reached, which tells you nothing. When an override in your `flags.json` gives a number to a channel known to need a name, the client log says so at launch (`flags: FLogNativeDM="100" is a bare number, ...`). Channels with no evidence are not warned about.

`python3 tools/flog_channel_shapes.py` prints the shape Roblox's own settings use for the channels they set (139 in the settings fetched on 2026-10-15). Five of them are given a number there and still went quiet under one, so that document is a hint and not the rule. To repeat the measurement on a new build, `tools/flog_channel_survey.py` documents the three launches it takes.

## If the interface looks coarse

The interface is laid out for a low-density phone: render resolution is 720p and `dpiScale` is 1.0. For a direct `vanta-run` launch, raise both:

```bash
VANTA_MONITOR=1 VANTA_RESOLUTION=1920x1200 VANTA_DPI_SCALE=1.75 \
vanta-run --lib-dir /path/to/lib/x86_64 --apk /path/to/base.apk \
  --host-libc --game-activity --run 30
```

Roblox's graphics-quality flags (`DebugFRMQualityLevelOverride` and the MSAA overrides) change nothing here, because they govern 3D scenes and the signed-out landing page is a 2D interface.

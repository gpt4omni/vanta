# Scheduler target headroom under MAILBOX

**What changed.** With Frame rate limit on Display refresh, a display above
60 Hz and a present mode that replaces frames (MAILBOX, the default, or
IMMEDIATE), `DFIntTaskSchedulerTargetFps` is now 1.5 times the refresh rate
instead of the rate itself: 150 on a 100 Hz output. FIFO, `fifo-relaxed` and
`VANTA_PRESENT_MODE=off` keep the old target, and a number chosen by hand is
never touched.

**Why.** Measured on main on a 100 Hz output (GTX 1080 Ti, NVIDIA 580,
Hyprland, Vulkan): 75-84 presents a second in game at a target of 100, p95
interval about 20 ms. Nothing in Vanta's present path waits: the
`vkQueuePresentKHR` wrapper counts, records the interval and forwards, and
neither queue submission nor fences are interposed. So the shortfall is on the
engine's side of the present, and the obvious candidate is its limiter landing
short of its own target. Under MAILBOX a frame that finishes after one refresh
and before the next is simply shown at the next; a refresh that finds no new
frame repeats the old one, and at 100 Hz that repeat is exactly a 20 ms
interval. A target above the refresh means there is nearly always a new frame
waiting, and the surplus is discarded before scan-out, so it costs GPU and
power but not latency. Under FIFO the same surplus would queue and add
latency, which is why FIFO is left alone.

**Model** (`tools/bench/present_headroom.py`, pure arithmetic, `INFERRED`).
The engine's interval is the target's times an undershoot, with 15% lognormal
jitter; an undershoot of 1.25 reproduces the 80 a second measured at 100.

| Target | Presents/s | Distinct frames on screen/s | Refreshes repeating a frame |
|---|---|---|---|
| 100 (old) | 80 | 79 | 21% |
| 125 | 99 | 94 | 6% |
| 150 (new) | 119 | 99 | 1% |
| 200 | 159 | 100 | 0% |

With an undershoot of 1.4 the new target still shows 97 a second, 1.1 gives 100.

**What this does not fix, and how to tell.** If the engine is bound by its own
CPU work rather than by its limiter -- RBX Worker C at about 85% of a core is
consistent with either -- a higher target changes nothing, and that belongs to
the worker-thread work, not here. The test that separates the two, on the
machine that measured 75-84: the same scene with input driven continuously
(present counts without input are pinned by the idle throttle), Frame rate
limit on Display refresh against a hand-picked 100 as the control, comparing
presents a second and the p95 interval in the frame-pacing log line. Not run:
this change was made from code reading and the model only.

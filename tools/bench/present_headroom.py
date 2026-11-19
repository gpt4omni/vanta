#!/usr/bin/env python3
"""Model of the engine's frame limiter against a MAILBOX swapchain.

The engine finishes frames at its scheduler target times an undershoot (1.25
reproduces the 75-84 presents a second measured at a target of 100 on a
100 Hz output), with lognormal jitter. Each refresh scans out the newest frame
finished since the last; a refresh that finds none shows the old frame again,
which is a 20 ms interval at 100 Hz. Pure arithmetic: INFERRED, not measured.
See docs/analysis/present-headroom.md.
"""
import random
def run(target, hz=100.0, undershoot=1.25, jitter=0.15, secs=60, seed=1):
    r=random.Random(seed); vb=1000/hz; t=0.0; done=[]
    while t<secs*1000:
        t+= (1000/target)*undershoot*r.lognormvariate(0,jitter); done.append(t)
    shown=0; repeats=0; i=0; n=int(secs*1000/vb)
    for k in range(1,n+1):
        v=k*vb; new=False
        while i<len(done) and done[i]<=v: i+=1; new=True
        if new: shown+=1
        else: repeats+=1
    return len(done)/secs, shown/secs, repeats/n
for tgt in (100, 125, 150, 200):
    p,s,rep=run(tgt); print(f"target {tgt}: presents {p:.0f}/s, distinct frames on screen {s:.0f}/s, vblanks repeating a frame {rep*100:.0f}%")

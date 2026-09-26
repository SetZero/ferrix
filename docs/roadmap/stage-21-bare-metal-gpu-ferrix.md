# Stage 21 — Bare metal, and a GPU of Ferrix's own  ·  *unsized, over 100 points*

Ferrix on a machine rather than in one, with the NVIDIA card in it driving
the screen: the second half of the decision of 2026-09-18, opened when the
customer wants Ferrix on bare metal and not before. `docs/GPU.md` §4 says
what it takes and why it is not sized: NVIDIA's open kernel modules as a
ring-3 driver process behind an OS interface layer written for Ferrix, the
GSP firmware they load, and a userspace that today means glibc-built closed
libraries -- or Mesa's NVK and Linux's Rust driver Nova, weighed when the
stage opens. What stage 19's Path A leaves ready for it: the render node,
the renderer trait and the GPU renderer's shaders, and gates that judge a
GPU's picture; `zwp_linux_dmabuf` too, once it lands after Path A. It depends on the dynamic linking stage,
whichever userspace is taken.

**Exit:** the stage 19 exit on real hardware, drawn by the card.

---


# Stage 1 — Boot, both architectures ✅

UEFI loader in Rust: read the kernel from the ESP, parse ELF, build page
tables, take the memory map, `ExitBootServices`, switch to our own tables, jump
to the kernel's Rust entry point. Kernel writes to a serial port and shuts the
machine down.

Zero assembly at boot on any architecture — firmware calls `efi_main` with a
stack and the MMU on. The only assembly is the page-table switch itself.

**Exit, met:** `cargo xtask test-boot --arch all` boots firmware → loader →
kernel on each architecture, and the kernel verifies five things before it
reports success:

* the hand-off structure's magic, version and layout constants agree with what
  the kernel was built against;
* the memory map is sorted, non-overlapping, has usable RAM, and describes the
  loader's *own* allocations — each one looked up by its address and required
  to lie inside a region of its own kind: the kernel image, the root tables,
  the boot info and its array, the boot stack, the device tree copy and the
  initramfs. Without that the frame allocator would hand out the frames
  holding its own page tables, or the stack the kernel is running on, and a
  check that only asked whether a region of each kind existed somewhere would
  still pass;
* the kernel faults when it writes through a read-only mapping, which on
  x86-64 is `CR0.WP`: the loader sets it, because firmware need not have, and
  the W^X sweep reads entries rather than trying a write;
* the direct map really does alias physical memory, checked by reading the
  kernel's own first bytes through both mappings;
* the kernel can walk the loader's page tables and *extend* them, which on
  AArch64 is not optional — the console is an `MMIO` register that has to be
  mapped as device memory before a byte can go out.

Two bugs this stage found, both of which would have been very hard to diagnose
later: firmware reports a reserved aperture at 1 TiB that must not be counted as
RAM, and `FERRIX` as a directory name is byte-identical to `FERRIX` as a FAT
volume label.

---


//! The Ferrix kernel.
//!
//! Entered from the UEFI loader with the MMU on, three mappings in place and
//! nothing else: no interrupt vectors, no allocator, no other CPU running. What
//! stage 1 does with that is prove the hand-off is sound and say so over the
//! serial port, which is the smallest thing that can honestly be called booting.
//!
//! See `docs/ROADMAP.md` for what comes next.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod console;
mod early;
mod mm;
mod trap;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use ferrix_bootinfo::{BootInfo, BootView, KERNEL_VMAP_BASE, MemKind, PAGE_SIZE};

use console::println;
use early::EarlyMemory;

/// What the boot test waits for. Changing it means changing
/// `xtask/src/qemu.rs`, and the two are checked against each other there.
const SUCCESS_MARKER: &str = "FERRIX-BOOT-OK";

/// The kernel's entry point.
///
/// The loader calls this with the boot info pointer as its only argument; the
/// signature is [`ferrix_bootinfo::KernelEntry`], declared in the crate both
/// sides share so that a mismatch is a type error rather than a triple fault.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
extern "C" fn _start(boot_info: *const BootInfo) -> ! {
    // Nothing has been checked yet, including whether this pointer is a
    // `BootInfo` at all — so `validate` is the first thing that runs, and it
    // checks the magic before it follows anything.
    //
    // SAFETY: the loader passes a pointer to a `BootInfo` it built, inside the
    // direct map, immutable for the life of the system.
    let info = unsafe { &*boot_info };
    // SAFETY: the same structure, so its `regions` and `cmdline` point at the
    // arrays the loader wrote beside it, for as long as the system runs.
    let Ok(view) = (unsafe { info.validate() }) else {
        // No console yet, and no way to make one without a valid hand-off.
        arch::halt()
    };

    let mut memory = EarlyMemory::new(&view);
    if arch::init_console(&mut memory).is_err() {
        arch::halt()
    }
    // SAFETY: `init_console` configured the port and, on AArch64, mapped it.
    unsafe { console::mark_ready() };

    kmain(&view, &mut memory)
}

/// The kernel proper.
fn kmain(view: &BootView<'_>, memory: &mut EarlyMemory) -> ! {
    println!();
    println!("Ferrix {} on {}", env!("CARGO_PKG_VERSION"), arch::NAME);

    report(view);

    if let Err(problem) = self_check(view, memory) {
        println!("FERRIX-PANIC stage 1 self-check failed: {problem}");
        arch::halt()
    }
    println!("  stage 1  loader hand-off verified");

    // Before anything else, and before anything can fault: until this runs the
    // CPU is still pointing at firmware's handlers, which stopped existing at
    // `exit_boot_services`. A fault in that window is a jump into reclaimed
    // memory, which on x86-64 is a triple fault and a silent reset.
    //
    // SAFETY: called exactly once, on the boot CPU, with interrupts masked.
    unsafe { arch::init_traps() };
    println!("  traps    vectors installed");

    let stats = match mm::init(view) {
        Ok(stats) => stats,
        Err(problem) => {
            println!("FERRIX-PANIC could not bring up memory: {problem}");
            arch::halt()
        }
    };
    report_memory(&stats);

    if let Err(problem) = memory_check(&stats) {
        println!("FERRIX-PANIC stage 2 self-check failed: {problem}");
        arch::halt()
    }
    println!("  stage 2  frame allocator and heap verified");

    if let Err(problem) = trap_check() {
        println!("FERRIX-PANIC stage 3 self-check failed: {problem}");
        arch::halt()
    }
    println!(
        "  stage 3  {} breakpoints and {} page faults handled",
        trap::breakpoint_count(),
        trap::handled_fault_count()
    );

    println!("{SUCCESS_MARKER} stages 1-3");
    arch::shutdown()
}

/// Stage 3's exit criterion: the kernel can take a trap and carry on.
///
/// Two things, and the second is the one that matters. A breakpoint proves the
/// whole entry path works — vector, register save, dispatch, restore, return —
/// because execution continues on the next instruction with every register
/// intact. A page fault proves the kernel can *resolve* a fault and let the
/// faulting instruction retry, which is exactly what demand paging is, and is
/// how every anonymous mapping will work from stage 6.
fn trap_check() -> Result<(), &'static str> {
    check_breakpoint()?;
    check_demand_paging()
}

/// A breakpoint must return to the instruction after it, twice.
fn check_breakpoint() -> Result<(), &'static str> {
    let before = trap::breakpoint_count();

    // A canary in a register the trap frame saves and restores. If the entry
    // path drops a register, this is what notices.
    let canary: u64 = 0x0123_4567_89AB_CDEF;
    let mut witness = canary;

    arch::breakpoint();
    witness = witness.rotate_left(1);
    arch::breakpoint();

    if trap::breakpoint_count() != before + 2 {
        return Err("a breakpoint did not reach the handler");
    }
    if witness != canary.rotate_left(1) {
        return Err("a register did not survive the trap");
    }
    Ok(())
}

/// A fault in the on-demand window must be resolved by mapping a page.
fn check_demand_paging() -> Result<(), &'static str> {
    let before = trap::handled_fault_count();
    let free_before = mm::free_frames();

    // Three pages, touched out of order, so a handler that mapped a fixed
    // address rather than the faulting one would fail here.
    let probes = [
        mm::DEMAND_WINDOW + 0x2000,
        mm::DEMAND_WINDOW,
        mm::DEMAND_WINDOW + 0x1000,
    ];

    for (index, probe) in probes.iter().enumerate() {
        if mm::translate(*probe).is_some() {
            return Err("the on-demand window was already mapped");
        }

        let value = 0xFEED_0000_u64 + index as u64;
        // SAFETY: nothing is mapped here, which is the point: the write takes a
        // page fault, the handler maps a zeroed page, and the CPU retries the
        // instruction. `volatile` so the compiler cannot decide the write is
        // dead and remove the fault along with it.
        unsafe { core::ptr::write_volatile(*probe as *mut u64, value) };
        // SAFETY: the page is mapped now, by the fault the write above took.
        let read_back = unsafe { core::ptr::read_volatile(*probe as *const u64) };

        if read_back != value {
            return Err("memory faulted in did not hold what was written to it");
        }
        if mm::translate(*probe).is_none() {
            return Err("the fault handler did not leave a mapping behind");
        }
    }

    let handled = trap::handled_fault_count() - before;
    if handled != probes.len() as u64 {
        return Err("the number of faults handled does not match the pages touched");
    }

    // Each fault consumes a frame for the page itself, and the *first* one into
    // a fresh region also consumes frames for the page tables above it -- three
    // of them here, since nothing was mapped in this window at all. So the
    // total is bounded rather than exact.
    let consumed = free_before.saturating_sub(mm::free_frames());
    if consumed < probes.len() as u64 {
        return Err("faulting in pages consumed fewer frames than pages");
    }
    if consumed > probes.len() as u64 + 3 {
        return Err("faulting in pages consumed more frames than pages plus a table per level");
    }

    // What *is* exact: a fault into a region whose tables already exist costs
    // one frame and no more. Checking it separately is what makes the bound
    // above a measurement rather than a shrug.
    let settled = mm::free_frames();
    let neighbour = mm::DEMAND_WINDOW + 0x3000;
    // SAFETY: unmapped, so this faults; the handler maps a zeroed page and the
    // instruction retries.
    unsafe { core::ptr::write_volatile(neighbour as *mut u64, 1) };
    if mm::free_frames() != settled - 1 {
        return Err("a fault into an already-tabled region cost more than one frame");
    }

    // The rest of the page must read as zero: a page handed out still holding
    // the last owner's data is an information leak, and from stage 6 the last
    // owner is another process.
    // SAFETY: mapped by the faults above.
    let tail = unsafe { core::ptr::read_volatile((mm::DEMAND_WINDOW + 0x800) as *const u64) };
    if tail != 0 {
        return Err("a faulted-in page was not zeroed");
    }
    Ok(())
}

/// Print what the allocators came up with.
fn report_memory(stats: &mm::Stats) {
    println!(
        "  frames   {} MiB managed, {} MiB free, {} entries at {:#x} ({} KiB)",
        stats.managed_frames * 4 / 1024,
        stats.free_frames * 4 / 1024,
        stats.managed_frames,
        stats.page_array_at,
        stats.page_array_bytes / 1024,
    );
}

/// Stage 2's exit criterion.
///
/// Every one of these is an invariant a later subsystem will assume without
/// checking, because by then there will be no way to check it: a scheduler that
/// gets a `Vec` back with the wrong contents has no idea the heap is at fault.
fn memory_check(stats: &mm::Stats) -> Result<(), &'static str> {
    if stats.managed_frames == 0 {
        return Err("the frame allocator was given nothing");
    }

    check_frames()?;
    check_heap()?;

    if mm::heap_allocated() != 0 {
        return Err("the heap did not give everything back");
    }
    Ok(())
}

/// Hammer the frame allocator and require the books to balance.
///
/// **Deliberately allocates nothing on the heap.** `Vec` would be far more
/// convenient here and would also make the check meaningless: growing one takes
/// slab pages out of this very allocator, and `ferrix_heap` documents that slab
/// pages are never returned. The first version of this used a `Vec` and
/// reported a leak that was the heap working as designed.
fn frames_hammer() -> Result<(), &'static str> {
    /// Blocks held at once. Sixteen bytes each on a 64 KiB boot stack.
    const BATCH: usize = 256;
    /// How many times to fill and drain the batch.
    const ROUNDS: usize = 16;

    let before = mm::free_frames();

    for round in 0..ROUNDS {
        let mut taken = [(0u64, 0u8); BATCH];
        let mut held = 0usize;

        for (step, slot) in taken.iter_mut().enumerate() {
            // Orders 0..4, in a pattern that shifts each round so blocks do not
            // always pair up the same way.
            let order = ((step + round) % 5) as u8;
            let Some(frame) = mm::allocate_frames(order) else {
                break;
            };
            *slot = (frame, order);
            held += 1;
        }
        if held == 0 {
            return Err("the frame allocator handed out nothing");
        }

        // Free every other block first, then the rest. Buddies come apart and
        // then back together, which is the path that actually exercises
        // coalescing -- freeing in allocation order barely does.
        for (frame, order) in taken.iter().take(held).skip(1).step_by(2) {
            mm::deallocate_frames(*frame, *order);
        }
        for (frame, order) in taken.iter().take(held).step_by(2) {
            mm::deallocate_frames(*frame, *order);
        }
    }

    let after = mm::free_frames();
    if after != before {
        return Err("frames leaked: the free count did not return to where it started");
    }
    Ok(())
}

/// Check the frame allocator hands out distinct, aligned blocks.
fn check_frames() -> Result<(), &'static str> {
    let first = mm::allocate_frames(0).ok_or("no frame available")?;
    let second = mm::allocate_frames(0).ok_or("only one frame available")?;
    if first == second {
        return Err("the same frame was handed out twice");
    }

    let block = mm::allocate_frames(4).ok_or("no sixteen-frame block available")?;
    if !block.is_multiple_of(16) {
        return Err("a sixteen-frame block is not sixteen-frame aligned");
    }

    mm::deallocate_frames(block, 4);
    mm::deallocate_frames(second, 0);
    mm::deallocate_frames(first, 0);

    frames_hammer()
}

/// Check that `alloc` works, which is the whole point of the stage.
fn check_heap() -> Result<(), &'static str> {
    // A `Box`, which is the smallest possible proof that `GlobalAlloc` is wired
    // up at all.
    let boxed = Box::new(0x5EED_1234_ABCD_0001u64);
    if *boxed != 0x5EED_1234_ABCD_0001 {
        return Err("a Box did not hold what was put in it");
    }
    drop(boxed);

    // A `Vec` that grows through several reallocations, so the heap has to
    // move data between size classes and then between whole pages.
    let mut values: Vec<u64> = Vec::new();
    for value in 0..4096u64 {
        values.push(value.wrapping_mul(2_654_435_761));
    }
    for (index, value) in values.iter().enumerate() {
        if *value != (index as u64).wrapping_mul(2_654_435_761) {
            return Err("a Vec did not survive its own reallocations");
        }
    }
    drop(values);

    // A `BTreeMap`, which allocates nodes of an awkward size and frees them in
    // an order nothing controls.
    let mut map: BTreeMap<u64, u64> = BTreeMap::new();
    for key in 0..2048u64 {
        let _ = map.insert(key.wrapping_mul(2_654_435_761) % 100_003, key);
    }
    let entries = map.len();
    if entries == 0 {
        return Err("a BTreeMap held nothing");
    }
    for (key, value) in &map {
        if value.wrapping_mul(2_654_435_761) % 100_003 != *key {
            return Err("a BTreeMap returned a value under the wrong key");
        }
    }
    drop(map);

    if mm::heap_pages() == 0 {
        return Err("the heap never took a page, so nothing was really allocated");
    }
    Ok(())
}

/// Print what the loader handed over.
fn report(view: &BootView<'_>) {
    let info = view.raw();
    let mebibytes = |bytes: u64| bytes / (1024 * 1024);

    println!(
        "  memory   {} MiB total, {} MiB usable, {} regions",
        mebibytes(view.total_ram()),
        mebibytes(view.usable_ram()),
        view.regions().len()
    );
    println!(
        "  kernel   {:#x} -> {:#x}, {} KiB",
        info.kernel_phys,
        info.kernel_virt,
        info.kernel_len / 1024
    );
    println!(
        "  physmap  {:#x} covering {} MiB",
        info.physmap_base,
        mebibytes(info.physmap_len)
    );
    println!("  tables   root {:#x}", info.root_table_phys);

    if info.framebuffer.is_present() {
        println!(
            "  display  {}x{}, stride {}",
            info.framebuffer.width, info.framebuffer.height, info.framebuffer.stride
        );
    }
    if info.rsdp != 0 {
        println!("  acpi     rsdp at {:#x}", info.rsdp);
    }
    if info.dtb != 0 {
        println!("  fdt      at {:#x}", info.dtb);
    }
}

/// Check the things the rest of the kernel is about to assume.
///
/// This is stage 1's exit criterion. Every one of these is something that would
/// otherwise be discovered much later, by a subsystem that had no way to know
/// the ground under it was wrong.
fn self_check(view: &BootView<'_>, memory: &mut EarlyMemory) -> Result<(), &'static str> {
    let regions = view.regions();
    if regions.is_empty() {
        return Err("the memory map is empty");
    }

    // The frame allocator will walk this map assuming it is ordered and that no
    // two regions claim the same frame.
    let mut previous_end = 0u64;
    for region in regions {
        if region.base < previous_end {
            return Err("the memory map is unsorted or overlapping");
        }
        previous_end = region.end();
    }

    if view.usable_ram() == 0 {
        return Err("the memory map reports no usable RAM");
    }

    // The loader must have described its own allocations, or the kernel will
    // hand the frames holding its own page tables to the first caller that asks
    // for memory.
    let mut kinds = (false, false, false);
    for region in regions {
        match region.kind {
            MemKind::Kernel => kinds.0 = true,
            MemKind::PageTables => kinds.1 = true,
            MemKind::BootInfo => kinds.2 = true,
            _ => {}
        }
    }
    if kinds != (true, true, true) {
        return Err("the memory map does not describe the loader's own allocations");
    }

    check_direct_map(view, memory)?;
    check_early_mapper(view, memory)
}

/// Prove the direct map really does alias physical memory.
///
/// Everything from stage 2 onwards reads physical memory through it — page
/// tables, page-cache pages, `DMA` buffers — so if the loader mapped it at the
/// wrong offset, the first symptom would be a page table full of plausible
/// nonsense.
///
/// The test is to read the kernel's own first bytes twice: once through the
/// image mapping, once through the direct map at the physical address the
/// loader reported. They are the same bytes, so they must agree.
fn check_direct_map(view: &BootView<'_>, memory: &EarlyMemory) -> Result<(), &'static str> {
    let info = view.raw();

    for offset in [0u64, 1, 2, 3, 64, 4095] {
        // SAFETY: `kernel_virt` is where the loader mapped the kernel image and
        // `offset` is inside its first page, which is `.text` and always
        // present.
        let through_image =
            unsafe { core::ptr::read_volatile((info.kernel_virt + offset) as *const u8) };
        let through_physmap = memory.read_physical_byte(info.kernel_phys + offset);

        if through_image != through_physmap {
            return Err("the direct map does not alias the kernel image");
        }
    }
    Ok(())
}

/// Prove the kernel can read and extend the page tables the loader left.
///
/// Two things, both of which everything after stage 1 depends on. First that
/// walking the loader's tables from software agrees with what the hardware is
/// doing — the kernel's own image is the one mapping whose answer is known in
/// advance. Second that a *new* mapping can be installed and takes effect,
/// which is the whole of `EarlyMemory`'s job and, on AArch64, the only reason
/// there is a console at all.
fn check_early_mapper(view: &BootView<'_>, memory: &mut EarlyMemory) -> Result<(), &'static str> {
    let info = view.raw();

    match memory.translate(info.kernel_virt) {
        Some(phys) if phys == info.kernel_phys => {}
        Some(_) => return Err("walking the page tables disagrees with the loader"),
        None => return Err("the kernel image is not mapped in its own page tables"),
    }

    // A device window the kernel has a real use for, when firmware left one.
    // On QEMU's AArch64 `virt` there is no display, so this does not run there
    // — the PL011 console took the same path a moment ago.
    if info.framebuffer.is_present() {
        let at = KERNEL_VMAP_BASE + FRAMEBUFFER_WINDOW;
        if memory
            .map_device(at, info.framebuffer.phys, PAGE_SIZE)
            .is_err()
        {
            return Err("could not map the framebuffer");
        }
        if memory.translate(at) != Some(info.framebuffer.phys) {
            return Err("the framebuffer mapping does not resolve to the framebuffer");
        }
    }

    Ok(())
}

/// Offset within the kernel's dynamic mapping area for the framebuffer window,
/// clear of the `AArch64` console at offset zero.
const FRAMEBUFFER_WINDOW: u64 = 0x1000_0000;

/// Where a kernel panic ends up.
///
/// There is no supervisor above us and no unwinder, so this says what happened
/// and stops the machine. The marker is what turns a panic into a failed boot
/// test rather than a two-minute timeout with no explanation.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!();
    println!("FERRIX-PANIC {info}");
    arch::halt()
}

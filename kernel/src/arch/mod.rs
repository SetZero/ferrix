//! The architecture facade.
//!
//! Generic kernel code reaches the CPU only through this module — never by
//! naming `x86_64`, `aarch64` or `armv7a` — and `#[cfg(target_arch)]` appears
//! nowhere else in the tree. Both rules are enforced by
//! `scripts/check-crate-layering.sh`, because a facade maintained by convention
//! is a facade for about six weeks.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "arm")]
mod armv7a;
#[cfg(target_arch = "x86_64")]
mod x86_64;

// Side-channel defences: what every architecture shares, its boot check, and
// each architecture's own half, which the shared part reaches by one name.
// `docs/certification/SPECULATION.md`.
mod speculation;
mod speculation_check;
#[cfg(target_arch = "aarch64")]
use aarch64::speculation as machine_speculation;
#[cfg(target_arch = "arm")]
use armv7a::speculation as machine_speculation;
pub(crate) use speculation::{nospec_below, nospec_index};
pub(crate) use speculation_check::check as check_speculation;
#[cfg(target_arch = "x86_64")]
use x86_64::speculation as machine_speculation;

/// Which `struct stat` this architecture's stat calls fill in.
///
/// The choice is an architecture's, so it is stated here beside the other ABI
/// facts the facade carries — `OPEN_FLAGS`, `EPOLL_EVENT_BYTES` — and each
/// architecture names one in its `STAT_LAYOUT`. What the bytes *are* is not an
/// architecture's business: `crate::syscall::stat` owns the encoding and
/// carries this type's `impl`.
///
/// It reads oddly to define a Linux type in the architecture facade until you
/// try it the other way round, which is how it was: the facade named
/// `crate::syscall::stat::StatLayout`, and the trusted core therefore depended
/// on the Linux personality for a constant. Data here, behaviour there, and
/// the dependency points the way `scripts/check-item-boundary.py` requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatLayout {
    /// x86-64's own, from `arch/x86/include/uapi/asm/stat.h`: 144 bytes. It
    /// predates the generic header and was kept rather than replaced.
    Legacy,
    /// The generic one, from `include/uapi/asm-generic/stat.h`: 128 bytes.
    Generic,
    /// ARMv7-A's `struct stat64`, from `arch/arm/include/uapi/asm/stat.h`:
    /// 104 bytes, filled by the `64` calls that are all it answers.
    Stat64,
}

// Register-level drivers for hardware the architecture that uses them has
// already found in the machine's description — the MADT on AArch64, the device
// tree on ARMv7-A. The GICv2 is shared by both Arm architectures. The two
// serial ports are ARMv7-A's, which has to pick between them because the
// machines it targets do not agree on one: `armv7a::console` is where the
// device tree decides, and the layering check keeps generic code from naming
// any of the three.
#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
mod gicv2;
#[cfg(target_arch = "arm")]
mod pl011;
#[cfg(target_arch = "arm")]
mod stm32_usart;

#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{
    ARCH, CpuStarter, EPOLL_EVENT_BYTES, Irq, NAME, OPEN_FLAGS, PageEncoding, STAT_LAYOUT,
    TLB_FLUSH_IS_BROADCAST, TrapFrame, USER_ARGUMENT_PROGRAM, USER_COW_PROGRAM, USER_EXEC_PROGRAM,
    USER_FAULT_PROGRAM, USER_FORK_PROGRAM, USER_MPROTECT_PROGRAM, USER_NAMESPACE_PROGRAM,
    USER_NATIVE_PROGRAM, USER_SHARED_PROGRAM, USER_SPIN_PROGRAM, USER_STEP_PROGRAM,
    USER_STEP_STATUS, USER_TEST_PROGRAM, USER_TEST_STATUS, UserRegs, UserState,
    advance_past_breakpoint, breakpoint, classify, console, console_receive_irq, counter_hz,
    counter_now, cpu_local, cpu_local_register, decode_syscall, describe_cpus, disable_interrupts,
    drain_console, drop_identity_map, enable_console_receive, enable_interrupts, enter_user,
    flush_tlb, forbid_user_access, frame_pointer, halt, hardware_id, hardware_random,
    identity_map_live, identity_root, init_console, init_interrupts, init_traps, install_user_root,
    interrupts_enabled, ipi_irq, kernel_write_protected, mask_interrupt, msi_allocate,
    msi_doorbell, permit_user_access, prepare_stack, prepare_user_root, read_console_byte,
    report_trap, reset, reset_user_state, restore_user_state, resume_user, save_user_state,
    send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to, system_call,
    take_console_byte, timer_arm, timer_disarm, timer_irq, uninstall_user_root, unmask_interrupt,
    user_hwcaps, user_platform, wait_for_interrupt, wait_for_work,
};
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::{
    ARCH, CpuStarter, EPOLL_EVENT_BYTES, Irq, NAME, OPEN_FLAGS, PageEncoding, STAT_LAYOUT,
    TLB_FLUSH_IS_BROADCAST, TrapFrame, USER_ARGUMENT_PROGRAM, USER_COW_PROGRAM, USER_EXEC_PROGRAM,
    USER_FAULT_PROGRAM, USER_FORK_PROGRAM, USER_MPROTECT_PROGRAM, USER_NAMESPACE_PROGRAM,
    USER_NATIVE_PROGRAM, USER_SHARED_PROGRAM, USER_SPIN_PROGRAM, USER_STEP_PROGRAM,
    USER_STEP_STATUS, USER_TEST_PROGRAM, USER_TEST_STATUS, UserRegs, UserState,
    advance_past_breakpoint, breakpoint, classify, console, console_receive_irq, counter_hz,
    counter_now, cpu_local, cpu_local_register, decode_syscall, describe_cpus, disable_interrupts,
    drain_console, drop_identity_map, enable_console_receive, enable_interrupts, enter_user,
    flush_tlb, forbid_user_access, frame_pointer, halt, hardware_id, hardware_random,
    identity_map_live, identity_root, init_console, init_interrupts, init_traps, install_user_root,
    interrupts_enabled, ipi_irq, kernel_write_protected, mask_interrupt, msi_allocate,
    msi_doorbell, permit_user_access, prepare_stack, prepare_user_root, read_console_byte,
    report_trap, reset, reset_user_state, restore_user_state, resume_user, save_user_state,
    send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to, system_call,
    take_console_byte, timer_arm, timer_disarm, timer_irq, uninstall_user_root, unmask_interrupt,
    user_hwcaps, user_platform, wait_for_interrupt, wait_for_work,
};
// The watchdogs a board's firmware leaves running: found at boot, fed once
// the scheduler can run a task, fired to reset. Only the Pixel 7's today.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{init_watchdogs, start_watchdogs};
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::{init_watchdogs, start_watchdogs};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{init_watchdogs, start_watchdogs};
// The program stage 13's `CLONE_INTO_CGROUP` check runs.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::USER_INTO_CGROUP_PROGRAM;
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::USER_INTO_CGROUP_PROGRAM;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::USER_INTO_CGROUP_PROGRAM;
// Signal delivery: the register context the way back to user mode loads, the
// architecture's signal frame, and the signal a user-mode fault becomes.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{
    SIGNAL_RED_ZONE, USER_DETHREAD_PROGRAM, USER_EXITS_PROGRAM, USER_HANDOFF_PROGRAM,
    USER_SIGNAL_PROGRAM, USER_STOPPED_PROGRAM, USER_SYSLOG_PROGRAM, USER_THREAD_PROGRAM,
    UserContext, fault_signal, restore_signal_frame, setup_signal_frame,
};
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::{
    SIGNAL_RED_ZONE, USER_DETHREAD_PROGRAM, USER_EXITS_PROGRAM, USER_HANDOFF_PROGRAM,
    USER_SIGNAL_PROGRAM, USER_STOPPED_PROGRAM, USER_SYSLOG_PROGRAM, USER_THREAD_PROGRAM,
    UserContext, fault_signal, restore_signal_frame, setup_signal_frame,
};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{
    ARCH, CpuStarter, EPOLL_EVENT_BYTES, Irq, NAME, OPEN_FLAGS, PageEncoding, STAT_LAYOUT,
    TLB_FLUSH_IS_BROADCAST, TrapFrame, USER_ARGUMENT_PROGRAM, USER_COW_PROGRAM, USER_EXEC_PROGRAM,
    USER_FAULT_PROGRAM, USER_FORK_PROGRAM, USER_MPROTECT_PROGRAM, USER_NAMESPACE_PROGRAM,
    USER_NATIVE_PROGRAM, USER_SHARED_PROGRAM, USER_SPIN_PROGRAM, USER_STEP_PROGRAM,
    USER_STEP_STATUS, USER_TEST_PROGRAM, USER_TEST_STATUS, UserRegs, UserState,
    advance_past_breakpoint, breakpoint, classify, console, console_receive_irq, counter_hz,
    counter_now, cpu_local, cpu_local_register, decode_syscall, describe_cpus, disable_interrupts,
    drain_console, drop_identity_map, enable_console_receive, enable_interrupts, enter_user,
    flush_tlb, forbid_user_access, frame_pointer, halt, hardware_id, hardware_random,
    identity_map_live, identity_root, init_console, init_interrupts, init_traps, install_user_root,
    interrupts_enabled, ipi_irq, kernel_write_protected, mask_interrupt, msi_allocate,
    msi_doorbell, permit_user_access, prepare_stack, prepare_user_root, read_console_byte,
    report_trap, reset, reset_user_state, restore_user_state, resume_user, save_user_state,
    send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to, system_call,
    take_console_byte, timer_arm, timer_disarm, timer_irq, uninstall_user_root, unmask_interrupt,
    user_hwcaps, user_platform, wait_for_interrupt, wait_for_work,
};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{
    SIGNAL_RED_ZONE, USER_DETHREAD_PROGRAM, USER_EXITS_PROGRAM, USER_HANDOFF_PROGRAM,
    USER_SIGNAL_PROGRAM, USER_STOPPED_PROGRAM, USER_SYSLOG_PROGRAM, USER_THREAD_PROGRAM,
    UserContext, fault_signal, restore_signal_frame, setup_signal_frame,
};
// The scoped TLB shootdown: one page invalidated, one processor interrupted,
// and the program that checks it from user mode.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{USER_RMAP_PROGRAM, flush_tlb_page, send_ipi_to};
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::{USER_RMAP_PROGRAM, flush_tlb_page, send_ipi_to};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{USER_RMAP_PROGRAM, flush_tlb_page, send_ipi_to};

// Deciding and applying the side-channel defences, on the boot processor.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::init_speculation;
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::init_speculation;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::init_speculation;

// The boot check for exceptions that arrive wherever the processor is.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::check_exception_entry;
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::check_exception_entry;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::check_exception_entry;

// Cache maintenance for a device that does not snoop the caches: the
// DK board's display controller reads a framebuffer straight from memory, so
// whatever a program drew has to be written back from the caches to the point
// of coherency before the controller is told to read it.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::clean_for_device;
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::clean_for_device;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::clean_for_device;

// The same, and the lines dropped too: for memory a program will share with
// such a device through a mapping past the caches (`vmo_pin`'s
// `PIN_COHERENT`), where a line left in the cache could be written back over
// what the device wrote.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::flush_for_device;
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::flush_for_device;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::flush_for_device;

// Instructions the kernel wrote into memory, made the ones every processor
// fetches there: for a page about to be mapped executable in user mode. An
// Arm core's instruction cache does not see what its data side wrote, nor
// what another core's did, until it is told to.
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::sync_instructions;
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::sync_instructions;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::sync_instructions;

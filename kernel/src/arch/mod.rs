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
    flush_tlb, frame_pointer, halt, hardware_id, hardware_random, identity_map_live, identity_root,
    init_console, init_interrupts, init_traps, install_user_root, interrupts_enabled, ipi_irq,
    kernel_write_protected, mask_interrupt, msi_allocate, msi_doorbell, prepare_stack,
    prepare_user_root, read_console_byte, report_trap, reset, reset_user_state, restore_user_state,
    resume_user, save_user_state, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown,
    switch_to, system_call, take_console_byte, timer_arm, timer_disarm, timer_irq,
    uninstall_user_root, unmask_interrupt, user_hwcaps, user_platform, wait_for_interrupt,
    wait_for_work,
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
    flush_tlb, frame_pointer, halt, hardware_id, hardware_random, identity_map_live, identity_root,
    init_console, init_interrupts, init_traps, install_user_root, interrupts_enabled, ipi_irq,
    kernel_write_protected, mask_interrupt, msi_allocate, msi_doorbell, prepare_stack,
    prepare_user_root, read_console_byte, report_trap, reset, reset_user_state, restore_user_state,
    resume_user, save_user_state, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown,
    switch_to, system_call, take_console_byte, timer_arm, timer_disarm, timer_irq,
    uninstall_user_root, unmask_interrupt, user_hwcaps, user_platform, wait_for_interrupt,
    wait_for_work,
};
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
    flush_tlb, frame_pointer, halt, hardware_id, hardware_random, identity_map_live, identity_root,
    init_console, init_interrupts, init_traps, install_user_root, interrupts_enabled, ipi_irq,
    kernel_write_protected, mask_interrupt, msi_allocate, msi_doorbell, prepare_stack,
    prepare_user_root, read_console_byte, report_trap, reset, reset_user_state, restore_user_state,
    resume_user, save_user_state, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown,
    switch_to, system_call, take_console_byte, timer_arm, timer_disarm, timer_irq,
    uninstall_user_root, unmask_interrupt, user_hwcaps, user_platform, wait_for_interrupt,
    wait_for_work,
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

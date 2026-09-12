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
    ARCH, CpuStarter, Irq, NAME, OPEN_FLAGS, PageEncoding, STAT_LAYOUT, TLB_FLUSH_IS_BROADCAST,
    TrapFrame, USER_EXEC_PROGRAM, USER_FORK_PROGRAM, USER_NATIVE_PROGRAM, USER_SPIN_PROGRAM,
    USER_TEST_PROGRAM, USER_TEST_STATUS, UserRegs, UserState, advance_past_breakpoint, breakpoint,
    classify, console, counter_hz, counter_now, cpu_local, cpu_local_register, decode_syscall,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, enter_user, flush_tlb,
    frame_pointer, halt, hardware_id, identity_root, init_console, init_interrupts, init_traps,
    install_user_root, ipi_irq, mask_interrupt, prepare_stack, prepare_user_root,
    read_console_byte, report_trap, reset_user_state, restore_user_state, resume_user,
    save_user_state, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to,
    system_call, timer_arm, timer_disarm, timer_irq, uninstall_user_root, unmask_interrupt,
    wait_for_interrupt, wait_for_work,
};
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::{
    ARCH, CpuStarter, Irq, NAME, OPEN_FLAGS, PageEncoding, STAT_LAYOUT, TLB_FLUSH_IS_BROADCAST,
    TrapFrame, USER_EXEC_PROGRAM, USER_FORK_PROGRAM, USER_NATIVE_PROGRAM, USER_SPIN_PROGRAM,
    USER_TEST_PROGRAM, USER_TEST_STATUS, UserRegs, UserState, advance_past_breakpoint, breakpoint,
    classify, console, counter_hz, counter_now, cpu_local, cpu_local_register, decode_syscall,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, enter_user, flush_tlb,
    frame_pointer, halt, hardware_id, identity_root, init_console, init_interrupts, init_traps,
    install_user_root, ipi_irq, mask_interrupt, prepare_stack, prepare_user_root,
    read_console_byte, report_trap, reset_user_state, restore_user_state, resume_user,
    save_user_state, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to,
    system_call, timer_arm, timer_disarm, timer_irq, uninstall_user_root, unmask_interrupt,
    wait_for_interrupt, wait_for_work,
};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{
    ARCH, CpuStarter, Irq, NAME, OPEN_FLAGS, PageEncoding, STAT_LAYOUT, TLB_FLUSH_IS_BROADCAST,
    TrapFrame, USER_EXEC_PROGRAM, USER_FORK_PROGRAM, USER_NATIVE_PROGRAM, USER_SPIN_PROGRAM,
    USER_TEST_PROGRAM, USER_TEST_STATUS, UserRegs, UserState, advance_past_breakpoint, breakpoint,
    classify, console, counter_hz, counter_now, cpu_local, cpu_local_register, decode_syscall,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, enter_user, flush_tlb,
    frame_pointer, halt, hardware_id, identity_root, init_console, init_interrupts, init_traps,
    install_user_root, ipi_irq, mask_interrupt, prepare_stack, prepare_user_root,
    read_console_byte, report_trap, reset_user_state, restore_user_state, resume_user,
    save_user_state, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to,
    system_call, timer_arm, timer_disarm, timer_irq, uninstall_user_root, unmask_interrupt,
    wait_for_interrupt, wait_for_work,
};

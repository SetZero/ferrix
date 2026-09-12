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
    CpuStarter, Irq, NAME, PageEncoding, TLB_FLUSH_IS_BROADCAST, TrapFrame,
    advance_past_breakpoint, breakpoint, classify, console, counter_hz, counter_now, cpu_local,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, flush_tlb, halt,
    hardware_id, identity_root, init_console, init_interrupts, init_traps, ipi_irq, prepare_stack,
    report_trap, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to,
    timer_arm, timer_disarm, timer_irq, wait_for_interrupt, wait_for_work,
};
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::{
    CpuStarter, Irq, NAME, PageEncoding, TLB_FLUSH_IS_BROADCAST, TrapFrame,
    advance_past_breakpoint, breakpoint, classify, console, counter_hz, counter_now, cpu_local,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, flush_tlb, halt,
    hardware_id, identity_root, init_console, init_interrupts, init_traps, ipi_irq, prepare_stack,
    report_trap, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to,
    timer_arm, timer_disarm, timer_irq, wait_for_interrupt, wait_for_work,
};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{
    CpuStarter, Irq, NAME, PageEncoding, TLB_FLUSH_IS_BROADCAST, TrapFrame,
    advance_past_breakpoint, breakpoint, classify, console, counter_hz, counter_now, cpu_local,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, flush_tlb, halt,
    hardware_id, identity_root, init_console, init_interrupts, init_traps, ipi_irq, prepare_stack,
    report_trap, send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, switch_to,
    timer_arm, timer_disarm, timer_irq, wait_for_interrupt, wait_for_work,
};

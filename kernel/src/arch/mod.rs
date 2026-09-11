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

// Register-level drivers for hardware the Arm machines share, which the
// architecture that uses them has already found in the machine's description.
// `pl011` is visible to the crate only because it is re-exported as the
// facade's `console`; the layering check keeps generic code from naming it.
#[cfg(target_arch = "arm")]
mod gicv2;
#[cfg(target_arch = "arm")]
pub(crate) mod pl011;

#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{
    CpuStarter, Irq, NAME, PageEncoding, TLB_FLUSH_IS_BROADCAST, TrapFrame,
    advance_past_breakpoint, breakpoint, classify, console, counter_hz, counter_now, cpu_local,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, flush_tlb, halt,
    hardware_id, identity_root, init_console, init_interrupts, init_traps, ipi_irq, report_trap,
    send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, timer_arm, timer_disarm,
    timer_irq, wait_for_interrupt, wait_for_work,
};
#[cfg(target_arch = "arm")]
pub(crate) use armv7a::{
    CpuStarter, Irq, NAME, PageEncoding, TLB_FLUSH_IS_BROADCAST, TrapFrame,
    advance_past_breakpoint, breakpoint, classify, console, counter_hz, counter_now, cpu_local,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, flush_tlb, halt,
    hardware_id, identity_root, init_console, init_interrupts, init_traps, ipi_irq, report_trap,
    send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, timer_arm, timer_disarm,
    timer_irq, wait_for_interrupt, wait_for_work,
};
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{
    CpuStarter, Irq, NAME, PageEncoding, TLB_FLUSH_IS_BROADCAST, TrapFrame,
    advance_past_breakpoint, breakpoint, classify, console, counter_hz, counter_now, cpu_local,
    describe_cpus, disable_interrupts, drop_identity_map, enable_interrupts, flush_tlb, halt,
    hardware_id, identity_root, init_console, init_interrupts, init_traps, ipi_irq, report_trap,
    send_ipi_to_others, service_interrupts, set_cpu_local, shutdown, timer_arm, timer_disarm,
    timer_irq, wait_for_interrupt, wait_for_work,
};

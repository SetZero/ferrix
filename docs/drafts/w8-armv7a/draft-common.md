# Draft: arch/arm_common low-level requirements (area A)

Read against worktree w8-armv7a at b9b7d26e, and `e4-w8:docs/sysml/19-aarch64-requirements.sysml`
(Interrupts and Console packages) as the model. The console's generic requirements
(docs/sysml/23-console-requirements.sysml) name no port-level unit: `L.console.*` stops at
`console::*` and `arch::armv7a::console::*` / `arch::aarch64::console::ramoops_zone`. So nothing
here duplicates it. The port requirements below are the byte-level half beneath L.console.9/.10/.13
and .20 to .24.

Which machine shows what: the GICv2 and the PL011 are on QEMU's `virt`, which boots both
Arm architectures (AArch64 by default with a GICv2, ARMv7-A always). The GICv2m frame is `virt`'s
too. The STM32 USART is only on the STM32MP157D-DK1, which no gate boots.

## Requirements

```sysml
    package ArmCommonInterrupts {
        doc /* kernel/src/arch/arm_common/gicv2.rs: the GICv2 both Arm
             * architectures drive -- the distributor and CPU interface
             * brought up, lines let through and stopped, each later core's
             * banked half, the claim, and message-signalled interrupts
             * through a GICv2m frame. */

        requirement <'L.armv7a.A1'> gicv2IsBroughtUp : ItemLowLevel {
            attribute :>> statement = "init shall map the distributor's 4 KiB
                and the CPU interface's 8 KiB as device memory, and configure
                shall turn the distributor off, disable every line GICD_TYPER
                counts and give each the default priority, then enable the
                distributor, open the CPU interface's priority mask to every
                priority and enable the interface.";
            attribute :>> criterion = "Every QEMU virt boot, ARMv7-A and
                AArch64's GICv2 one, takes its timer interrupts; a check that
                every line reads back disabled after bring-up except those enabled
                since, and that GICC_PMR reads 0xFF, is not written.";
            attribute :>> parent = "H.IRQ.3";
            attribute :>> unit = ("arch::arm_common::gicv2::init",
                "arch::arm_common::gicv2::configure");
        }
        requirement <'L.armv7a.A2'> linesAreLetThroughAndStopped : ItemLowLevel {
            attribute :>> statement = "enable shall set a line's bit in the
                distributor's set-enable register and disable shall set it in the
                clear-enable register, through the distributor window init
                recorded, so that the register reads back what was asked.";
            attribute :>> criterion = "An idle shared line and an idle private
                line, let through and stopped again, each read back enabled and
                then disabled at the distributor (the `machine` line's idle
                lines).";
            attribute :>> parent = "H.IRQ.2";
            attribute :>> unit = ("arch::arm_common::gicv2::enable",
                "arch::arm_common::gicv2::disable", "arch::arm_common::gicv2::window");
        }
        requirement <'L.armv7a.A3'> sharedLinesAreRouted : ItemLowLevel {
            attribute :>> statement = "enable shall give the line the default
                priority, and a shared line whose target byte is zero the calling
                core's CPU interface as its target, leaving a target already set
                as it was.";
            attribute :>> criterion = "ARMv7-A's QEMU boot, whose U-Boot leaves
                every target byte zero, takes its console and virtio interrupts;
                a check that enabling an idle shared line with a zero target reads
                back this core's bit and priority 0xA0, and that a line with a
                target keeps it, is not written.";
            attribute :>> parent = "H.IRQ.1";
            attribute :>> unit = "arch::arm_common::gicv2::enable";
        }
        requirement <'L.armv7a.A4'> secondariesTakeProcessorInterrupts : ItemLowLevel {
            attribute :>> statement = "init_this_cpu shall open the calling
                core's CPU interface -- the priority mask to every priority, the
                interface enabled -- and enable in its banked registers the
                inter-processor interrupt's SGI the boot core enabled, so that an
                SGI sent to that core is taken.";
            attribute :>> criterion = "Over 100 rounds of work run everywhere,
                every secondary takes at least one inter-processor interrupt (the
                `smp` line), on ARMv7-A and on AArch64's GICv2 boot.";
            attribute :>> parent = ("H.MEM.7", "H.SCHED.1");
            attribute :>> unit = "arch::arm_common::gicv2::init_this_cpu";
        }
        requirement <'L.armv7a.A5'> privateLinesFollowEveryCore : ItemLowLevel {
            attribute :>> statement = "enable and disable shall record a private
                line's enable, and init_this_cpu shall give each later core's
                banked lines 0 to 31 the default priority and enable there every
                private line recorded, the timer's among them.";
            attribute :>> criterion = "A check that each secondary's banked
                set-enable word reads back the recorded set, the timer's PPI in
                it, is not written; the scheduler's fairness check nearly shows it
                (each core's pinned spinners all start only if that core's timer
                preempts) but asserts EEVDF's shares, not the enable.";
            attribute :>> parent = "H.BOOT.5";
            attribute :>> unit = ("arch::arm_common::gicv2::init_this_cpu",
                "arch::arm_common::gicv2::enable", "arch::arm_common::gicv2::disable");
        }
        requirement <'L.armv7a.A6'> claimSaysWhatArrived : ItemLowLevel {
            attribute :>> statement = "claim shall read GICC_IAR once and give
                the interrupt's identifier with the whole acknowledgement value,
                CPU identifier bits included, for complete, and none for the
                identifiers 1020 to 1023.";
            attribute :>> criterion = "Every boot's timer ticks and device
                interrupts arrive; a check that a spurious answer yields none and
                that a claimed SGI's acknowledgement keeps its source-CPU bits is
                not written.";
            attribute :>> parent = ("H.IRQ.1", "H.IRQ.3");
            attribute :>> unit = "arch::arm_common::gicv2::claim";
        }
        requirement <'L.armv7a.A7'> unusableGicv2mFramesAreRefused : ItemLowLevel {
            attribute :>> statement = "init_msi_frame shall refuse a GICv2m frame
                at physical address zero, and one whose stated first identifier is
                past 1023.";
            attribute :>> criterion = "A frame at address zero with no stated
                range, and one at 0x0802_0000 whose stated range starts at 1024,
                are both refused.";
            attribute :>> parent = "H.IRQ.1";
            attribute :>> unit = "arch::arm_common::gicv2::init_msi_frame";
        }
        requirement <'L.armv7a.A8'> theGicv2mRangeIsRecorded : ItemLowLevel {
            attribute :>> statement = "init_msi_frame shall take the SPI range
                firmware states, or else the one the frame's MSI_TYPER reads,
                refuse a range that is not shared peripheral interrupts (count
                zero, first below 32, or reaching 1020) or a frame it cannot map,
                and record the frame and at most 64 SPIs of the range.";
            attribute :>> criterion = "QEMU virt's frame is read from its
                MSI_TYPER on both Arm architectures; a check that a stated range
                is recorded as given, capped at 64, and that a range reaching 1020
                is refused, is not written (it would have to put the machine's
                frame back after).";
            attribute :>> parent = "H.IRQ.1";
            attribute :>> unit = "arch::arm_common::gicv2::init_msi_frame";
        }
        requirement <'L.armv7a.A9'> msiVectorsAreAllocated : ItemLowLevel {
            attribute :>> statement = "msi_allocate shall take the lowest SPI of
                the frame's range no allocation has taken, make it edge-triggered
                while still disabled, enable it, and give the message that raises
                it -- the frame's SETSPI address, the identifier as data; and
                shall refuse when there is no frame or every SPI is taken.";
            attribute :>> criterion = "A check that two allocations get two
                distinct SPIs of the range, each edge-triggered and enabled, and
                that one past the count is refused, is not written; the virtio
                entropy check nearly shows delivery (its MSI-X completion arrives
                through a vector from here on QEMU virt), but a completion without
                its interrupt is reported and skipped, not failed.";
            attribute :>> parent = "H.IRQ.1";
            attribute :>> unit = ("arch::arm_common::gicv2::msi_allocate",
                "arch::arm_common::gicv2::set_edge_triggered");
        }
        requirement <'L.armv7a.A10'> theGicv2mDoorbellIsNamed : ItemLowLevel {
            attribute :>> statement = "msi_doorbell shall name the page holding
                the GICv2m frame init_msi_frame recorded, and none when it recorded
                none.";
            attribute :>> criterion = "A check that the page named is the one
                holding the frame the device tree gives, and none on a machine
                without one, is not written; the machine check's doorbell
                assertion cannot fail (see Notes).";
            attribute :>> parent = "H.DMA.3";
            attribute :>> unit = "arch::arm_common::gicv2::msi_doorbell";
        }
    }

    package Pl011 {
        doc /* kernel/src/arch/arm_common/pl011.rs: the PL011 UART, the
             * console on QEMU's virt for both Arm architectures. */

        requirement <'L.armv7a.A11'> pl011BytesGoOut : ItemLowLevel {
            attribute :>> statement = "init shall map the page holding the
                PL011's registers as device memory, keeping their offset in the
                page, and write_byte shall wait at most 100,000 polls for room in
                the transmit FIFO and then write the byte, leaving the baud rate
                and line format as firmware set them.";
            attribute :>> criterion = "Every QEMU virt boot, ARMv7-A and
                AArch64, carries its log through the PL011 and test-boot finds
                FERRIX-BOOT-OK in it; that is a gate's reading, not a check.";
            attribute :>> parent = ("H.BOOT.8", "H.FAIL.1");
            attribute :>> unit = ("arch::arm_common::pl011::init",
                "arch::arm_common::pl011::write_byte", "arch::arm_common::pl011::read",
                "arch::arm_common::pl011::write");
        }
        requirement <'L.armv7a.A12'> pl011Drains : ItemLowLevel {
            attribute :>> statement = "drain shall return once the PL011's
                transmit FIFO is empty and the port no longer busy, or after at
                most 10,000,000 polls.";
            attribute :>> criterion = "QEMU's port sends at once, so no QEMU
                boot can fail it; a check would need a port that is still sending,
                which only real hardware has.";
            attribute :>> parent = "H.BOOT.6";
            attribute :>> unit = "arch::arm_common::pl011::drain";
        }
        requirement <'L.armv7a.A13'> pl011BytesComeIn : ItemLowLevel {
            attribute :>> statement = "read_byte shall give the next byte in the
                PL011's receive FIFO, none when it is empty, and deliver a byte
                that arrived with an error flag while clearing the flags; and
                enable_receive_interrupt shall let out the receive and
                receive-timeout interrupts, leaving the others as they were.";
            attribute :>> criterion = "test-jobs's typed steps on QEMU virt are
                answered; a check that a byte the host sends reaches read_byte by
                the receive interrupt is not written.";
            attribute :>> parent = "H.BOOT.8";
            attribute :>> unit = ("arch::arm_common::pl011::read_byte",
                "arch::arm_common::pl011::enable_receive_interrupt");
        }
        requirement <'L.armv7a.A14'> pl011TakesWhatItHasRoomFor : ItemLowLevel {
            attribute :>> statement = "transmit_room shall say 1 while the
                PL011's transmit FIFO is not full and 0 when it is, put shall
                write a byte without waiting, and transmit_interrupt shall set or
                clear the transmit interrupt's mask bit alone.";
            attribute :>> criterion = "The console output check's line is sent
                by the transmit interrupt on QEMU virt (L.console.9, .10), but it
                asserts the ring's behaviour and sits in a product file; a check
                of the port's room and mask bit is not written.";
            attribute :>> parent = "H.BOOT.8";
            attribute :>> unit = ("arch::arm_common::pl011::transmit_room",
                "arch::arm_common::pl011::put",
                "arch::arm_common::pl011::transmit_interrupt");
        }
    }

    package Stm32Usart {
        doc /* kernel/src/arch/arm_common/stm32_usart.rs: the STM32MP1's
             * USART, the console on the STM32MP157D-DK1 board, which no gate
             * boots. */

        requirement <'L.armv7a.A15'> usartBytesGoOut : ItemLowLevel {
            attribute :>> statement = "init shall map the page holding the
                USART's registers as device memory, keeping their offset in the
                page, and write_byte shall wait at most 100,000 polls for
                ISR.TXE and then write the byte to TDR, leaving the baud rate and
                line format as firmware set them.";
            attribute :>> criterion = "Only the DK1 shows it: its boot log
                reaches the ST-LINK's virtual serial port. No gate boots the
                board, and no check is written.";
            attribute :>> parent = ("H.BOOT.8", "H.FAIL.1");
            attribute :>> unit = ("arch::arm_common::stm32_usart::init",
                "arch::arm_common::stm32_usart::write_byte",
                "arch::arm_common::stm32_usart::read",
                "arch::arm_common::stm32_usart::write");
        }
        requirement <'L.armv7a.A16'> usartDrains : ItemLowLevel {
            attribute :>> statement = "drain shall return once the USART reports
                transmission complete (ISR.TC), or after at most 10,000,000
                polls.";
            attribute :>> criterion = "Only the DK1 shows it: the last line
                before PSCI SYSTEM_OFF reaches the terminal whole. No gate boots
                the board.";
            attribute :>> parent = "H.BOOT.6";
            attribute :>> unit = "arch::arm_common::stm32_usart::drain";
        }
        requirement <'L.armv7a.A17'> usartBytesComeIn : ItemLowLevel {
            attribute :>> statement = "read_byte shall clear an overrun whenever
                ISR shows one and give the byte in RDR when ISR.RXNE is set, none
                otherwise; and enable_receive_interrupt shall set CR1.RXNEIE
                alone.";
            attribute :>> criterion = "Only the DK1 shows it, by typing at its
                shell. No gate boots the board.";
            attribute :>> parent = "H.BOOT.8";
            attribute :>> unit = ("arch::arm_common::stm32_usart::read_byte",
                "arch::arm_common::stm32_usart::enable_receive_interrupt");
        }
        requirement <'L.armv7a.A18'> usartTakesWhatItHasRoomFor : ItemLowLevel {
            attribute :>> statement = "transmit_room shall say 1 while ISR.TXE
                is set and 0 otherwise, put shall write a byte without waiting,
                transmit_interrupt shall set or clear CR1.TXFEIE on a port whose
                FIFO firmware enabled and CR1.TXEIE on one without, and has_fifo
                shall say which.";
            attribute :>> criterion = "Only the DK1 shows it: its console line
                says it sends through the USART's FIFO. No gate boots the
                board.";
            attribute :>> parent = "H.BOOT.8";
            attribute :>> unit = ("arch::arm_common::stm32_usart::transmit_room",
                "arch::arm_common::stm32_usart::put",
                "arch::arm_common::stm32_usart::transmit_interrupt",
                "arch::arm_common::stm32_usart::has_fifo");
        }
    }
```

## Verification

| id | verifying check | why |
|---|---|---|
| A1 | baseline | Every boot relies on it, but no check reads the distributor's state back after bring-up. |
| A2 | `arch::armv7a::check::check_masking` | Unmasks and masks an idle SPI and an idle PPI through `unmask_interrupt`/`mask_interrupt` → `gicv2::enable`/`disable`, and reads the set-enable bit back each time. That is the whole criterion. AArch64's `arch::aarch64::check::check_masking` asserts the same on the GICv2 boot and could carry the tag as well. |
| A3 | baseline | check_masking reads back only the enable bit, not the priority byte or the target byte. |
| A4 | `smp::check::everywhere` | Asserts that every secondary takes ≥1 IPI over 100 rounds. That needs this core's PMR and CTLR opened and the SGI enabled by `init_this_cpu`. This matches x86's `L.x86_64.19` (apic::init_this_cpu), which the same check verifies. Caveat: the check already carries two `Verifies:` lines, so the tag goes on a third. |
| A5 | baseline | Nothing reads a secondary's banked enables. `sched::check::fairness` nearly shows the timer replay, but asserts EEVDF's shares. |
| A6 | baseline | `check_refusals` refuses 1020/1023 in `armv7a::mask_interrupt`, not in `claim`. |
| A7 | `arch::armv7a::check::check_refusals` | `init_msi_frame(0, None)` and `init_msi_frame(0x0802_0000, Some((1024, 64)))` are both required to be refused. That is exactly the criterion. `arch::aarch64::check::check_refusals` asserts the same two calls too, and 19 credits it only with L.aarch64.17/.21. |
| A8 | baseline | No check records a range. Only refusals are tested (A7). |
| A9 | baseline | `pci::virtio::entropy` nearly does it, but skips instead of failing on a missing interrupt, and never checks edge-triggering or distinctness. |
| A10 | baseline | check_refusals' doorbell assertion is vacuous (see Notes). |
| A11 | baseline | Shown by every QEMU virt boot's serial log (test-boot), which is a gate and not a check function. |
| A12 | baseline | QEMU's PL011 is never busy, so no QEMU check can fail it. |
| A13 | baseline | Input is shown by test-jobs typing, not by a check. |
| A14 | baseline | `console::output::check` exercises it but asserts ring behaviour, and it is check code in a product file (L.console.9/.10 wait in the baseline for the same reason). |
| A15–A18 | baseline | STM32 USART: DK1 only, and no gate boots the board. |

Summary: 18 requirements, 2 proposed verified (A2 by `arch::armv7a::check::check_masking`, A7 by `arch::armv7a::check::check_refusals`), plus A4 proposed verified by the generic `smp::check::everywhere`. That makes 3 verified and 15 baseline.

## Check code in product files

| unit | reason |
|---|---|
| `arch::arm_common::gicv2::idle_line_for_check` | Finds the highest line nothing has enabled, for the ARMv7-A and AArch64 machine checks' masking case. It needs the distributor window private to gicv2.rs. |
| `arch::arm_common::gicv2::enabled_for_check` (not in my list; the gate classed it as an accessor) | Reads a line's set-enable bit back for the same masking case. It is listed for consistency with `arch::aarch64::gic::enabled_for_check`, which 19's JSON already lists. |

## Notes

**Decisions for you**

1. **Units in more than one requirement.** The brief says each unit appears in exactly one requirement, but rule 1 (name every function that carries the behaviour) plus rule 2 (split by what one check proves) forces a few repeats. 19 does the same with `mask_interrupt` in both L.aarch64.16 and .17. The repeats:
   - `gicv2::enable` in A2, A3 and A5
   - `gicv2::disable` in A2 and A5
   - `gicv2::init_this_cpu` in A4 and A5
   - `gicv2::init_msi_frame` in A7 and A8

   If you need exactly one, drop the repeats from the baseline requirements (A3, A5, A8). A5 would then name `init_this_cpu` only, with a note that enable and disable do the recording.

2. **Merging with the facade (file 24 area).** `arch::armv7a::mask_interrupt`/`unmask_interrupt` (the facade) plus A2 make the analogue of L.aarch64.16. The facade's `service_interrupts` plus A6's `claim` (and `gicv2::complete`, an accessor) make L.aarch64.18. The facade's `send_ipi_to_others` plus `gicv2::send_sgi_to_others` (an accessor) plus A4 make L.aarch64.19. You may want to merge them into single requirements when you renumber.

3. **`this_cpu_target`.** It is not in my list (the gate presumably classed it as one expression), but it carries A3's "this core's bit". Add it to A3's unit if you name accessors that are a behaviour's core.

4. **Parents.** A5 uses H.BOOT.5 (every processor comes up). The stage-5 failure it prevents is a core never preempted, which is arguably H.SCHED.1 instead; your call. A12 and A16 use H.BOOT.6 (powered off after the console has drained).

**Possible bugs (reported, not fixed)**

1. **Unlocked read-modify-writes in gicv2.rs.** `enable` rewrites a whole GICD_IPRIORITYR word and a whole GICD_ITARGETSR word (4 lines each). `set_edge_triggered` rewrites a whole GICD_ICFGR word (16 lines). None of them takes a lock.
   - If two cores enable, for the first time, two SPIs in the same targets word (for example Interrupt objects unmasked on different cores), one can write back the other's still-zero target byte. That line is then enabled but delivered nowhere.
   - Two concurrent `msi_allocate` calls whose SPIs share an ICFGR word can lose one edge bit. That MSI would then be silently lost (the file's own comment says a level-configured v2m SPI loses its pulse).
   - I did not find a lock in the callers (`armv7a::unmask_interrupt`, `enable_console_receive`, `device.rs`'s `minted` lock is per device).
2. **Private lines enabled after the secondaries are up.** `enable` and `disable` on a private line act on the calling core's bank only. `PRIVATE_ENABLED` is global, so a core that comes up later follows it, but already-running cores do not. A `disable` on one core also clears the record while the other cores keep the line enabled.
3. **The doorbell assertion in `check_refusals` cannot fail.** `msi_doorbell().is_some_and(|page| !page.is_multiple_of(0x1000))` tests a value `msi_doorbell` has already masked with `& !0xFFF`. The check nevertheless counts it as one of its "4 requests refused" (`Ok(4)` counts three refusals plus this). AArch64's version returns `refused = 4` for the three calls and does not do the doorbell assertion.
4. **`check_masking` does not leave its SPI as it found it.** The file header says the lines are "left that way", but `enable` gives the idle SPI a target byte (when it was zero) and a priority, and `disable` restores neither. Only the enable bit goes back. This is harmless but not what the header says.
5. **`msi_allocate` can leak an SPI.** It sets the SPI's taken bit before `gicv2m_message` can fail (address overflow), and never clears it on that error. This is practically unreachable.
6. **`gicv2::init` can leak the distributor mapping.** If the CPU interface fails to map, the already-mapped distributor stays mapped (the boot fails anyway).
7. **`configure` writes reserved priority registers.** It writes GICD_IPRIORITYR for identifiers 1020–1023 when GICD_TYPER reports 1024 lines. Those registers are reserved and write-ignored, so this is harmless.
8. **Wrong SAFETY comment on `Base` in pl011.rs and stm32_usart.rs.** The `Sync` comment says "early boot is single-threaded", but `BASE` is read from every core and from interrupt handlers for the life of the machine. The code is sound only because `init` is the sole writer and runs before any other core starts. Consider rewording the comment.
9. **Write after timeout.** After the bounded wait expires, both ports' `write_byte` writes to the data register anyway. On a full PL011 FIFO that byte is silently dropped. This is the bounded-wait design, but no requirement says that a byte may be lost.
10. **The "nothing before init" guard is untested.** Every port function returns `None`, 0 or `false` before `init`. No gate can test this after boot, so I left it out of the statements; say if you want it stated.

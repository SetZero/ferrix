/*
 * Ferrix — SysML v2 model, part 8: devices and userspace drivers.
 *
 * docs/ARCHITECTURE.md §7. The kernel enumerates buses because that needs
 * ACPI or a device tree and privileged access; it does not drive devices.
 * Enumeration's parsers and the kernel's access to the tables run today;
 * everything from the device node outward is stage 10.
 */
package FerrixDrivers {
    private import ScalarValues::*;
    private import FerrixLifecycle::*;
    private import FerrixObjects::*;
    private import FerrixMemory::Vmo;

    // -----------------------------------------------------------------------
    // Reaching the machine's description, today
    // -----------------------------------------------------------------------

    #implemented part def AcpiAccess {
        @stage { number = 3; }
        doc /* kernel/src/acpi.rs: the one place a physical address firmware
             * wrote becomes a reference the parser reads, through the direct
             * map, refusing addresses or lengths outside it. libs/acpi never
             * dereferences a pointer: RSDP, XSDT/RSDT, MADT, FADT fixed
             * fields, GTDT, HPET. No AML. */
        action open;
        action madt;
        action fadt;
        action gtdt;
        action hpet;
    }

    #implemented part def FdtAccess {
        @stage { number = 1; }
        doc /* kernel/src/fdt.rs: the loader's copy of firmware's tree, read
             * through the direct map from DeviceTree memory that nothing
             * reclaims, so the borrow is honestly 'static. libs/fdt: nodes,
             * properties, reg, interrupts, compatible, stdout-path, the
             * interrupt controller, the timer, /cpus, the PSCI conduit. */
        action open;
        action console;
        action interruptController;
        action timer;
        action cpus;
        action psci;
    }

    #implemented part def MmioWindows {
        @stage { number = 3; }
        doc /* kernel/src/mmio.rs: the one place that says read_volatile and
             * write_volatile. A window with no base reads zero and discards
             * writes, so "never mapped" is a value rather than a null
             * dereference. */
        action read32;
        action write32;
    }

    // -----------------------------------------------------------------------
    // Stage 10
    // -----------------------------------------------------------------------

    #inProgress part def DeviceNode {
        @stage { number = 10; }
        doc /* What the kernel creates per device found, and hands devmgr a
             * handle to. kernel/src/device.rs: one per PCI function and per
             * virtio,mmio tree node, published at boot. Apertures and vectors
             * are Aperture and Vector tokens only this module mints, and
             * IoMapping and Interrupt take those rather than numbers, so a
             * driver cannot name memory or an interrupt its device does not
             * have. The pages of a PCI function's MSI-X table and pending-bit
             * array are withheld from its apertures. Missing: PCI vectors
             * (programming MSI-X). */
        attribute compatible : String;
        attribute mmioWindows : Natural[0..*];
        attribute interrupts : Natural[0..*];
    }

    #inProgress part def DeviceEnumeration {
        @stage { number = 10; }
        doc /* ACPI on x86-64 and AArch64 under EDK2, device tree on ARMv7-A
             * and where AArch64 firmware offers one; PCIe bus walk from
             * either. kernel/src/pci.rs: MCFG (libs/acpi) or
             * pci-host-ecam-generic (libs/fdt), ECAM mapped a bus at a time,
             * the libs/pci walk with every BAR sized and every capability
             * list walked, in the boot test on all three architectures.
             * Device nodes are built from what it finds: see DeviceNode. */
        part nodes : DeviceNode[0..*];
        action enumerate;
    }

    #planned part def IommuDomain {
        @stage { number = 10; }
        doc /* Scoped to one device: the device addresses a DMA VMO gets come
             * from here, so a driver cannot DMA over the kernel or over
             * another driver. */
        ref part device : DeviceNode;
        action mapForDevice;
        action unmapForDevice;
    }

    enum def IommuMode {
        Enforced;   // VT-d, AMD-Vi, SMMUv3
        DegradedTrusted;  // no IOMMU: announced loudly at boot
    }

    #planned part def IommuDomains {
        @stage { number = 10; }
        attribute mode : IommuMode;
        part domains : IommuDomain[0..*];
    }

    #planned part def DriverProcess {
        @stage { number = 10; }
        doc /* An ordinary user process in its own Job, holding exactly the
             * capabilities devmgr gave it. A driver fault is a process fault;
             * a wedged driver is a Job kill. */
        ref part job : Job;
        part ioMappings : IoMapping[0..*] {
            doc /* One per BAR or MMIO window, and nothing outside it. */
        }
        part interrupts : Interrupt[0..*] {
            doc /* One per vector, bound to a Port. */
        }
        part eventPort : Port;
        ref part dmaBuffers : Vmo[0..*] {
            doc /* Device addresses from the device's IOMMU domain. */
        }
        part channel : Channel {
            doc /* To the kernel subsystem it serves: block, net, input. */
        }
        part ring : SharedRing;
    }

    #planned part def SharedRing {
        @stage { number = 10; }
        doc /* The data path is not per-request IPC: driver and kernel share
             * a descriptor ring in a VMO and ring a doorbell; requests batch.
             * The same shape virtio and NVMe already use. libs/virtio's split
             * virtqueue (driver and device halves, over abstract shared
             * memory) is the first of these. */
        ref part memory : Vmo;
        action addChain;
        action takeUsed;
        action doorbell;
    }

    #planned part def DevMgr {
        @stage { number = 10; }
        doc /* A normal musl binary that also speaks the native ABI. Receives a
             * handle per device node, matches a driver, spawns it in its own
             * Job with the four resources above. */
        action match;
        action spawnDriver;
    }

    #planned part def VirtioBlkDriver :> DriverProcess {
        @stage { number = 10; }
        doc /* The first driver: virtio-blk as a user process. Exit test: a
             * sector read through ring 3 with the IOMMU on, and a deliberate
             * out-of-domain DMA attempt faulting. */
    }

    #planned part def VirtioNetDriver :> DriverProcess;

    #planned action def DriverBootstrap {
        @stage { number = 10; }
        doc /* The loader places an initramfs in RAM holding devmgr, the
             * virtio-blk driver and init. The kernel mounts it as root,
             * starts init, drivers come up, and the system pivots onto btrfs.
             * Linux's answer, for the same reason. */
        first start;
        then action mountInitramfsAsRoot;
        then action startInit;
        then action startDevMgr;
        then action spawnBlockDriver;
        then action mountBtrfs;
        then action pivotRoot;
        then done;
    }
}

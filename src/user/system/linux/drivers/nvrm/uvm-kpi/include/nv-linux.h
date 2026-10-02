/*
 * uvm-kpi's nv-linux.h.
 *
 * NVIDIA's own nv-linux.h is the Linux glue of RM: PCI, interrupts, ACPI,
 * Tegra clocks and GPIOs, in 1,852 lines. UVM includes it but uses seven
 * of its names (docs/NVIDIA.md §11.3). This header comes first on the
 * include path and gives UVM those seven, plus the NVIDIA headers the
 * original pulls in that UVM relies on, so none of RM's Linux glue is
 * compiled into UVM.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef _NV_LINUX_H_
#define _NV_LINUX_H_

#include "nvstatus.h"
#include "nv.h"
#include "conftest.h"
#include "nv-mm.h"
#include "nv-time.h"
#include "nv-list-helpers.h"
#include "nv-kthread-q.h"

#define NV_MAY_SLEEP() (!irqs_disabled() && !in_interrupt())
static inline NvBool nv_numa_node_has_memory(int node_id)
{
    return node_id == 0 ? NV_TRUE : NV_FALSE;
}

#define NV_PAGE_MASK ((NvU64)(long)PAGE_MASK)

static inline void *nv_ioremap_cache(NvU64 phys, NvU64 size)
{
    return ioremap_cache(phys, size);
}

static inline void nv_iounmap(void *ptr, NvU64 size)
{
    (void)size;
    iounmap(ptr);
}

/* NVIDIA's BAR index to the PCI BAR number: each 64-bit BAR before it
 * takes two. The 3060's (and every Ampere's) BAR0, BAR1 and BAR3 are
 * 32-bit, 64-bit and 64-bit (docs/NVIDIA.md §2.3). */
static inline NvU8 nv_bar_index_to_os_bar_index(struct pci_dev *dev, NvU8 nv_bar_index)
{
    static const NvU8 os_bar[] = { 0, 1, 3 };
    (void)dev;
    BUG_ON(nv_bar_index >= ARRAY_SIZE(os_bar));
    return os_bar[nv_bar_index];
}

static inline struct kmem_cache *nv_kmem_cache_create(const char *name, unsigned int size,
                                                      unsigned int align)
{
    return kmem_cache_create(name, size, align, 0, NULL);
}

#define NV_KMEM_CACHE_CREATE(name, type) nv_kmem_cache_create(name, sizeof(type), 0)

static inline void *nv_kmem_cache_zalloc(struct kmem_cache *k, gfp_t flags)
{
    return kmem_cache_zalloc(k, flags);
}

#endif

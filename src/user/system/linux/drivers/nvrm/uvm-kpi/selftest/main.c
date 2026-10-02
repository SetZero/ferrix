/*
 * uvm-selftest: NVIDIA's UVM, on uvm-kpi, with no GPU.
 *
 * C0a (docs/NVIDIA.md §11.5): before any client or card exists, run UVM's
 * own built-in tests through UVM's own ioctl path, to check the shim. The
 * program starts UVM as Linux's module loader would, opens its device as
 * a client would, initializes a VA space, and runs each test that needs no
 * GPU. It prints one line per test and exits 0 only if all passed.
 *
 * It stands in for nvrm until N1b exists; nvrm will run the same tests
 * from a test build.
 *
 * SPDX-License-Identifier: MIT
 */
#include "uvm_linux.h"
#include "uvm_common.h"
#include "uvm_ioctl.h"
#include "uvm_test_ioctl.h"
#include "uvm_linux_ioctl.h"

int kpi_module_init(void);
void kpi_module_exit(void);
void kpi_start_services(void);
struct file *kpi_open(dev_t dev);
long kpi_ioctl(struct file *f, unsigned int cmd, unsigned long arg);
void kpi_close(struct file *f);

static int failures;

#define RUN(cmd, params, check)                                                         \
    do {                                                                                \
        u64 t0 = kpi_monotonic_ns();                                                    \
        long r = kpi_ioctl(f, cmd, (unsigned long)&(params));                            \
        bool ok = r == 0 && (check);                                                    \
        printk("uvm-selftest: %-34s %s (ioctl %ld, status %s, %llu ms)\n", #cmd,        \
               ok ? "PASS" : "FAIL", r, nvstatusToString((params).rmStatus),            \
               (kpi_monotonic_ns() - t0) / 1000000ULL);                                 \
        if (!ok)                                                                        \
            failures++;                                                                 \
    } while (0)

#define SIMPLE(name)                                                                    \
    do {                                                                                \
        UVM_TEST_##name##_PARAMS p;                                                     \
        memset(&p, 0, sizeof(p));                                                       \
        RUN(UVM_TEST_##name, p, p.rmStatus == NV_OK);                                   \
    } while (0)

int main(void)
{
    static const char *params[][2] = {
        { "uvm_enable_builtin_tests", "1" },
        { "uvm_enable_va_space_mm", "0" },          /* §11.4: no client mm */
        { "uvm_perf_access_counter_migration_enable", "0" },
        { "uvm_cpu_chunk_allocation_sizes", "4096" }, /* §11.4: 4 KiB chunks */
    };
    UVM_INITIALIZE_PARAMS init;
    struct file *f;
    size_t i;
    int r;

    for (i = 0; i < ARRAY_SIZE(params); i++) {
        if (kpi_param_set(params[i][0], params[i][1]) != 0)
            printk("uvm-selftest: no parameter %s\n", params[i][0]);
    }
    kpi_start_services();

    r = kpi_module_init();
    printk("uvm-selftest: module init %d\n", r);
    if (r)
        return 1;

    /* UVM's region is the first one allocated: major 240, minor 0. */
    f = kpi_open(MKDEV(240, NVIDIA_UVM_PRIMARY_MINOR_NUMBER));
    if (IS_ERR(f)) {
        printk("uvm-selftest: open failed %ld\n", PTR_ERR(f));
        return 1;
    }
    memset(&init, 0, sizeof(init));
    RUN(UVM_INITIALIZE, init, init.rmStatus == NV_OK);

    SIMPLE(RNG_SANITY);
    SIMPLE(RANGE_TREE_DIRECTED);
    SIMPLE(LOCK_SANITY);
    SIMPLE(PERF_UTILS_SANITY);
    SIMPLE(KVMALLOC);
    SIMPLE(PERF_EVENTS_SANITY);
    SIMPLE(NV_KTHREAD_Q);
    SIMPLE(RB_TREE_DIRECTED);
    SIMPLE(CPU_CHUNK_API);
    {
        UVM_TEST_RANGE_ALLOCATOR_SANITY_PARAMS p = { .seed = 1, .iters = 10000 };
        RUN(UVM_TEST_RANGE_ALLOCATOR_SANITY, p, p.rmStatus == NV_OK);
    }
    {
        UVM_TEST_RB_TREE_RANDOM_PARAMS p = { .iterations = 100000, .range_max = 1ULL << 20,
                                             .node_limit = 4096, .seed = 1 };
        RUN(UVM_TEST_RB_TREE_RANDOM, p, p.rmStatus == NV_OK);
    }
    {
        UVM_TEST_RANGE_GROUP_TREE_PARAMS p;
        memset(&p, 0, sizeof(p));
        for (i = 0; i < ARRAY_SIZE(p.rangeGroupIds); i++) {
            UVM_CREATE_RANGE_GROUP_PARAMS g;
            memset(&g, 0, sizeof(g));
            RUN(UVM_CREATE_RANGE_GROUP, g, g.rmStatus == NV_OK);
            p.rangeGroupIds[i] = g.rangeGroupId;
        }
        RUN(UVM_TEST_RANGE_GROUP_TREE, p, p.rmStatus == NV_OK);
    }
    {
        UVM_TEST_THREAD_CONTEXT_SANITY_PARAMS p = { .iterations = 100 };
        RUN(UVM_TEST_THREAD_CONTEXT_SANITY, p, p.rmStatus == NV_OK);
    }
    {
        UVM_TEST_THREAD_CONTEXT_PERF_PARAMS p = { .iterations = 1000, .delay_us = 1 };
        RUN(UVM_TEST_THREAD_CONTEXT_PERF, p, p.rmStatus == NV_OK);
    }
    {
        UVM_TEST_GET_CPU_CHUNK_ALLOC_SIZES_PARAMS p;
        memset(&p, 0, sizeof(p));
        RUN(UVM_TEST_GET_CPU_CHUNK_ALLOC_SIZES, p, p.rmStatus == NV_OK && p.alloc_size_mask == PAGE_SIZE);
    }

    kpi_close(f);
    kpi_module_exit();
    printk("uvm-selftest: %s, %d failed\n", failures ? "FAIL" : "PASS", failures);
    return failures ? 1 : 0;
}

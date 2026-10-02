/*
 * uvm-kpi's uvm_common.c.
 *
 * NVIDIA's uvm_common.c is the one GPL-2.0-or-later file in nvidia-uvm
 * (docs/NVIDIA.md §11.2), so nvrm never links it. This file is written
 * for Ferrix from the contract in uvm_common.h, which is MIT: the debug and
 * assertion switches, the spin loop, and the mapping between NV_STATUS and
 * errno. It is compiled as a UVM unit, against uvm-kpi.
 *
 * SPDX-License-Identifier: MIT
 */
#include "uvm_common.h"
#include "uvm_linux.h"
#include "uvm_forward_decl.h"

/* A spinner yields every 10 µs of waiting and asks for a warning every
 * 30 s. */
#define SPIN_YIELD_NS (10 * 1000ULL)
#define SPIN_WARN_NS (30 * 1000 * 1000 * 1000ULL)

static int uvm_debug_prints = 0;
module_param(uvm_debug_prints, int, S_IRUGO | S_IWUSR);

int uvm_enable_builtin_tests = 0;
module_param(uvm_enable_builtin_tests, int, S_IRUGO);

int uvm_release_asserts = 1;
module_param(uvm_release_asserts, int, S_IRUGO | S_IWUSR);

int uvm_release_asserts_dump_stack = 0;
module_param(uvm_release_asserts_dump_stack, int, S_IRUGO | S_IWUSR);

int uvm_release_asserts_set_global_error = 0;
module_param(uvm_release_asserts_set_global_error, int, S_IRUGO | S_IWUSR);

bool uvm_release_asserts_set_global_error_for_tests = false;

bool uvm_debug_prints_enabled(void)
{
    return uvm_debug_prints != 0;
}

/* Hooks a debugger can break on. */
void on_uvm_test_fail(void)
{
    (void)NULL;
}

void on_uvm_assert(void)
{
    (void)NULL;
}

unsigned uvm_get_stale_process_id(void)
{
    return (unsigned)task_tgid_vnr(current);
}

unsigned uvm_get_stale_thread_id(void)
{
    return (unsigned)task_pid_vnr(current);
}

NV_STATUS uvm_spin_loop(uvm_spin_loop_t *spin)
{
    NvU64 now = NV_GETTIME();

    if (now - spin->start_time_ns >= SPIN_YIELD_NS)
        cond_resched();

    if (now - spin->print_time_ns >= SPIN_WARN_NS) {
        spin->print_time_ns = now;
        return NV_ERR_TIMEOUT_RETRY;
    }

    return NV_OK;
}

static const struct {
    int err;
    NV_STATUS status;
} errno_status[] = {
    { 0, NV_OK },
    { E2BIG, NV_ERR_INVALID_ARGUMENT },
    { EINVAL, NV_ERR_INVALID_ARGUMENT },
    { EBUSY, NV_ERR_BUSY_RETRY },
    { EAGAIN, NV_ERR_BUSY_RETRY },
    { EFAULT, NV_ERR_INVALID_ADDRESS },
    { ENOMEM, NV_ERR_NO_MEMORY },
    { ENOENT, NV_ERR_OBJECT_NOT_FOUND },
    { ENODEV, NV_ERR_INVALID_DEVICE },
    { ENXIO, NV_ERR_MODULE_LOAD_FAILED },
    { EPERM, NV_ERR_INSUFFICIENT_PERMISSIONS },
    { EACCES, NV_ERR_INSUFFICIENT_PERMISSIONS },
    { EEXIST, NV_ERR_IN_USE },
    { ENOSPC, NV_ERR_NO_MEMORY },
    { ENOSYS, NV_ERR_NOT_SUPPORTED },
    { EOPNOTSUPP, NV_ERR_NOT_SUPPORTED },
    { ENOTSUPP, NV_ERR_NOT_SUPPORTED },
    { ETIMEDOUT, NV_ERR_TIMEOUT },
    { EINTR, NV_ERR_SIGNAL_PENDING },
    { ERESTARTSYS, NV_ERR_SIGNAL_PENDING },
    { EIO, NV_ERR_RC_ERROR },
    { ECANCELED, NV_ERR_ECC_ERROR },
    { EOVERFLOW, NV_ERR_OUT_OF_RANGE },
    { ERANGE, NV_ERR_OUT_OF_RANGE },
    { EBADF, NV_ERR_INVALID_ARGUMENT },
    { EBADFD, NV_ERR_INVALID_ARGUMENT },
    { EHWPOISON, NV_ERR_RESET_REQUIRED },
};

NV_STATUS errno_to_nv_status(int errnoCode)
{
    size_t i;
    int err = errnoCode < 0 ? -errnoCode : errnoCode;

    for (i = 0; i < ARRAY_SIZE(errno_status); i++) {
        if (errno_status[i].err == err)
            return errno_status[i].status;
    }
    return NV_ERR_GENERIC;
}

static const struct {
    NV_STATUS status;
    int err;
} status_errno[] = {
    { NV_OK, 0 },
    { NV_ERR_BUSY_RETRY, EAGAIN },
    { NV_ERR_INSUFFICIENT_PERMISSIONS, EPERM },
    { NV_ERR_GPU_IN_DEBUG_MODE, EPERM },
    { NV_ERR_INSUFFICIENT_RESOURCES, ENOSPC },
    { NV_ERR_INVALID_ADDRESS, EFAULT },
    { NV_ERR_INVALID_ARGUMENT, EINVAL },
    { NV_ERR_INVALID_DEVICE, ENODEV },
    { NV_ERR_INVALID_REQUEST, EINVAL },
    { NV_ERR_INVALID_STATE, EINVAL },
    { NV_ERR_IN_USE, EBUSY },
    { NV_ERR_MODULE_LOAD_FAILED, ENXIO },
    { NV_ERR_NOT_SUPPORTED, ENOSYS },
    { NV_ERR_NO_MEMORY, ENOMEM },
    { NV_ERR_OBJECT_NOT_FOUND, ENOENT },
    { NV_ERR_OUT_OF_RANGE, ERANGE },
    { NV_ERR_SIGNAL_PENDING, EINTR },
    { NV_ERR_TIMEOUT, ETIMEDOUT },
    { NV_ERR_ECC_ERROR, EIO },
    { NV_ERR_RC_ERROR, EIO },
    { NV_ERR_RESET_REQUIRED, EIO },
    { NV_ERR_UVM_ADDRESS_IN_USE, EADDRINUSE },
};

int nv_status_to_errno(NV_STATUS status)
{
    size_t i;

    for (i = 0; i < ARRAY_SIZE(status_errno); i++) {
        if (status_errno[i].status == status)
            return status_errno[i].err;
    }
    return EINVAL;
}

void uvm_uuid_string(char *buffer, const NvProcessorUuid *uuid)
{
    const NvU8 *b = uuid->uuid;
    int i, n = 0;

    for (i = 0; i < 16; i++) {
        n += snprintf(buffer + n, UVM_UUID_STRING_LENGTH - n, "%02x", b[i]);
        if (i == 3 || i == 5 || i == 7 || i == 9)
            buffer[n++] = '-';
    }
    buffer[n] = '\0';
}

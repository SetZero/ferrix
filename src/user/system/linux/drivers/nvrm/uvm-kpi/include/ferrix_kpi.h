/*
 * uvm-kpi: the Linux kernel interfaces NVIDIA's nvidia-uvm uses, provided
 * to a user-space process (nvrm) on Ferrix. docs/NVIDIA.md §11.3.
 *
 * Every UVM translation unit includes this header first (-include), and
 * every <linux/...> and <asm/...> header it names is an empty file beside
 * this one. Only what UVM uses is here, and it is written from the
 * interfaces' documented behaviour, not copied from Linux.
 *
 * Inline parts are freestanding C (compiler builtins only). Everything that
 * needs the C library or the system is a kpi_* or Linux-named function
 * implemented in ../kpi/, which is compiled as ordinary C.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef FERRIX_KPI_H
#define FERRIX_KPI_H

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdarg.h>

#include "kpi/base.h"
#include "kpi/sync.h"
#include "kpi/mm.h"
#include "kpi/misc.h"

#endif

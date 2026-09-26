/* Ferrousli: the architecture's own copy of musl 1.2.5's bits/ipcstat.h. */
#if defined(__x86_64__)
#include "x86_64/ipcstat.h"
#elif defined(__aarch64__)
#include "aarch64/ipcstat.h"
#elif defined(__arm__)
#include "arm/ipcstat.h"
#else
#error "ferrousli supports x86-64, AArch64 and ARMv7-A"
#endif

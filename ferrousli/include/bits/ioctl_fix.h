/* Ferrousli: the architecture's own copy of musl 1.2.5's bits/ioctl_fix.h. */
#if defined(__x86_64__)
#include "x86_64/ioctl_fix.h"
#elif defined(__aarch64__)
#include "aarch64/ioctl_fix.h"
#elif defined(__arm__)
#include "arm/ioctl_fix.h"
#else
#error "ferrousli supports x86-64, AArch64 and ARMv7-A"
#endif

/* Ferrousli: the architecture's own copy of musl 1.2.5's bits/limits.h. */
#if defined(__x86_64__)
#include "x86_64/limits.h"
#elif defined(__aarch64__)
#include "aarch64/limits.h"
#elif defined(__arm__)
#include "arm/limits.h"
#else
#error "ferrousli supports x86-64, AArch64 and ARMv7-A"
#endif

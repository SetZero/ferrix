/* Ferrousli: the architecture's own copy of musl 1.2.5's bits/msg.h. */
#if defined(__x86_64__)
#include "x86_64/msg.h"
#elif defined(__aarch64__)
#include "aarch64/msg.h"
#elif defined(__arm__)
#include "arm/msg.h"
#else
#error "ferrousli supports x86-64, AArch64 and ARMv7-A"
#endif

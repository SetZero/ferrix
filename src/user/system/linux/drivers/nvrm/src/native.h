/*
 * The native calls nvrm makes, and the layouts they read and write
 * (src/lib/proto/native-abi). x86-64 only, as nvrm is.
 *
 * A process may make Linux and native calls alike: the kernel picks the
 * table by the number, 0x1000 to 0x1FFF being native
 * (docs/ARCHITECTURE.md §2). The convention is Linux's: SYSCALL, the
 * number in RAX, the arguments in RDI, RSI, RDX, R10, R8 and R9, the result
 * in RAX, an error as -errno in -4095..-1. Some calls write words back into
 * RSI, RDX and R10, so all three are treated as clobbered.
 *
 * The numbers are the ABI's own. `cargo xtask check`'s host tests read this
 * file and compare every NV_* number with ferrix_native_abi's
 * (tools/common/xtask/src/nvrm/tests.rs), so the two cannot drift.
 */
#ifndef NVRM_NATIVE_H
#define NVRM_NATIVE_H

#include <stdint.h>

/* Call numbers (ferrix_native_abi::nr). */
#define NV_HANDLE_CLOSE 0x1000
#define NV_OBJECT_WAIT_ONE 0x1008
#define NV_CHANNEL_READ 0x1012
#define NV_IO_MAPPING_CREATE 0x1040
#define NV_IO_MAPPING_MAP 0x1041
#define NV_DEVICE_INFO 0x1049
#define NV_DEVICE_APERTURE 0x1054
#define NV_DEVICE_CONFIG_READ 0x1055
#define NV_DEVICE_GET_LIMIT 0x1058
#define NV_DEVICE_ISOLATION 0x1059

/* Signals (ferrix_native_abi::signals). */
#define NV_SIGNAL_READABLE 0x1
#define NV_SIGNAL_PEER_CLOSED 0x4

/* The most handles a channel message carries (CHANNEL_MAX_HANDLES). */
#define NV_CHANNEL_MAX_HANDLES 64

/* device_get_limit's limits and device_isolation's bits (types). */
#define NV_DEVICE_LIMIT_PIN_PAGES 0
#define NV_DEVICE_LIMIT_PIN_CEILING 1
#define NV_DEVICE_LIMIT_PIN_ROOM 2
#define NV_DEVICE_LIMIT_ISOLATED_INTERRUPTS 3
#define NV_DEVICE_ISOLATION_DMA_TRANSLATED 0x1
#define NV_DEVICE_ISOLATION_INTERRUPTS 0x2

/* ApertureInfo's flags (types). */
#define NV_APERTURE_PREFETCHABLE 0x1
#define NV_APERTURE_WHOLE_PAGES 0x2
#define NV_APERTURE_BAR_64 0x4

/* DeviceBlock, 16 bytes. */
struct nv_device_block {
	uint64_t phys;
	uint32_t offset;
	uint32_t length;
};

/* DeviceInfo, as device_info writes it: DEVICE_INFO_BYTES, 96. */
struct nv_device_info {
	struct nv_device_block common;
	struct nv_device_block notify;
	struct nv_device_block isr;
	struct nv_device_block device;
	uint32_t location;
	uint32_t class_code;
	uint32_t apertures;
	uint32_t vectors;
	uint32_t notify_off_multiplier;
	uint16_t vendor_id;
	uint16_t device_id;
	uint16_t msix_table_size;
	uint16_t virtio;
	uint16_t subsystem_vendor_id;
	uint16_t subsystem_id;
};
_Static_assert(sizeof(struct nv_device_info) == 96, "DEVICE_INFO_BYTES");

/* ApertureInfo, as device_aperture writes it: APERTURE_INFO_BYTES, 32. */
struct nv_aperture_info {
	uint64_t phys;
	uint64_t len;
	uint8_t bar;
	uint8_t flags;
	uint8_t reserved[6];
	uint64_t offset;
};
_Static_assert(sizeof(struct nv_aperture_info) == 32, "APERTURE_INFO_BYTES");

/* IoMappingSpec, which io_mapping_create reads. */
struct nv_io_mapping_spec {
	uint64_t phys;
	uint64_t len;
};

/* A native call: its result, or -errno. */
static inline long nv_call(long number, long a0, long a1, long a2, long a3,
			   long a4, long a5)
{
	register long r10 __asm__("r10") = a3;
	register long r8 __asm__("r8") = a4;
	register long r9 __asm__("r9") = a5;
	long result = number;
	__asm__ volatile("syscall"
			 : "+a"(result), "+S"(a1), "+d"(a2), "+r"(r10)
			 : "D"(a0), "r"(r8), "r"(r9)
			 : "rcx", "r11", "memory");
	return result;
}

/* Whether a result is an error. */
static inline int nv_failed(long result)
{
	return result < 0 && result >= -4095;
}

#endif

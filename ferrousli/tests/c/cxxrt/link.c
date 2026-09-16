/*
 * dl_iterate_phdr and dladdr in a static program: one object, the program,
 * whose headers hold the code calling them, with this thread's TLS block.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <elf.h>
#include <link.h>
#include <stdint.h>

#include "check.h"

static __thread int tls_value = 42;

struct seen {
	int calls;
	int code_covered;
	int tls_covered;
};

static int callback(struct dl_phdr_info *info, size_t size, void *data)
{
	struct seen *seen = data;
	uintptr_t code = (uintptr_t)&callback;
	uintptr_t tls = (uintptr_t)&tls_value;
	seen->calls++;
	CHECK(size >= sizeof *info);
	CHECK(info->dlpi_name != 0 && info->dlpi_name[0] == 0);
	CHECK(info->dlpi_phnum > 0);
	CHECK(info->dlpi_adds == 1 && info->dlpi_subs == 0);
	CHECK(info->dlpi_tls_modid == 1);
	for (int i = 0; i < info->dlpi_phnum; i++) {
		const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
		uintptr_t start = info->dlpi_addr + ph->p_vaddr;
		if (ph->p_type == PT_LOAD && code >= start && code - start < ph->p_memsz)
			seen->code_covered = 1;
		if (ph->p_type == PT_TLS) {
			uintptr_t block = (uintptr_t)info->dlpi_tls_data;
			if (tls >= block && tls - block < ph->p_memsz)
				seen->tls_covered = 1;
		}
	}
	return 7;
}

int main(void)
{
	struct seen seen = {0};
	CHECK(tls_value == 42);
	CHECK(dl_iterate_phdr(callback, &seen) == 7);
	CHECK(seen.calls == 1);
	CHECK(seen.code_covered);
	CHECK(seen.tls_covered);

	Dl_info info = {0};
	CHECK(dladdr(&callback, &info) != 0);
	CHECK(info.dli_fname != 0 && info.dli_fname[0] == 0);
	CHECK(info.dli_sname == 0 && info.dli_saddr == 0);
	CHECK(dladdr((void *)16, &info) == 0);
	return t_status;
}

/* A library that finds the C library's function after itself, as
   Chromium's localtime wrappers do: dlsym(RTLD_NEXT, ...), through the
   loader's interface with this library's own address as the caller, which
   is what the C library's dlsym passes. */

#include <stddef.h>

/* The loader's __ferrousli_loader, to revision 4: ld/src/interface.rs. */
struct loader {
	unsigned long version, tls_size, tls_align;
	void *init_tls, *run_program_init, *dlopen, *dlsym, *dlclose, *dlerror;
	void *dladdr, *dl_iterate_phdr, *auxv;
	void *(*dlsym_from)(void *handle, const char *name, const void *caller);
};

extern struct loader __ferrousli_loader;

int next_answer(void)
{
	int (*answer)(void);

	if (__ferrousli_loader.version < 4)
		return 92;
	answer = (int (*)(void))__ferrousli_loader.dlsym_from((void *)-1, "libc_answer",
							      (const void *)next_answer);
	return answer ? answer() : 91;
}

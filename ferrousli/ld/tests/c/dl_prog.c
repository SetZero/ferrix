/* dlopen, dlsym, dladdr, dladdr1, dlinfo, dl_iterate_phdr, dlerror and dlclose, called with
   no C library at all, so that what they prove is the loader's. LIBRARY is
   libgreet.so's path and TLS_LIBRARY a library with its own PT_TLS, both
   given on the command line. Each check exits with its own number. */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <link.h>

static int same(const char *a, const char *b) {
	while (*a && *a == *b) { a++; b++; }
	return *a == *b;
}

struct seen { void *base; int objects; int found; };

static int count(struct dl_phdr_info *info, size_t size, void *data) {
	struct seen *seen = data;
	(void)size;
	seen->objects++;
	if ((void *)info->dlpi_addr == seen->base && info->dlpi_phnum > 0)
		seen->found = 1;
	return 0;
}

/* Whether `dir` is the directory `path` is in: all of `path` up to its last
   slash. */
static int directory_of(const char *dir, const char *path) {
	const char *last = 0;
	for (const char *p = path; *p; p++)
		if (*p == '/')
			last = p;
	if (!last)
		return 0;
	for (; path < last; path++, dir++)
		if (*dir != *path)
			return 0;
	return *dir == 0;
}

/* Entered with the stack as the kernel left it, 16-aligned rather than 8 off
   as a called function expects, so GCC realigns it before calling anything
   that may keep aligned vectors on it. */
__attribute__((force_align_arg_pointer)) void _start(void) {
	long r = 42;
	void *h = dlopen(LIBRARY, RTLD_NOW);
	int (*greet)(void) = h ? (int (*)(void))dlsym(h, "greet") : 0;
	int *value = h ? dlsym(h, "greeting_value") : 0;
	Dl_info info;
	struct seen seen = {0, 0, 0};
	void *extra = 0;
	struct link_map *lm = 0;
	void *t;
	int *(*gd)(void);
	int (*bump)(void);
	char origin[4096];
	if (!h) {
		r = 91; /* not loaded */
	} else if (!greet || greet() != 42) {
		r = 92; /* not found, not relocated, or its constructor did not run */
	} else if (!value || *value != 40 || dlsym(RTLD_DEFAULT, "greeting_value") != value) {
		r = 93; /* a datum by handle and by the global scope disagree */
	} else if (!dladdr((void *)greet, &info) || !info.dli_sname || !same(info.dli_sname, "greet")) {
		r = 94; /* dladdr did not name the function */
	} else if (dlopen("/nonexistent/libnope.so", RTLD_NOW) || !dlerror() || dlerror()) {
		r = 95; /* a failure not reported once, then cleared */
	} else if (seen.base = info.dli_fbase, dl_iterate_phdr(count, &seen), seen.objects < 3 || !seen.found) {
		r = 96; /* dl_iterate_phdr missed the program, the loader or the library */
	} else if (dlopen(LIBRARY, RTLD_NOW) != h || dlopen(LIBRARY, RTLD_NOLOAD) != h) {
		r = 97; /* a second open is not the same handle */
	} else if (!(t = dlopen(TLS_LIBRARY, RTLD_NOW))
		|| !(gd = (int *(*)(void))dlsym(t, "gd_address"))
		|| !(bump = (int (*)(void))dlsym(t, "ld_bump"))
		|| *gd() != 40 || bump() != 6 || bump() != 7) {
		r = 98; /* a library with its own TLS did not open, or its variables
		           did not start at their image's values in this thread,
		           which was running before the library was loaded */
	} else if (!dladdr1((void *)greet, &info, &extra, RTLD_DL_LINKMAP) || !extra
		|| !same(((struct link_map *)extra)->l_name, LIBRARY)) {
		r = 86; /* dladdr1 gave no link map, or another object's */
	} else if (lm = extra, !dladdr1((void *)greet, &info, &extra, RTLD_DL_SYMENT) || !extra
		|| (char *)lm->l_addr + ((const ElfW(Sym) *)extra)->st_value != (char *)greet) {
		r = 87; /* dladdr1's symbol table entry is not the function's */
	} else if (dlinfo(h, RTLD_DI_LINKMAP, &lm) != 0 || !lm || !same(lm->l_name, LIBRARY) || !lm->l_ld) {
		r = 88; /* dlinfo's link map is not the library's */
	} else if (dlinfo(h, RTLD_DI_ORIGIN, origin) != 0 || !directory_of(origin, LIBRARY)) {
		r = 89; /* dlinfo's origin is not the library's directory */
	} else if (dlinfo(h, 12345, &lm) != -1 || !dlerror()) {
		r = 90; /* an unknown request did not fail with a reason */
	} else if (dlclose(h) != 0 || dlclose((void *)1) != -1) {
		r = 99; /* dlclose on a handle, and on something that is not one */
	}
	__asm__ volatile("syscall" :: "a"(231L), "D"(r) : "rcx", "r11", "memory");
	__builtin_unreachable();
}

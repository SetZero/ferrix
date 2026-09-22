/* What a program links against to call the loader's dlfcn.h: the names, and
   nothing behind them. It is linked with the loader's own file name as its
   SONAME, so the program's DT_NEEDED names the loader, which the loader
   recognises as itself and never maps this file; the calls bind to the
   loader's exports. glibc programs name ld-linux the same way. */

#include <stddef.h>

void *dlopen(const char *path, int flags) { (void)path; (void)flags; return NULL; }
void *dlsym(void *handle, const char *name) { (void)handle; (void)name; return NULL; }
int dlclose(void *handle) { (void)handle; return -1; }
char *dlerror(void) { return NULL; }
int dladdr(const void *address, void *info) { (void)address; (void)info; return 0; }
int dl_iterate_phdr(int (*callback)(void *, size_t, void *), void *data) {
	(void)callback; (void)data; return 0;
}

/* mmap for fixdep: a read into memory, since it only maps files to read. */
#ifndef HOSTCOMPAT_SYS_MMAN_H
#define HOSTCOMPAT_SYS_MMAN_H
#include <stdlib.h>
#include <io.h>

#define PROT_READ 1
#define MAP_PRIVATE 2
#define MAP_FAILED ((void *)-1)

static inline void *mmap(void *addr, size_t len, int prot, int flags, int fd, long off)
{
	(void)addr; (void)prot; (void)flags;
	char *p = malloc(len ? len : 1);
	size_t got = 0;
	if (!p || _lseek(fd, off, SEEK_SET) < 0)
		goto fail;
	while (got < len) {
		int n = _read(fd, p + got, (unsigned)(len - got));
		if (n <= 0)
			goto fail;
		got += (size_t)n;
	}
	return p;
fail:
	free(p);
	return MAP_FAILED;
}

static inline int munmap(void *addr, size_t len)
{
	(void)len;
	free(addr);
	return 0;
}
#endif

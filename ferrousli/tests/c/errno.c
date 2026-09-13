/* A failed call returns -1 and leaves the reason in errno. */

typedef unsigned long size_t;
typedef long ssize_t;

int *__errno_location(void);
ssize_t write(int fd, const void *buf, size_t count);

#define EBADF 9

int main(void)
{
	if (write(-1, "x", 1) != -1)
		return 1;
	if (*__errno_location() != EBADF)
		return 2;
	return 0;
}

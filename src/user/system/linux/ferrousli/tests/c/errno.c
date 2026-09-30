/* A failed call returns -1 and leaves the reason in errno. */

#include <errno.h>
#include <unistd.h>

int main(void)
{
	if (write(-1, "x", 1) != -1)
		return 1;
	if (errno != EBADF)
		return 2;
	return 0;
}

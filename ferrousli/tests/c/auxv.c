/*
 * getauxval reads what the kernel passed, and reports a missing key with
 * ENOENT. Exits zero, or with the number of the check that failed.
 */

#include <errno.h>
#include <sys/auxv.h>

int main(void)
{
	if (getauxval(AT_PAGESZ) != 4096)
		return 1;
	if (getauxval(AT_RANDOM) == 0)
		return 2;
	errno = 0;
	if (getauxval(12345) != 0 || errno != ENOENT)
		return 3;
	return 0;
}

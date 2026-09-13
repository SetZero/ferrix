/*
 * The stack protector's canary is in place before main, and a smashed stack
 * is caught. Built with -fstack-protector-all, so every function checks.
 *
 * With no arguments, exits zero if the canary at %fs:0x28 is set and its low
 * byte is zero. With "smash", overruns a buffer and must die of SIGABRT.
 */

#include <string.h>

unsigned long read_canary(void);
__asm__(".text\n"
	".globl read_canary\n"
	".type read_canary, @function\n"
	"read_canary:\n"
	"\tmovq %fs:0x28, %rax\n"
	"\tret\n");

/* Through a pointer, so the compiler cannot see the overrun and refuse it. */
static void *(*volatile memset_p)(void *, int, size_t) = memset;

__attribute__((noinline))
static void smash(void)
{
	char buf[8];
	memset_p(buf, 'x', 64);
}

int main(int argc, char **argv)
{
	unsigned long canary = read_canary();
	if (canary == 0)
		return 1;
	if ((canary & 0xff) != 0)
		return 2;
	if (argc > 1 && strcmp(argv[1], "smash") == 0) {
		smash();
		return 3;
	}
	return 0;
}

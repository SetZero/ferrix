/*
 * Thread-local variables small enough to share the control block's static
 * memory: an initialised one, a zeroed one and an over-aligned one each read
 * back what the TLS image holds, and keep what is written to them.
 * Exits zero, or with the number of the check that failed.
 */

#include <stdint.h>

_Thread_local int initialised = 42;
_Thread_local char zeroed[100];
_Thread_local _Alignas(64) long aligned = 7;

int main(void)
{
	if (initialised != 42)
		return 1;
	for (int i = 0; i < 100; i++)
		if (zeroed[i])
			return 2;
	if (aligned != 7)
		return 3;
	if ((uintptr_t)&aligned % 64)
		return 4;

	initialised = 5;
	zeroed[99] = 1;
	aligned = 9;
	if (initialised != 5 || zeroed[99] != 1 || aligned != 9)
		return 5;
	return 0;
}

/*
 * Thread-local storage too large for the control block's static memory, so
 * the library has to map it. Exits zero, or with the number of the check that
 * failed.
 */

_Thread_local int small = 3;
_Thread_local char large[8192];

int main(void)
{
	if (small != 3)
		return 1;
	for (int i = 0; i < 8192; i++)
		if (large[i])
			return 2;

	large[8191] = 1;
	small = 4;
	if (large[8191] != 1 || small != 4)
		return 3;
	return 0;
}

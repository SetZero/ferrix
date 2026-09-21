/* A library whose writable segment ends in a large .bss, with PAD bytes of
   initialised data before it. Where the file's part of the segment ends
   relative to a page depends on PAD, so building it at several sizes puts
   that end on both sides of the case the loader got wrong: a .bss page past
   the file that was left inaccessible. The array is global and written
   through a volatile pointer, so that the compiler can neither drop it nor
   work out the answer without it. */

char padding[PAD] = {1};
char big[3 * 4096];

int touch(void) {
	volatile char *at = big;
	int sum = 0;
	for (unsigned i = 0; i < sizeof big; i++) {
		at[i] = 1;
		sum += at[i];
	}
	return sum == (int)sizeof big ? 42 : 94;
}

/* A program reading its library's TLS variable by initial-exec, as a PIE
   does, and asking the library for the same variable by general-dynamic.
   The two answers are one address only if the loader gave both models the
   same module offset. */

extern __thread int gd_value;
extern int *gd_address(void);
extern int ld_bump(void);

void _start(void) {
	long r;
	if (*gd_address() != 40) {
		r = 91; /* the general-dynamic answer is not the image's value */
	} else if (gd_address() != &gd_value) {
		r = 92; /* the two models disagree about where it is */
	} else if (ld_bump() != 6 || ld_bump() != 7) {
		r = 93; /* the local-dynamic block is wrong, or not kept */
	} else {
		r = 42;
	}
	__asm__ volatile("syscall" :: "a"(231L), "D"(r) : "rcx", "r11", "memory");
	__builtin_unreachable();
}

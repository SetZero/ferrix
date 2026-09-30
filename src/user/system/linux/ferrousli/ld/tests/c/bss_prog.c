/* Calls into a library with a large .bss: 42 when every byte of it could be
   written, and a signal when a page of it was not mapped. */

extern int touch(void);

void _start(void) {
	long r = touch();
	__asm__ volatile("syscall" :: "a"(231L), "D"(r) : "rcx", "r11", "memory");
	__builtin_unreachable();
}

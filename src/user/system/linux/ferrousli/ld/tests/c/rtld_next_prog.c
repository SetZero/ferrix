/* Exits with what libnext.so's RTLD_NEXT lookup of the C library's
   libc_answer gives: 42 when it found it, 91 when it found nothing. The
   program names libdl.so.2 before libnext.so and libc.so.6 after it, as
   glibc programs do. */

extern int next_answer(void);

/* Entered by a jump with the stack at argc, sixteen-byte aligned, where a
   called function expects it eight off: realigned, since the loader's code
   the lookup reaches keeps SSE values on its stack. */
__attribute__((force_align_arg_pointer)) void _start(void) {
	long r = next_answer();
	__asm__ volatile("syscall" :: "a"(231L), "D"(r) : "rcx", "r11", "memory");
	__builtin_unreachable();
}

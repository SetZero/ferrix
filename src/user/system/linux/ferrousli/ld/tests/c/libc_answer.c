/* A stand-in C library for the RTLD_NEXT test: linked as libc.so.6, with no
   PT_INTERP, so the loader takes it for ferrousli's. */

int libc_answer(void) { return 42; }

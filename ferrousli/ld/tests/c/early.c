/* A library whose constructor asks for the auxiliary vector. The loader runs
   it before it enters the program, so before the program's
   __libc_start_main has recorded the vector: compiler-builtins' choice of
   AArch64's LSE atomics asks at that moment. getauxval resolves to the C
   library linked into the program, which must answer from the loader's copy.
   AT_PAGESZ is 6 in every architecture's headers. */

extern unsigned long getauxval(unsigned long type);

static unsigned long seen;

__attribute__((constructor)) static void early(void) { seen = getauxval(6); }

unsigned long early_page_size(void) { return seen; }

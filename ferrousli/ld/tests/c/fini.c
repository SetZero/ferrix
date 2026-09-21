/* A shared object whose destructor gives the loader's exit callback an
   observable job.  There is no C library here: write is the Linux syscall. */

__attribute__((destructor)) static void finished(void) {
	static const char message[] = "fini\n";
	__asm__ volatile("syscall"
			 :
			 : "a"(1L), "D"(1L), "S"(message), "d"(sizeof message - 1)
			 : "rcx", "r11", "memory");
}

int fini_ready(void) { return 42; }

/* A shared object's finaliser-array entry. It writes without libc, so its
   position relative to the legacy DT_FINI hook is observable. */

__attribute__((destructor)) static void finished(void) {
	static const char message[] = "array\n";
	__asm__ volatile("syscall"
			 :
			 : "a"(1L), "D"(1L), "S"(message), "d"(sizeof message - 1)
			 : "rcx", "r11", "memory");
}

int fini_ready(void) { return 42; }

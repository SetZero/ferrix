/* `crt1.o` starts this through Ferrousli's __libc_start_main.  Returning lets
   exit run this array before the loader callback it received in rdx. */

extern int fini_ready(void);

__attribute__((destructor)) static void finished(void) {
	static const char message[] = "main\n";
	__asm__ volatile("syscall"
			 :
			 : "a"(1L), "D"(1L), "S"(message), "d"(sizeof message - 1)
			 : "rcx", "r11", "memory");
}

int main(void) { return fini_ready(); }

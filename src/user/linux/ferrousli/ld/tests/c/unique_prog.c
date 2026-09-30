/* Reads a library's STB_GNU_UNIQUE variable: 42 when the program and the
   library agree on its address and it holds its initial value, 91 when
   they do not. A loader that does not take the binding for a definition
   stops before this runs, on an undefined symbol. */

extern int unique_value;
extern int *unique_address(void);

void _start(void) {
	long r = &unique_value == unique_address() && unique_value == 40 ? 42 : 91;
	__asm__ volatile("syscall" :: "a"(231L), "D"(r) : "rcx", "r11", "memory");
	__builtin_unreachable();
}

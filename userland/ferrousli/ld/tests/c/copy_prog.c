/* A position-independent program whose reference to the library's variable
   is a copy relocation, as glibc's environ is in every program built against
   it. It writes its copy; the library must read that copy, not its own.
   42 is the library seeing the write; 95 is the library reading a variable
   of its own that the program never wrote. */

extern int shared_word;
extern int read_word(void);

void _start(void) {
    shared_word = 7;
    long r = read_word() == 7 ? 42 : 95;
    __asm__ volatile("syscall" :: "a"(231L), "D"(r) : "rcx", "r11", "memory");
    __builtin_unreachable();
}

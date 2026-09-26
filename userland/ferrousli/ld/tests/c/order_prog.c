/* Names liborder_first before liborder_second, which needs it, so the
   loader loads first before second: running constructors in the reverse of
   load order would run second's first. 42 is dependencies first; 94 is the
   other way round. */

extern int second_saw(void);

void _start(void) {
    long r = second_saw() == 1 ? 42 : 94;
    __asm__ volatile("syscall" :: "a"(231L), "D"(r) : "rcx", "r11", "memory");
    __builtin_unreachable();
}

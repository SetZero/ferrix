/* The legacy DT_FINI function. It must run after DT_FINI_ARRAY, making the
   loader's finalisation order visible without a C library. */

void loader_fini(void) {
    static const char text[] = "fini\n";
    __asm__ volatile("syscall"
                     :
                     : "a"(1L), "D"(1L), "S"(text), "d"(sizeof text - 1)
                     : "rcx", "r11", "memory");
}

/* The program and its dependency both use the initial-exec TLS model.  There
   is no C runtime: a successful exit proves the loader installed %fs and
   wrote the dependency's R_X86_64_TPOFF64 relocation before _start ran. */

extern int tls_greet(void);

__thread int program_tls __attribute__((tls_model("initial-exec"))) = 7;

void _start(void) {
    int result;
    if (program_tls != 7) {
        result = 91; /* The program's PT_TLS image was not copied. */
    } else if (tls_greet() != 41) {
        result = 92; /* The library's image or its TPOFF relocation is wrong. */
    } else if (tls_greet() != 42) {
        result = 93; /* The library did not retain this thread's TLS value. */
    } else {
        result = 42;
    }
    __asm__ volatile("syscall" :: "a"(231L), "D"((long)result) : "rcx", "r11", "memory");
    __builtin_unreachable();
}

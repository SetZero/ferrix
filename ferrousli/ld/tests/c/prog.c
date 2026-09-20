/* A dynamically linked program with no C library at all, so that what it
   proves is the loader and nothing else.

   Each check exits with its own number, so a failure says which of the four
   kinds of relocation was not written rather than only that something was
   wrong. 42 is every one of them right. */

extern int greet(void);
extern int greeting_value;
extern const char *greet_name(void);

/* A pointer in this program's writable data to a function in the library:
   an R_*_GLOB_DAT the loader writes before _start runs. */
static int (*const indirect)(void) = greet;

void _start(void) {
    int r;
    if (greeting_value != 40) {
        r = 91; /* the library's datum, read from the program */
    } else if (indirect() != 42) {
        r = 92; /* a function pointer in the program's own data, and the
                   library's constructor having run before it was called */
    } else if (greet_name()[0] != 'l') {
        r = 93; /* a pointer into the library's own data */
    } else {
        r = greet(); /* through the procedure linkage table */
    }
    __asm__ volatile("syscall" :: "a"(231L), "D"((long)r) : "rcx", "r11", "memory");
    __builtin_unreachable();
}

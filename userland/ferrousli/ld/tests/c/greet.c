/* A shared library with something of every kind the loader has to relocate:
   a datum another object reads, a datum of its own reached through a pointer
   it has to relocate itself, a function called through the procedure linkage
   table, and a constructor that has to run before anything calls in. */

int greeting_value = 40;

static int constructed = 0;

__attribute__((constructor)) static void setup(void) { constructed = 2; }

int greet(void) { return greeting_value + constructed; }

/* Returns a pointer into this object's own read-only data, which is an
   R_*_RELATIVE inside the library. */
const char *greet_name(void) { return "libgreet"; }

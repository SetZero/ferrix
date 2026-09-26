/* A versioned library variable a program copies into itself, and a function
   that reads it through the library's GOT, as glibc's own code reads
   __environ. */

int shared_word = 5;

int read_word(void) { return shared_word; }

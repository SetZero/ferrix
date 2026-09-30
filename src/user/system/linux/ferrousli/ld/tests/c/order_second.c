/* A library whose constructor calls into the library it needs, and records
   whether that one's constructor had run. */

extern int first_ready(void);

static int saw = -1;

__attribute__((constructor)) static void setup(void) { saw = first_ready(); }

int second_saw(void) { return saw; }

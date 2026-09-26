/* A library another needs, whose constructor must run before that one's. */

static int ready;

__attribute__((constructor)) static void setup(void) { ready = 1; }

int first_ready(void) { return ready; }

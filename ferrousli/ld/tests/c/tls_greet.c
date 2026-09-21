/* An initial-exec TLS variable in a dependency.  It starts from the image the
   loader copied, then proves that it stays in this thread's library block. */

__thread int library_tls __attribute__((tls_model("initial-exec"))) = 40;

int tls_greet(void) { return ++library_tls; }

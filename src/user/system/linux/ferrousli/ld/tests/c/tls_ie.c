/* A library that reads its own TLS by the initial-exec model, as libglvnd's
   libGL.so.1 reads its dispatch table: nothing passes through
   __tls_get_addr, so a thread sees the image only if it was copied into its
   block before it looks. */

__thread int ie_value __attribute__((tls_model("initial-exec"))) = 40;

int ie_read(void) { return ie_value; }

/* A shared library's TLS, read the way -fPIC code reads it: gd_value by the
   general-dynamic model, through __tls_get_addr or, built with
   -mtls-dialect=gnu2, a TLS descriptor; ld_count, which only this library
   can name, by the local-dynamic model, whose module is the library's own. */

__thread int gd_value = 40;
static __thread int ld_count = 5;

int *gd_address(void) { return &gd_value; }

int ld_bump(void) { return ++ld_count; }

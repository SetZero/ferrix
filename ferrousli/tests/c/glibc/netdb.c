/*
 * glibc's reentrant protocol lookups and its resolver state: the answers
 * glibc 2.43 gives for a protocol found, not found, and a buffer too small;
 * and a state __res_ninit fills from the system's resolv.conf, whose name
 * servers and options Chrome reads, released by __res_nclose.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <netdb.h>
#include <netinet/in.h>
#include <string.h>

#include "check.h"

/* glibc's struct __res_state as far as this reads it; its own header
 * gives the rest. */
struct res_head {
	int retrans, retry;
	unsigned long options;
	int nscount;
	struct sockaddr_in nsaddr_list[3];
};

#ifndef __GLIBC__
/* glibc's, which musl's header does not declare. */
int getprotobyname_r(const char *, struct protoent *, char *, size_t, struct protoent **);
int getprotobynumber_r(int, struct protoent *, char *, size_t, struct protoent **);
#endif
struct __res_state *__res_state(void);
int __res_ninit(void *);
void __res_nclose(void *);
int res_nquery(void *, const char *, int, int, unsigned char *, int);

int main(void)
{
	struct protoent pe, *res = (struct protoent *)1;
	char buf[1024], small[8];
	/* Room and alignment for glibc's 568 bytes. */
	static _Alignas(16) unsigned char state[1024];
	struct res_head *head = (struct res_head *)state;

	CHECK(getprotobyname_r("tcp", &pe, buf, sizeof buf, &res) == 0 && res == &pe);
	CHECK(strcmp(pe.p_name, "tcp") == 0 && pe.p_proto == 6);
	CHECK(getprotobynumber_r(17, &pe, buf, sizeof buf, &res) == 0 && res == &pe);
	CHECK(strcmp(pe.p_name, "udp") == 0);
	CHECK(getprotobyname_r("no-such-protocol", &pe, buf, sizeof buf, &res) == 0 && res == NULL);
	CHECK(getprotobynumber_r(9999, &pe, buf, sizeof buf, &res) == 0 && res == NULL);
	CHECK(getprotobyname_r("tcp", &pe, small, sizeof small, &res) == ERANGE && res == NULL);

	CHECK(__res_state() != NULL);
	memset(state, 0, sizeof state);
	CHECK(__res_ninit(state) == 0);
	CHECK(head->options & 1); /* RES_INIT */
	CHECK(head->nscount >= 1 && head->nscount <= 3);
	CHECK(head->retrans > 0 && head->retry > 0);
	/* An IPv4 name server has its family set; an IPv6 one's slot is 0. */
	CHECK(head->nsaddr_list[0].sin_family == AF_INET || head->nsaddr_list[0].sin_family == 0);
	/* glibc's leaves RES_INIT set, and a second __res_ninit starts over. */
	__res_nclose(state);
	CHECK(head->options & 1);
	CHECK(__res_ninit(state) == 0 && head->nscount >= 1);
	__res_nclose(state);
	return t_status;
}

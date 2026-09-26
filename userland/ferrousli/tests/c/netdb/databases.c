/*
 * The legacy netdb.h lookups: gethostbyname, the services and protocols
 * databases, h_errno and hstrerror.
 *
 * `/etc/services` is the machine's, so what it holds is not asserted; what is
 * asserted is that a name it gives and the port that name gives agree, which
 * is true of any such file. `/etc/protocols` need not be there at all: the
 * library carries musl's table for when it is not.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <netdb.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/socket.h>

#include "check.h"

/* How many entries the null-ended array has. */
static int entries(char **list)
{
	int n = 0;
	if (!list)
		return -1;
	while (list[n])
		n++;
	return n;
}

static void hosts(void)
{
	struct hostent *h = gethostbyname("localhost");
	struct hostent entry, *found = 0;
	char buf[512];
	int err = 0;

	CHECK(h != 0);
	if (h) {
		CHECK(!strcmp(h->h_name, "localhost"));
		CHECK(h->h_addrtype == AF_INET);
		CHECK(h->h_length == 4);
		CHECK(entries(h->h_addr_list) == 1);
		CHECK(entries(h->h_aliases) >= 1);
		CHECK(!memcmp(h->h_addr, "\177\0\0\1", 4));
	}

	h = gethostbyname2("localhost", AF_INET6);
	CHECK(h != 0);
	if (h) {
		CHECK(h->h_addrtype == AF_INET6);
		CHECK(h->h_length == 16);
		CHECK(!memcmp(h->h_addr_list[0], &in6addr_loopback, 16));
	}

	/* A literal address is its own name. */
	h = gethostbyname("192.0.2.10");
	CHECK(h != 0);
	if (h)
		CHECK(!strcmp(h->h_name, "192.0.2.10"));

	/* The reentrant form fills the caller's buffer. */
	CHECK(gethostbyname_r("localhost", &entry, buf, sizeof buf, &found, &err) == 0);
	CHECK(found == &entry);
	CHECK(found && !strcmp(found->h_name, "localhost"));
	/* A buffer with no room says so and leaves the answer unset. */
	found = &entry;
	CHECK(gethostbyname_r("localhost", &entry, buf, 8, &found, &err) == ERANGE);
	CHECK(found == 0);

	/* A name that resolves to nothing sets h_errno. */
	h = gethostbyname("");
	CHECK(h == 0);
	CHECK(h_errno == HOST_NOT_FOUND);
	CHECK(!strcmp(hstrerror(HOST_NOT_FOUND), "Host not found"));
	CHECK(hstrerror(TRY_AGAIN) != hstrerror(NO_DATA));
	/* h_errno is an lvalue, as <netdb.h> defines it. */
	h_errno = NO_DATA;
	CHECK(h_errno == NO_DATA);
}

static void addresses(void)
{
	struct in_addr loopback;
	struct hostent *h;

	loopback.s_addr = htonl(INADDR_LOOPBACK);
	h = gethostbyaddr(&loopback, sizeof loopback, AF_INET);
	/* Whether 127.0.0.1 has a name depends on the machine's /etc/hosts, but
	 * an answer must be a well formed one. */
	if (h) {
		CHECK(h->h_addrtype == AF_INET);
		CHECK(h->h_length == 4);
		CHECK(entries(h->h_addr_list) == 1);
		CHECK(!memcmp(h->h_addr, &loopback, 4));
		CHECK(h->h_name && *h->h_name);
	}
	/* A length that does not match the family is refused. */
	CHECK(gethostbyaddr(&loopback, 5, AF_INET) == 0);
}

static void services(void)
{
	struct servent *se = getservbyname("http", "tcp");

	/* The machine may have no /etc/services; if it has one, what it says
	 * about a name and about that name's port must agree. */
	if (se) {
		struct servent *back;
		int port = se->s_port;
		CHECK(!strcmp(se->s_name, "http"));
		CHECK(!strcmp(se->s_proto, "tcp"));
		CHECK(entries(se->s_aliases) >= 1);
		back = getservbyport(port, "tcp");
		CHECK(back != 0);
		if (back) {
			CHECK(back->s_port == port);
			CHECK(!strcmp(back->s_proto, "tcp"));
			CHECK(back->s_name && *back->s_name);
		}
	}
	/* A number is a port, not a service record. */
	CHECK(getservbyname("80", "tcp") == 0);
	/* A protocol that is neither tcp nor udp is refused. */
	CHECK(getservbyname("http", "sctp") == 0);
	/* A service name nothing knows. */
	CHECK(getservbyname("ferrousli-none", "tcp") == 0);

	/* The cursor opens, reads and closes without crashing, and every entry
	 * it gives is well formed. */
	setservent(1);
	for (int seen = 0; seen < 4; seen++) {
		struct servent *entry = getservent();
		if (!entry)
			break;
		CHECK(entry->s_name && *entry->s_name);
		CHECK(entry->s_proto && *entry->s_proto);
		CHECK(entries(entry->s_aliases) >= 0);
	}
	endservent();
}

static void protocols(void)
{
	struct protoent *pe = getprotobyname("tcp");
	int seen = 0;

	/* This one is answered from the table built into the library when the
	 * machine has no /etc/protocols, so it is always there. */
	CHECK(pe != 0);
	if (pe) {
		CHECK(pe->p_proto == IPPROTO_TCP);
		CHECK(!strcmp(pe->p_name, "tcp"));
		CHECK(entries(pe->p_aliases) >= 0);
	}
	pe = getprotobynumber(IPPROTO_UDP);
	CHECK(pe != 0);
	if (pe)
		CHECK(!strcmp(pe->p_name, "udp"));
	CHECK(getprotobyname("ferrousli-none") == 0);

	setprotoent(0);
	while (getprotoent()) {
		if (++seen > 8)
			break;
	}
	CHECK(seen > 0);
	endprotoent();
}

static void the_rest(void)
{
	int seen = 0;

	/* The hosts and networks cursors answer, or answer nothing, without
	 * crashing; what they hold is the machine's. */
	sethostent(1);
	while (gethostent()) {
		if (++seen > 4)
			break;
	}
	endhostent();

	setnetent(1);
	for (int n = 0; n < 4; n++) {
		struct netent *ne = getnetent();
		if (!ne)
			break;
		CHECK(ne->n_name && *ne->n_name);
		CHECK(ne->n_addrtype == AF_INET);
	}
	endnetent();
	CHECK(getnetbyname("ferrousli-none") == 0);
	CHECK(getnetbyaddr(0xfffffffe, AF_INET) == 0);
}

int main(void)
{
	hosts();
	addresses();
	services();
	protocols();
	the_rest();
	return t_status;
}

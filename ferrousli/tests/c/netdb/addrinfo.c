/*
 * getaddrinfo, getnameinfo, freeaddrinfo and gai_strerror, and the two
 * in6_addr objects <netinet/in.h> declares.
 *
 * Nothing here needs a network or a file: every lookup is of a numeric
 * address, a numeric service, or `localhost`, which RFC 6761 says a resolver
 * answers itself.
 */

#define _GNU_SOURCE

#include <netdb.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/socket.h>

#include "check.h"

/* How many results the list holds. */
static int count(struct addrinfo *list)
{
	int n = 0;
	for (struct addrinfo *p = list; p; p = p->ai_next)
		n++;
	return n;
}

/* Whether any result is of this family. */
static int has_family(struct addrinfo *list, int family)
{
	for (struct addrinfo *p = list; p; p = p->ai_next)
		if (p->ai_family == family)
			return 1;
	return 0;
}

static void numeric_address(void)
{
	struct addrinfo hint, *list = 0;
	struct sockaddr_in *sin;

	memset(&hint, 0, sizeof hint);
	hint.ai_family = AF_INET;
	hint.ai_socktype = SOCK_STREAM;
	hint.ai_flags = AI_NUMERICHOST | AI_NUMERICSERV;
	CHECK(getaddrinfo("192.0.2.10", "8080", &hint, &list) == 0);
	CHECK(count(list) == 1);
	if (list) {
		CHECK(list->ai_family == AF_INET);
		CHECK(list->ai_socktype == SOCK_STREAM);
		CHECK(list->ai_protocol == IPPROTO_TCP);
		CHECK(list->ai_addrlen == sizeof(struct sockaddr_in));
		sin = (struct sockaddr_in *)list->ai_addr;
		CHECK(sin->sin_family == AF_INET);
		CHECK(sin->sin_port == htons(8080));
		CHECK(sin->sin_addr.s_addr == htonl(0xc000020a));
		CHECK(list->ai_canonname != 0);
		CHECK(!strcmp(list->ai_canonname, "192.0.2.10"));
	}
	freeaddrinfo(list);

	/* An IPv6 literal, and a service given as a number only. */
	list = 0;
	memset(&hint, 0, sizeof hint);
	hint.ai_family = AF_INET6;
	hint.ai_flags = AI_NUMERICHOST;
	CHECK(getaddrinfo("2001:db8::1", "53", &hint, &list) == 0);
	/* One for each socket type, since the hint named none. */
	CHECK(count(list) == 2);
	if (list) {
		struct sockaddr_in6 *sin6 = (struct sockaddr_in6 *)list->ai_addr;
		CHECK(list->ai_addrlen == sizeof(struct sockaddr_in6));
		CHECK(sin6->sin6_family == AF_INET6);
		CHECK(sin6->sin6_port == htons(53));
		CHECK(sin6->sin6_addr.s6_addr[0] == 0x20);
		CHECK(sin6->sin6_addr.s6_addr[1] == 0x01);
		CHECK(sin6->sin6_addr.s6_addr[15] == 1);
	}
	freeaddrinfo(list);

	/* A name that is not numeric, with AI_NUMERICHOST, resolves to nothing. */
	list = 0;
	memset(&hint, 0, sizeof hint);
	hint.ai_flags = AI_NUMERICHOST;
	CHECK(getaddrinfo("nothing.example.test", "80", &hint, &list) == EAI_NONAME);
	/* Neither a host nor a service is nothing to look up. */
	CHECK(getaddrinfo(0, 0, 0, &list) == EAI_NONAME);
	/* A family the library does not have. */
	memset(&hint, 0, sizeof hint);
	hint.ai_family = 99;
	CHECK(getaddrinfo("192.0.2.10", 0, &hint, &list) == EAI_FAMILY);
	/* A flag that is not one of the standard's. */
	memset(&hint, 0, sizeof hint);
	hint.ai_flags = 0x8000;
	CHECK(getaddrinfo("192.0.2.10", 0, &hint, &list) == EAI_BADFLAGS);
}

static void localhost(void)
{
	struct addrinfo hint, *list = 0;

	memset(&hint, 0, sizeof hint);
	hint.ai_socktype = SOCK_STREAM;
	hint.ai_flags = AI_CANONNAME | AI_NUMERICSERV;
	CHECK(getaddrinfo("localhost", "80", &hint, &list) == 0);
	CHECK(count(list) >= 1);
	CHECK(has_family(list, AF_INET));
	CHECK(has_family(list, AF_INET6));
	for (struct addrinfo *p = list; p; p = p->ai_next) {
		CHECK(p->ai_socktype == SOCK_STREAM);
		CHECK(p->ai_canonname && !strcmp(p->ai_canonname, "localhost"));
		if (p->ai_family == AF_INET) {
			struct sockaddr_in *sin = (struct sockaddr_in *)p->ai_addr;
			CHECK(sin->sin_addr.s_addr == htonl(INADDR_LOOPBACK));
			CHECK(sin->sin_port == htons(80));
		} else {
			struct sockaddr_in6 *sin6 = (struct sockaddr_in6 *)p->ai_addr;
			CHECK(!memcmp(&sin6->sin6_addr, &in6addr_loopback, 16));
			CHECK(sin6->sin6_port == htons(80));
		}
	}
	freeaddrinfo(list);

	/* AI_PASSIVE and no name gives the wildcard addresses. */
	list = 0;
	memset(&hint, 0, sizeof hint);
	hint.ai_family = AF_INET6;
	hint.ai_socktype = SOCK_DGRAM;
	hint.ai_flags = AI_PASSIVE | AI_NUMERICSERV;
	CHECK(getaddrinfo(0, "9", &hint, &list) == 0);
	CHECK(count(list) == 1);
	if (list) {
		struct sockaddr_in6 *sin6 = (struct sockaddr_in6 *)list->ai_addr;
		CHECK(!memcmp(&sin6->sin6_addr, &in6addr_any, 16));
		CHECK(list->ai_protocol == IPPROTO_UDP);
	}
	freeaddrinfo(list);
	/* Freeing nothing is allowed here, where musl would crash. */
	freeaddrinfo(0);
}

static void back_again(void)
{
	struct sockaddr_in sin;
	struct sockaddr_in6 sin6;
	char node[NI_MAXHOST], serv[NI_MAXSERV];

	memset(&sin, 0, sizeof sin);
	sin.sin_family = AF_INET;
	sin.sin_port = htons(8080);
	sin.sin_addr.s_addr = htonl(0xc000020a);
	CHECK(getnameinfo((struct sockaddr *)&sin, sizeof sin, node, sizeof node,
		serv, sizeof serv, NI_NUMERICHOST | NI_NUMERICSERV) == 0);
	CHECK(!strcmp(node, "192.0.2.10"));
	CHECK(!strcmp(serv, "8080"));

	memset(&sin6, 0, sizeof sin6);
	sin6.sin6_family = AF_INET6;
	sin6.sin6_port = htons(53);
	sin6.sin6_addr.s6_addr[0] = 0x20;
	sin6.sin6_addr.s6_addr[1] = 0x01;
	sin6.sin6_addr.s6_addr[2] = 0x0d;
	sin6.sin6_addr.s6_addr[3] = 0xb8;
	sin6.sin6_addr.s6_addr[15] = 1;
	CHECK(getnameinfo((struct sockaddr *)&sin6, sizeof sin6, node, sizeof node,
		serv, sizeof serv, NI_NUMERICHOST | NI_NUMERICSERV) == 0);
	CHECK(!strcmp(node, "2001:db8::1"));
	CHECK(!strcmp(serv, "53"));

	/* A buffer too small for the text says so rather than truncating. */
	CHECK(getnameinfo((struct sockaddr *)&sin, sizeof sin, node, 4,
		0, 0, NI_NUMERICHOST) == EAI_OVERFLOW);
	/* A length short of the family's structure is refused. */
	CHECK(getnameinfo((struct sockaddr *)&sin, sizeof sin - 1, node, sizeof node,
		0, 0, NI_NUMERICHOST) == EAI_FAMILY);
	/* Neither a node nor a service asked for is no work and no error. */
	CHECK(getnameinfo((struct sockaddr *)&sin, sizeof sin, 0, 0, 0, 0,
		NI_NUMERICHOST) == 0);
}

static void errors(void)
{
	static const int codes[] = {
		EAI_BADFLAGS, EAI_NONAME, EAI_AGAIN, EAI_FAIL, EAI_NODATA,
		EAI_FAMILY, EAI_SOCKTYPE, EAI_SERVICE, EAI_MEMORY, EAI_SYSTEM,
		EAI_OVERFLOW,
	};
	const char *unknown = gai_strerror(1);

	CHECK(unknown && *unknown);
	for (unsigned i = 0; i < sizeof codes / sizeof *codes; i++) {
		const char *text = gai_strerror(codes[i]);
		CHECK(text && *text);
		CHECK(strcmp(text, unknown) != 0);
	}
	CHECK(!strcmp(gai_strerror(EAI_NONAME), "Name does not resolve"));
	/* Two calls give the same static storage, not a copy. */
	CHECK(gai_strerror(EAI_AGAIN) == gai_strerror(EAI_AGAIN));
}

static void wildcards(void)
{
	static const unsigned char zero[16] = { 0 };
	static const unsigned char one[16] = { [15] = 1 };
	const struct in6_addr *any = &in6addr_any;

	CHECK(!memcmp(&in6addr_any, zero, 16));
	CHECK(!memcmp(&in6addr_loopback, one, 16));
	/* They are objects, so their address may be taken and passed on. */
	CHECK(any->s6_addr[0] == 0);
	CHECK(IN6_IS_ADDR_UNSPECIFIED(&in6addr_any));
	CHECK(IN6_IS_ADDR_LOOPBACK(&in6addr_loopback));
}

int main(void)
{
	numeric_address();
	localhost();
	back_again();
	errors();
	wildcards();
	return t_status;
}

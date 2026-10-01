/*
 * The address conversions, the byte order functions and if_nametoindex,
 * through the headers' declarations and types.
 */

#include <arpa/inet.h>
#include <errno.h>
#include <net/if.h>
#include <netinet/ether.h>
#include <netinet/in.h>
#include <string.h>

#include "check.h"

int main(void)
{
	struct in_addr a;
	struct in6_addr b;
	char text[INET6_ADDRSTRLEN];
	unsigned char bytes[4];
	uint32_t big;
	struct ether_addr ethernet;
	char host[32];

	big = htonl(0x7f000001);
	memcpy(bytes, &big, sizeof bytes);
	CHECK(bytes[0] == 0x7f && bytes[3] == 1 && ntohl(big) == 0x7f000001);
	CHECK(ntohs(htons(0x1234)) == 0x1234);

	CHECK(inet_aton("127.1", &a) == 1 && a.s_addr == htonl(INADDR_LOOPBACK));
	CHECK(inet_aton("127.0.0.256", &a) == 0);
	CHECK(inet_addr("10.1.2.3") == htonl(0x0a010203));
	CHECK(inet_addr("10.1.2.300") == INADDR_NONE);
	a.s_addr = htonl(0xc0a80001);
	CHECK(!strcmp(inet_ntoa(a), "192.168.0.1"));

	CHECK(inet_pton(AF_INET, "192.168.0.1", &a) == 1 && ntohl(a.s_addr) == 0xc0a80001);
	CHECK(inet_ntop(AF_INET, &a, text, sizeof text) == text && !strcmp(text, "192.168.0.1"));
	errno = 0;
	CHECK(!inet_ntop(AF_INET, &a, text, 11) && errno == ENOSPC);
	CHECK(inet_pton(AF_INET6, "fe80::1:2", &b) == 1);
	CHECK(b.s6_addr[0] == 0xfe && b.s6_addr[1] == 0x80 && b.s6_addr[13] == 1 && b.s6_addr[15] == 2);
	CHECK(inet_ntop(AF_INET6, &b, text, sizeof text) == text && !strcmp(text, "fe80::1:2"));
	CHECK(inet_pton(AF_INET6, "1::2::3", &b) == 0);
	errno = 0;
	CHECK(inet_pton(AF_UNIX, "1.2.3.4", &a) == -1 && errno == EAFNOSUPPORT);

	CHECK(if_nametoindex("lo") > 0);
	CHECK(if_nametoindex("ferrousli-none") == 0);

	CHECK(ether_aton_r("52:54:00:12:34:56", &ethernet) == &ethernet);
	CHECK(!strcmp(ether_ntoa_r(&ethernet, text), "52:54:00:12:34:56"));
	CHECK(!ether_line("02:00:00:00:00:01 ferrix", &ethernet, host));
	CHECK(!strcmp(host, "ferrix") && ethernet.ether_addr_octet[0] == 2);
	return t_status;
}

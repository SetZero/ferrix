/*
 * The interface list, the Ethernet address conversions and sockatmark.
 *
 * Every machine has a loopback interface, so that much is asserted; what else
 * it has is not.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netinet/ether.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#include "check.h"

static void interfaces(void)
{
	struct ifaddrs *list = 0, *p;
	int loopback = 0, ipv4 = 0;

	CHECK(getifaddrs(&list) == 0);
	for (p = list; p; p = p->ifa_next) {
		CHECK(p->ifa_name && *p->ifa_name);
		if (strcmp(p->ifa_name, "lo"))
			continue;
		loopback++;
		CHECK((p->ifa_flags & IFF_LOOPBACK) != 0);
		CHECK((p->ifa_flags & IFF_UP) != 0);
		if (p->ifa_addr && p->ifa_addr->sa_family == AF_INET) {
			struct sockaddr_in *sin = (struct sockaddr_in *)p->ifa_addr;
			CHECK(sin->sin_addr.s_addr == htonl(INADDR_LOOPBACK));
			CHECK(p->ifa_netmask != 0);
			ipv4++;
		}
	}
	CHECK(loopback > 0);
	CHECK(ipv4 > 0);
	freeifaddrs(list);
	freeifaddrs(0);
}

static void names_and_indexes(void)
{
	struct if_nameindex *list = if_nameindex(), *p;
	unsigned expected = if_nametoindex("lo");
	char name[IF_NAMESIZE];
	int found = 0;

	CHECK(list != 0);
	CHECK(expected > 0);
	for (p = list; p && p->if_name; p++) {
		CHECK(p->if_index > 0);
		if (!strcmp(p->if_name, "lo")) {
			found++;
			CHECK(p->if_index == expected);
		}
	}
	CHECK(found == 1);
	if_freenameindex(list);

	CHECK(if_indextoname(expected, name) == name);
	CHECK(!strcmp(name, "lo"));
	errno = 0;
	CHECK(if_indextoname(0xffffffffu, name) == 0);
	CHECK(errno == ENXIO);
}

static void ethernet(void)
{
	struct ether_addr a, b;
	char text[18];
	char host[128];

	CHECK(ether_aton_r("00:11:22:33:44:55", &a) == &a);
	CHECK(a.ether_addr_octet[0] == 0x00 && a.ether_addr_octet[5] == 0x55);
	CHECK(ether_ntoa_r(&a, text) == text);
	CHECK(!strcmp(text, "00:11:22:33:44:55"));
	CHECK(ether_aton_r("nonsense", &b) == 0);

	CHECK(ether_aton("aa:bb:cc:dd:ee:ff") != 0);
	CHECK(!strcmp(ether_ntoa(ether_aton("aa:bb:cc:dd:ee:ff")), "AA:BB:CC:DD:EE:FF"));

	/* A line of an ethers file splits into an address and a host name. */
	CHECK(ether_line("00:00:5e:00:53:01 first.example.test", &b, host) == 0);
	CHECK(b.ether_addr_octet[2] == 0x5e && b.ether_addr_octet[5] == 0x01);
	CHECK(!strcmp(host, "first.example.test"));
	CHECK(ether_line("# a comment", &b, host) == -1);

	/* The machine almost certainly has no /etc/ethers naming these. */
	CHECK(ether_hostton("ferrousli-none.example.test", &b) == -1);
	CHECK(ether_ntohost(host, &a) == -1);
}

static void marks(void)
{
	int fds[2];

	CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, fds) == 0);
	/* A socket with no urgent data is not at a mark. A Unix socket may not
	 * answer the request at all, which is the kernel's business, not this
	 * library's: either way sockatmark must not crash. */
	CHECK(sockatmark(fds[0]) <= 1);
	close(fds[0]);
	close(fds[1]);

	errno = 0;
	CHECK(sockatmark(-1) == -1);
	CHECK(errno == EBADF);
}

int main(void)
{
	interfaces();
	names_and_indexes();
	ethernet();
	marks();
	return t_status;
}

/*
 * Replay the server's answer to a real libwayland client, and print what the
 * client made of it.
 *
 * `compositor/server`'s tests build the bytes with `compositor_wire` and read
 * them back with `compositor_wire`, which proves the crate agrees with itself.
 * This is the other half: libwayland, which every real client is built on,
 * has to accept the same bytes and report the same globals.
 *
 * The answer is passed in as hex on argv -- `examples/transcript.rs` prints
 * it -- and written into a socket pair before the client is even connected.
 * Wayland is a stream, so a client that has not asked yet still finds them
 * waiting when it reads; no server loop is needed and nothing can deadlock.
 *
 * Prints one line per global the client's listener was given, then the
 * roundtrip's result.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>

#include <wayland-client.h>

static void on_global(void *data, struct wl_registry *registry, uint32_t name,
		      const char *interface, uint32_t version)
{
	(void)registry;
	int *count = data;
	printf("global %u %s %u\n", name, interface, version);
	(*count)++;
}

static void on_global_remove(void *data, struct wl_registry *registry,
			     uint32_t name)
{
	(void)data;
	(void)registry;
	printf("global_remove %u\n", name);
}

static const struct wl_registry_listener listener = {
	.global = on_global,
	.global_remove = on_global_remove,
};

int main(int argc, char **argv)
{
	if (argc != 2) {
		fprintf(stderr, "usage: %s <hex>\n", argv[0]);
		return 2;
	}

	size_t digits = strlen(argv[1]);
	if (digits % 2) {
		fprintf(stderr, "an odd number of hex digits\n");
		return 2;
	}
	size_t len = digits / 2;
	unsigned char *bytes = malloc(len ? len : 1);
	if (!bytes)
		return 1;
	for (size_t i = 0; i < len; i++) {
		unsigned value;
		if (sscanf(argv[1] + i * 2, "%2x", &value) != 1) {
			fprintf(stderr, "not hex at %zu\n", i * 2);
			return 2;
		}
		bytes[i] = (unsigned char)value;
	}

	int pair[2];
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) < 0) {
		perror("socketpair");
		return 1;
	}

	/* The answer goes in before the client connects. A stream keeps it. */
	if (len && write(pair[0], bytes, len) != (ssize_t)len) {
		perror("write");
		return 1;
	}

	char number[32];
	snprintf(number, sizeof number, "%d", pair[1]);
	setenv("WAYLAND_SOCKET", number, 1);

	struct wl_display *display = wl_display_connect(NULL);
	if (!display) {
		fprintf(stderr, "wl_display_connect: %s\n", strerror(errno));
		return 1;
	}

	int count = 0;
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &listener, &count);

	/* Sends wl_display.sync as object 3 and waits for its callback, which
	 * is the message the replayed answer ends with. */
	int result = wl_display_roundtrip(display);
	printf("roundtrip %d globals %d error %d\n", result, count,
	       wl_display_get_error(display));

	wl_registry_destroy(registry);
	wl_display_disconnect(display);
	free(bytes);
	return 0;
}

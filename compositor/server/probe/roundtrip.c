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
#include <sys/mman.h>
#include <sys/socket.h>

#include <wayland-client.h>
#include <wayland-client-protocol.h>

/* What the registry listener gathers, so the surface half can bind. */
struct found {
	int count;
	struct wl_compositor *compositor;
	struct wl_shm *shm;
};

static void on_global(void *data, struct wl_registry *registry, uint32_t name,
		      const char *interface, uint32_t version)
{
	struct found *found = data;
	printf("global %u %s %u\n", name, interface, version);
	found->count++;
	if (!strcmp(interface, "wl_compositor"))
		found->compositor = wl_registry_bind(
			registry, name, &wl_compositor_interface, version);
	else if (!strcmp(interface, "wl_shm"))
		found->shm = wl_registry_bind(registry, name,
					      &wl_shm_interface, version);
}

static void on_global_remove(void *data, struct wl_registry *registry,
			     uint32_t name)
{
	(void)data;
	(void)registry;
	printf("global_remove %u\n", name);
}

/* The window the probe pretends to draw. */
#define WIDTH 16
#define HEIGHT 16
#define POOL_BYTES 4096

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

	struct found found = { 0 };
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &listener, &found);

	/* Sends wl_display.sync as object 3 and waits for its callback, which
	 * is the message the replayed answer ends with. */
	int result = wl_display_roundtrip(display);
	printf("roundtrip %d globals %d error %d\n", result, found.count,
	       wl_display_get_error(display));
	if (!found.compositor || !found.shm) {
		fprintf(stderr, "the replay did not carry both globals\n");
		return 1;
	}

	/* Everything a client does to put a picture on screen, none of which
	 * needs an answer: the server's side of it is what the crate's tests
	 * replay these bytes into. */
	int memory = memfd_create("probe-pool", MFD_CLOEXEC);
	if (memory < 0 || ftruncate(memory, POOL_BYTES) < 0) {
		perror("memfd_create");
		return 1;
	}
	struct wl_surface *surface = wl_compositor_create_surface(found.compositor);
	struct wl_region *region = wl_compositor_create_region(found.compositor);
	wl_region_add(region, 0, 0, WIDTH, HEIGHT);
	wl_surface_set_opaque_region(surface, region);
	struct wl_shm_pool *pool =
		wl_shm_create_pool(found.shm, memory, POOL_BYTES);
	struct wl_buffer *buffer = wl_shm_pool_create_buffer(
		pool, 0, WIDTH, HEIGHT, WIDTH * 4, WL_SHM_FORMAT_XRGB8888);
	wl_surface_attach(surface, buffer, 0, 0);
	wl_surface_damage_buffer(surface, 0, 0, WIDTH, HEIGHT);
	struct wl_callback *frame = wl_surface_frame(surface);
	wl_surface_set_buffer_scale(surface, 1);
	wl_surface_commit(surface);
	wl_display_flush(display);

	/* What the client wrote, which is what a server has to understand. */
	unsigned char out[8192];
	ssize_t wrote = read(pair[0], out, sizeof out);
	if (wrote < 0) {
		perror("read");
		return 1;
	}
	printf("client-objects surface %u region %u pool %u buffer %u frame %u\n",
	       wl_proxy_get_id((struct wl_proxy *)surface),
	       wl_proxy_get_id((struct wl_proxy *)region),
	       wl_proxy_get_id((struct wl_proxy *)pool),
	       wl_proxy_get_id((struct wl_proxy *)buffer),
	       wl_proxy_get_id((struct wl_proxy *)frame));
	printf("client-geometry %d %d %d %u\n", WIDTH, HEIGHT, WIDTH * 4,
	       (unsigned)POOL_BYTES);
	printf("client-requests ");
	for (ssize_t i = 0; i < wrote; i++)
		printf("%02x", out[i]);
	printf("\n");

	close(memory);
	wl_display_disconnect(display);
	free(bytes);
	return 0;
}

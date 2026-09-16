/*
 * Print the exact bytes libwayland puts on the socket, so this crate's
 * encoder can be required to produce the same ones.
 *
 * `compositor/wire` writes Wayland's wire format from the protocol and from
 * connection.c rather than from libwayland's binary, so nothing in it is
 * checked against a real implementation by construction. This probe is the
 * check: it drives a real libwayland client and a real libwayland server
 * over socket pairs it owns, reads back what they wrote, and prints it as
 * hex with a name for each message. `src/tests.rs` reads the committed
 * output and requires the crate's writer to match byte for byte.
 *
 * Both directions are covered because the arguments differ between them:
 * a client sends `new_id`, the unnamed `new_id` of `wl_registry.bind`, a
 * nullable `object` and a descriptor; a server sends `array` and `fixed`,
 * which no core request carries.
 *
 * No display server is needed. WAYLAND_SOCKET hands the client one end of a
 * pair and nothing ever answers; a request is marshalled and flushed whether
 * or not anyone reads it. The server half creates resources directly rather
 * than waiting for a client to ask for them.
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/types.h>

#include <wayland-client.h>
#include <wayland-client-protocol.h>
#include <wayland-server.h>
#include <wayland-server-protocol.h>

/* Read everything waiting on `fd` and print it as `name`, with the number of
 * descriptors that came with it. */
static void dump(const char *name, int fd)
{
	unsigned char bytes[4096];
	char control[CMSG_SPACE(sizeof(int) * 16)];
	struct iovec iov = { .iov_base = bytes, .iov_len = sizeof bytes };
	struct msghdr msg = {
		.msg_iov = &iov,
		.msg_iovlen = 1,
		.msg_control = control,
		.msg_controllen = sizeof control,
	};

	ssize_t len = recvmsg(fd, &msg, MSG_DONTWAIT);
	if (len < 0) {
		fprintf(stderr, "%s: recvmsg: %s\n", name, strerror(errno));
		exit(1);
	}

	int fds = 0;
	for (struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg); cmsg;
	     cmsg = CMSG_NXTHDR(&msg, cmsg)) {
		if (cmsg->cmsg_level == SOL_SOCKET &&
		    cmsg->cmsg_type == SCM_RIGHTS) {
			int count = (cmsg->cmsg_len - CMSG_LEN(0)) / sizeof(int);
			const int *got = (const int *)CMSG_DATA(cmsg);
			for (int i = 0; i < count; i++)
				close(got[i]);
			fds += count;
		}
	}

	printf("%s fds=%d bytes=", name, fds);
	for (ssize_t i = 0; i < len; i++)
		printf("%02x", bytes[i]);
	printf("\n");
}

/* The requests a client sends: every argument type that appears in one. */
static void client_side(void)
{
	int pair[2];
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) < 0) {
		perror("socketpair");
		exit(1);
	}

	char number[32];
	snprintf(number, sizeof number, "%d", pair[0]);
	setenv("WAYLAND_SOCKET", number, 1);

	struct wl_display *display = wl_display_connect(NULL);
	if (!display) {
		fprintf(stderr, "wl_display_connect: %s\n", strerror(errno));
		exit(1);
	}

	/* wl_display.get_registry: one new_id. The first request of every
	 * connection, and the smallest message there is after a header. */
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_display_flush(display);
	dump("wl_display.get_registry", pair[1]);

	/* wl_display.sync: one new_id, to a different opcode. */
	struct wl_callback *callback = wl_display_sync(display);
	wl_display_flush(display);
	dump("wl_display.sync", pair[1]);

	/* wl_registry.bind: a uint and the one unnamed new_id in the
	 * protocol, which is an interface string, a version and an id. The
	 * name is invented; nothing answers, and the bytes are the same. */
	struct wl_compositor *compositor =
		wl_registry_bind(registry, 1, &wl_compositor_interface, 6);
	wl_display_flush(display);
	dump("wl_registry.bind:wl_compositor:6", pair[1]);

	/* A shorter interface name, so the string's padding differs: "wl_shm"
	 * is six bytes, seven with its NUL, eight padded. */
	struct wl_shm *shm = wl_registry_bind(registry, 2, &wl_shm_interface, 1);
	wl_display_flush(display);
	dump("wl_registry.bind:wl_shm:1", pair[1]);

	/* wl_compositor.create_surface: a new_id on a bound object. */
	struct wl_surface *surface = wl_compositor_create_surface(compositor);
	wl_display_flush(display);
	dump("wl_compositor.create_surface", pair[1]);

	/* wl_surface.attach(NULL, 0, 0): a null object where the protocol
	 * allows one, and two ints. */
	wl_surface_attach(surface, NULL, 0, 0);
	wl_display_flush(display);
	dump("wl_surface.attach:null", pair[1]);

	/* wl_surface.damage with negative coordinates, for the sign. */
	wl_surface_damage(surface, -1, -2, 3, 4);
	wl_display_flush(display);
	dump("wl_surface.damage:negative", pair[1]);

	/* wl_shm.create_pool: a new_id, a descriptor and a size. The
	 * descriptor is not in the byte stream at all. */
	int memory = memfd_create("probe", MFD_CLOEXEC);
	if (memory < 0 || ftruncate(memory, 4096) < 0) {
		perror("memfd_create");
		exit(1);
	}
	struct wl_shm_pool *pool = wl_shm_create_pool(shm, memory, 4096);
	wl_display_flush(display);
	dump("wl_shm.create_pool", pair[1]);

	/* Three requests flushed together, to show they are packed with no
	 * padding between them. */
	wl_surface_set_buffer_scale(surface, 2);
	wl_surface_set_buffer_transform(surface, 1);
	wl_surface_commit(surface);
	wl_display_flush(display);
	dump("wl_surface.scale+transform+commit", pair[1]);

	(void)callback;
	(void)pool;
	close(memory);
	close(pair[1]);
	unsetenv("WAYLAND_SOCKET");
}

/* The events a server sends: array and fixed, which no core request has. */
static void server_side(void)
{
	int pair[2];
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, pair) < 0) {
		perror("socketpair");
		exit(1);
	}

	struct wl_display *display = wl_display_create();
	if (!display) {
		fprintf(stderr, "wl_display_create failed\n");
		exit(1);
	}
	struct wl_client *client = wl_client_create(display, pair[0]);
	if (!client) {
		fprintf(stderr, "wl_client_create: %s\n", strerror(errno));
		exit(1);
	}

	/* Ids the server picks for the test, not ones a client asked for, so
	 * the bytes do not depend on a handshake. They have to be taken in
	 * order from 2: libwayland's `wl_map_insert_at` refuses an index past
	 * the end of the map, and id 1 is the display's. */
	struct wl_resource *registry = wl_resource_create(
		client, &wl_registry_interface, wl_registry_interface.version, 2);
	struct wl_resource *surface = wl_resource_create(
		client, &wl_surface_interface, wl_surface_interface.version, 3);
	struct wl_resource *keyboard = wl_resource_create(
		client, &wl_keyboard_interface, wl_keyboard_interface.version, 4);
	struct wl_resource *pointer = wl_resource_create(
		client, &wl_pointer_interface, wl_pointer_interface.version, 5);
	if (!surface || !keyboard || !pointer || !registry) {
		fprintf(stderr, "wl_resource_create failed\n");
		exit(1);
	}

	/* wl_registry.global: a uint, a string and a uint. */
	wl_registry_send_global(registry, 1, "wl_compositor", 6);
	wl_client_flush(client);
	dump("wl_registry.global:wl_compositor", pair[1]);

	/* wl_keyboard.enter: a serial, an object and an array of keycodes,
	 * three words of it. */
	uint32_t codes[] = { 30, 48, 46 };
	struct wl_array keys;
	wl_array_init(&keys);
	void *room = wl_array_add(&keys, sizeof codes);
	memcpy(room, codes, sizeof codes);
	wl_keyboard_send_enter(keyboard, 42, surface, &keys);
	wl_client_flush(client);
	dump("wl_keyboard.enter:3keys", pair[1]);

	/* The same event with nothing held: an array of length zero. */
	struct wl_array none;
	wl_array_init(&none);
	wl_keyboard_send_enter(keyboard, 43, surface, &none);
	wl_client_flush(client);
	dump("wl_keyboard.enter:nokeys", pair[1]);

	/* An array whose length is not a multiple of four, for the padding:
	 * five bytes take eight. */
	struct wl_array odd;
	wl_array_init(&odd);
	room = wl_array_add(&odd, 5);
	memcpy(room, "abcde", 5);
	wl_keyboard_send_enter(keyboard, 44, surface, &odd);
	wl_client_flush(client);
	dump("wl_keyboard.enter:5bytes", pair[1]);

	/* wl_pointer.motion: a time and two fixed, one of them negative. */
	wl_pointer_send_motion(pointer, 1000, wl_fixed_from_double(1.5),
			       wl_fixed_from_double(-2.25));
	wl_client_flush(client);
	dump("wl_pointer.motion:1.5,-2.25", pair[1]);

	/* wl_display.error, which every protocol violation ends in: an
	 * object, a code and a string. */
	struct wl_resource *display_resource =
		wl_client_get_object(client, 1);
	if (display_resource)
		wl_resource_post_error(display_resource, 1, "no such thing");
	wl_client_flush(client);
	dump("wl_display.error:invalid_method", pair[1]);

	wl_array_release(&keys);
	wl_array_release(&none);
	wl_array_release(&odd);
	close(pair[1]);
}

int main(void)
{
	printf("# libwayland %s\n", WAYLAND_VERSION);
	client_side();
	server_side();
	return 0;
}

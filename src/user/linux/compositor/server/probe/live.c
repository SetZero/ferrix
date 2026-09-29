/*
 * Run a real libwayland client against the server, over a real socket, and
 * print what the client was told.
 *
 * `roundtrip.c` replays a recording, which can answer a client's opening
 * requests but not one whose answer depends on ids the client chose -- and
 * that is every `xdg_surface.configure`. This one connects to
 * `examples/serve.rs` listening on the path in argv[1] and has the whole
 * conversation: bind, make a window, take the configure the compositor sends,
 * ack it, attach a buffer and commit.
 *
 * That is the handshake every application performs, and a compositor that
 * gets any step of it wrong is one no application will start on. What the
 * client prints is what it was told, so a wrong configure or a missing ack
 * shows up here rather than as a blank screen.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>

#include <wayland-client.h>
#include <wayland-client-protocol.h>

#include "xdg-shell-client-protocol.h"

#define WIDTH 16
#define HEIGHT 16
#define POOL_BYTES 4096

struct state {
	struct wl_compositor *compositor;
	struct wl_shm *shm;
	struct xdg_wm_base *shell;
	struct wl_surface *surface;
	struct xdg_surface *shell_surface;
	struct xdg_toplevel *toplevel;
	struct wl_buffer *buffer;
	int configures;
	int configured_width;
	int configured_height;
	int activated;
	int tiled;
	int released;
	int frames;
	int formats;
};

static void on_global(void *data, struct wl_registry *registry, uint32_t name,
		      const char *interface, uint32_t version)
{
	struct state *state = data;
	if (!strcmp(interface, "wl_compositor"))
		state->compositor = wl_registry_bind(
			registry, name, &wl_compositor_interface, version);
	else if (!strcmp(interface, "wl_shm"))
		state->shm = wl_registry_bind(registry, name,
					      &wl_shm_interface, version);
	else if (!strcmp(interface, "xdg_wm_base"))
		state->shell = wl_registry_bind(registry, name,
						&xdg_wm_base_interface, version);
}

static void on_global_remove(void *data, struct wl_registry *registry,
			     uint32_t name)
{
	(void)data; (void)registry; (void)name;
}

static const struct wl_registry_listener registry_listener = {
	.global = on_global,
	.global_remove = on_global_remove,
};

static void on_format(void *data, struct wl_shm *shm, uint32_t format)
{
	(void)shm;
	struct state *state = data;
	printf("format %u\n", format);
	state->formats++;
}

static const struct wl_shm_listener shm_listener = { .format = on_format };

static void on_ping(void *data, struct xdg_wm_base *shell, uint32_t serial)
{
	(void)data;
	xdg_wm_base_pong(shell, serial);
}

static const struct xdg_wm_base_listener shell_listener = { .ping = on_ping };

static void on_toplevel_configure(void *data, struct xdg_toplevel *toplevel,
				  int32_t width, int32_t height,
				  struct wl_array *states)
{
	(void)toplevel;
	struct state *state = data;
	state->configured_width = width;
	state->configured_height = height;
	uint32_t *value;
	wl_array_for_each(value, states) {
		if (*value == XDG_TOPLEVEL_STATE_ACTIVATED)
			state->activated = 1;
		if (*value == XDG_TOPLEVEL_STATE_TILED_LEFT)
			state->tiled = 1;
	}
	printf("toplevel-configure %d %d states %zu\n", width, height,
	       states->size / sizeof(uint32_t));
}

static void on_toplevel_close(void *data, struct xdg_toplevel *toplevel)
{
	(void)data; (void)toplevel;
	printf("toplevel-close\n");
}

static void on_configure_bounds(void *data, struct xdg_toplevel *toplevel,
				int32_t width, int32_t height)
{
	(void)data; (void)toplevel; (void)width; (void)height;
}

static void on_wm_capabilities(void *data, struct xdg_toplevel *toplevel,
			       struct wl_array *capabilities)
{
	(void)data; (void)toplevel; (void)capabilities;
}

static const struct xdg_toplevel_listener toplevel_listener = {
	.configure = on_toplevel_configure,
	.close = on_toplevel_close,
	.configure_bounds = on_configure_bounds,
	.wm_capabilities = on_wm_capabilities,
};

static void on_surface_configure(void *data, struct xdg_surface *surface,
				 uint32_t serial)
{
	struct state *state = data;
	printf("surface-configure serial %u\n", serial);
	/* Acking is what says "I have drawn at the size you asked for". The
	 * compositor refuses a buffer before the first ack. */
	xdg_surface_ack_configure(surface, serial);
	state->configures++;
}

static const struct xdg_surface_listener surface_listener = {
	.configure = on_surface_configure,
};

static void on_release(void *data, struct wl_buffer *buffer)
{
	(void)buffer;
	struct state *state = data;
	printf("buffer-release\n");
	state->released++;
}

static const struct wl_buffer_listener buffer_listener = {
	.release = on_release,
};

static void on_frame(void *data, struct wl_callback *callback, uint32_t time)
{
	wl_callback_destroy(callback);
	struct state *state = data;
	printf("frame %u\n", time);
	state->frames++;
}

static const struct wl_callback_listener frame_listener = { .done = on_frame };

int main(int argc, char **argv)
{
	if (argc != 2) {
		fprintf(stderr, "usage: %s <socket-path>\n", argv[0]);
		return 2;
	}
	setenv("WAYLAND_DISPLAY", argv[1], 1);

	struct wl_display *display = wl_display_connect(argv[1]);
	if (!display) {
		fprintf(stderr, "wl_display_connect(%s): %s\n", argv[1],
			strerror(errno));
		return 1;
	}
	printf("connected\n");

	struct state state = { 0 };
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, &state);
	if (wl_display_roundtrip(display) < 0) {
		fprintf(stderr, "roundtrip: %s\n", strerror(errno));
		return 1;
	}
	if (!state.compositor || !state.shm || !state.shell) {
		fprintf(stderr, "a global is missing\n");
		return 1;
	}
	printf("bound\n");

	wl_shm_add_listener(state.shm, &shm_listener, &state);
	xdg_wm_base_add_listener(state.shell, &shell_listener, &state);

	/* The window, in the order every toolkit does it. */
	state.surface = wl_compositor_create_surface(state.compositor);
	state.shell_surface =
		xdg_wm_base_get_xdg_surface(state.shell, state.surface);
	xdg_surface_add_listener(state.shell_surface, &surface_listener,
				 &state);
	state.toplevel = xdg_surface_get_toplevel(state.shell_surface);
	xdg_toplevel_add_listener(state.toplevel, &toplevel_listener, &state);
	xdg_toplevel_set_title(state.toplevel, "probe window");
	xdg_toplevel_set_app_id(state.toplevel, "rocks.magical.probe");
	/* The first commit carries no buffer: it asks to be configured. */
	wl_surface_commit(state.surface);

	/* Wait for the configure and ack it. */
	if (wl_display_roundtrip(display) < 0) {
		fprintf(stderr, "roundtrip after commit: %s\n",
			strerror(errno));
		return 1;
	}

	/* Now a buffer is allowed. */
	int memory = memfd_create("probe-pool", MFD_CLOEXEC);
	if (memory < 0 || ftruncate(memory, POOL_BYTES) < 0) {
		perror("memfd_create");
		return 1;
	}
	struct wl_shm_pool *pool =
		wl_shm_create_pool(state.shm, memory, POOL_BYTES);
	state.buffer = wl_shm_pool_create_buffer(
		pool, 0, WIDTH, HEIGHT, WIDTH * 4, WL_SHM_FORMAT_XRGB8888);
	wl_buffer_add_listener(state.buffer, &buffer_listener, &state);
	wl_surface_attach(state.surface, state.buffer, 0, 0);
	wl_surface_damage_buffer(state.surface, 0, 0, WIDTH, HEIGHT);
	struct wl_callback *frame = wl_surface_frame(state.surface);
	wl_callback_add_listener(frame, &frame_listener, &state);
	wl_surface_commit(state.surface);

	if (wl_display_roundtrip(display) < 0) {
		fprintf(stderr, "roundtrip after attach: %s\n",
			strerror(errno));
		return 1;
	}

	printf("result configures %d size %dx%d activated %d tiled %d formats %d error %d\n",
	       state.configures, state.configured_width,
	       state.configured_height, state.activated, state.tiled,
	       state.formats, wl_display_get_error(display));

	close(memory);
	wl_display_disconnect(display);
	return 0;
}

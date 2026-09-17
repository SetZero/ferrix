/*
 * Print libwayland's own compiled interface tables, so the tables generated
 * from the XML can be required to match them.
 *
 * `scripts/gen-wayland-protocol.py` reads the same XML `wayland-scanner`
 * reads, and could read it wrong: an opcode off by one, an argument type
 * confused, a nullable flag dropped. Nothing in the generator would notice.
 * This links against the real `wl_*_interface` structures -- libwayland's own
 * for the core protocol, and wayland-scanner's output for the rest -- and
 * prints what they say, in the order they say it.
 *
 * One line per message:
 *
 *     <interface> <version> <request|event> <opcode> <name> <signature>
 *
 * The signature is libwayland's own string, from `struct wl_message`: a
 * `since` version as leading digits, `?` before a nullable argument, and then
 * one letter per argument -- i u f s o n a h, and `n` with no interface in
 * `types` is the unnamed new_id of `wl_registry.bind`. `src/tests.rs` reads
 * the committed output and requires the generated tables to agree.
 */

#include <stdio.h>
#include <string.h>

#include <wayland-client.h>
#include <wayland-client-protocol.h>

#include "xdg-shell-client-protocol.h"
#include "xdg-decoration-client-protocol.h"
#include "wlr-layer-shell-client-protocol.h"
#include "wlr-foreign-toplevel-management-client-protocol.h"

static void print_messages(const struct wl_interface *interface,
			   const char *kind, const struct wl_message *messages,
			   int count)
{
	for (int opcode = 0; opcode < count; opcode++) {
		const struct wl_message *message = &messages[opcode];
		printf("%s %d %s %d %s %s\n", interface->name,
		       interface->version, kind, opcode, message->name,
		       message->signature);
		/* An `n` whose entry in `types` is NULL is the unnamed new_id;
		 * say so, since the signature letter alone does not. */
		int slot = 0;
		for (const char *c = message->signature; *c; c++) {
			if (*c >= '0' && *c <= '9')
				continue;
			if (*c == '?')
				continue;
			if (*c == 'n' && message->types &&
			    message->types[slot] == NULL)
				printf("%s %d %s %d %s any-new-id %d\n",
				       interface->name, interface->version,
				       kind, opcode, message->name, slot);
			slot++;
		}
	}
}

static void print_interface(const struct wl_interface *interface)
{
	print_messages(interface, "request", interface->methods,
		       interface->method_count);
	print_messages(interface, "event", interface->events,
		       interface->event_count);
	if (interface->method_count == 0 && interface->event_count == 0)
		printf("%s %d empty\n", interface->name, interface->version);
}

int main(void)
{
	const struct wl_interface *interfaces[] = {
		/* wayland.xml */
		&wl_display_interface,
		&wl_registry_interface,
		&wl_callback_interface,
		&wl_compositor_interface,
		&wl_shm_pool_interface,
		&wl_shm_interface,
		&wl_buffer_interface,
		&wl_data_offer_interface,
		&wl_data_source_interface,
		&wl_data_device_interface,
		&wl_data_device_manager_interface,
		&wl_shell_interface,
		&wl_shell_surface_interface,
		&wl_surface_interface,
		&wl_seat_interface,
		&wl_pointer_interface,
		&wl_keyboard_interface,
		&wl_touch_interface,
		&wl_output_interface,
		&wl_region_interface,
		&wl_subcompositor_interface,
		&wl_subsurface_interface,
		/* xdg-shell.xml */
		&xdg_wm_base_interface,
		&xdg_positioner_interface,
		&xdg_surface_interface,
		&xdg_toplevel_interface,
		&xdg_popup_interface,
		/* xdg-decoration-unstable-v1.xml */
		&zxdg_decoration_manager_v1_interface,
		&zxdg_toplevel_decoration_v1_interface,
		/* wlr-layer-shell-unstable-v1.xml */
		&zwlr_layer_shell_v1_interface,
		&zwlr_layer_surface_v1_interface,
		/* wlr-foreign-toplevel-management-unstable-v1.xml */
		&zwlr_foreign_toplevel_manager_v1_interface,
		&zwlr_foreign_toplevel_handle_v1_interface,
	};

	printf("# libwayland %s\n", WAYLAND_VERSION);
	for (size_t i = 0; i < sizeof interfaces / sizeof interfaces[0]; i++)
		print_interface(interfaces[i]);
	return 0;
}

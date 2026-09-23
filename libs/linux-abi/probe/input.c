/*
 * The evdev numbers and layouts `libs/linux-abi/src/input.rs` writes down,
 * printed from the UAPI headers themselves.
 *
 * Run by `libs/linux-abi/probe/input.sh` on a Linux host with the kernel's
 * UAPI headers (`linux-libc-dev`) once natively for 64-bit and once for
 * ARMv7-A under qemu-arm, into `input-64.txt` and `input-32.txt` beside it.
 * The crate's tests read both files and require every line to match what the
 * crate says, so a number changed on either side fails `cargo test`.
 *
 * Each line is `name value`, in decimal. Layouts are `sizeof.<struct>` and
 * `offsetof.<struct>.<field>`. A request whose number depends on an argument
 * is printed for a sample argument, named as it was called.
 *
 * Built with `VIEW` defined, it prints only `struct input_event` and
 * `time_t`, each line prefixed with `view.<VIEW>.`: `input.sh` builds it so
 * three more times, for 32-bit and 64-bit `time_t` and for a libc with a
 * 64-bit `time_t` that does not define `__USE_TIME_BITS64` (built with
 * `UNDEF_TIME_BITS64`), because `linux/input.h` picks the structure's first
 * two fields by that macro and only the kernel's view is what `read` returns.
 */

#include <stddef.h>
#include <stdio.h>
#include <sys/time.h>
#include <time.h>
#ifdef UNDEF_TIME_BITS64
#undef __USE_TIME_BITS64
#endif
#include <linux/input.h>
#include <linux/major.h>

#ifndef VIEW
#define PREFIX ""
#else
#define PREFIX "view." VIEW "."
#endif

#define VALUE(name) printf("%s %llu\n", #name, (unsigned long long)(name))
#define SIZE(type) printf(PREFIX "sizeof.%s %zu\n", #type, sizeof(struct type))
#define FIELD(type, field) \
	printf(PREFIX "offsetof.%s.%s %zu\n", #type, #field, offsetof(struct type, field))

static void input_event(void)
{
	SIZE(input_event);
	/* The first two fields are `time.tv_sec` or `__sec` by the view. */
	printf(PREFIX "offsetof.input_event.sec %zu\n",
	       offsetof(struct input_event, input_event_sec));
	printf(PREFIX "offsetof.input_event.usec %zu\n",
	       offsetof(struct input_event, input_event_usec));
	FIELD(input_event, type);
	FIELD(input_event, code);
	FIELD(input_event, value);
}

#ifdef VIEW
int main(void)
{
	printf(PREFIX "sizeof.time_t %zu\n", sizeof(time_t));
	input_event();
	return 0;
}
#else
int main(void)
{
	/* The protocol version and the character device's major. */
	VALUE(EV_VERSION);
	VALUE(INPUT_MAJOR);

	/* How a request number is put together (`asm-generic/ioctl.h`). */
	VALUE(_IOC_NRBITS);
	VALUE(_IOC_TYPEBITS);
	VALUE(_IOC_SIZEBITS);
	VALUE(_IOC_DIRBITS);
	VALUE(_IOC_NRSHIFT);
	VALUE(_IOC_TYPESHIFT);
	VALUE(_IOC_SIZESHIFT);
	VALUE(_IOC_DIRSHIFT);
	VALUE(_IOC_NONE);
	VALUE(_IOC_WRITE);
	VALUE(_IOC_READ);

	/* The requests with fixed numbers. */
	VALUE(EVIOCGVERSION);
	VALUE(EVIOCGID);
	VALUE(EVIOCGREP);
	VALUE(EVIOCSREP);
	VALUE(EVIOCGKEYCODE);
	VALUE(EVIOCGKEYCODE_V2);
	VALUE(EVIOCSKEYCODE);
	VALUE(EVIOCSKEYCODE_V2);
	VALUE(EVIOCSFF);
	VALUE(EVIOCRMFF);
	VALUE(EVIOCGEFFECTS);
	VALUE(EVIOCGRAB);
	VALUE(EVIOCREVOKE);
	VALUE(EVIOCGMASK);
	VALUE(EVIOCSMASK);
	VALUE(EVIOCSCLOCKID);

	/* The requests that carry a length or a code, at sample arguments. */
	VALUE(EVIOCGNAME(256));
	VALUE(EVIOCGNAME(4096));
	VALUE(EVIOCGPHYS(256));
	VALUE(EVIOCGUNIQ(256));
	VALUE(EVIOCGPROP(8));
	VALUE(EVIOCGMTSLOTS(64));
	VALUE(EVIOCGKEY(96));
	VALUE(EVIOCGLED(8));
	VALUE(EVIOCGSND(8));
	VALUE(EVIOCGSW(8));
	VALUE(EVIOCGBIT(0,8));
	VALUE(EVIOCGBIT(EV_KEY,96));
	VALUE(EVIOCGBIT(EV_REL,8));
	VALUE(EVIOCGBIT(EV_ABS,8));
	VALUE(EVIOCGBIT(EV_MAX,8));
	VALUE(EVIOCGABS(ABS_X));
	VALUE(EVIOCGABS(ABS_Y));
	VALUE(EVIOCGABS(ABS_MAX));
	VALUE(EVIOCSABS(ABS_X));
	VALUE(EVIOCSABS(ABS_MAX));

	/* Event types. */
	VALUE(EV_SYN);
	VALUE(EV_KEY);
	VALUE(EV_REL);
	VALUE(EV_ABS);
	VALUE(EV_MSC);
	VALUE(EV_SW);
	VALUE(EV_LED);
	VALUE(EV_SND);
	VALUE(EV_REP);
	VALUE(EV_FF);
	VALUE(EV_PWR);
	VALUE(EV_FF_STATUS);
	VALUE(EV_MAX);
	VALUE(EV_CNT);

	/* Synchronisation events. */
	VALUE(SYN_REPORT);
	VALUE(SYN_CONFIG);
	VALUE(SYN_MT_REPORT);
	VALUE(SYN_DROPPED);
	VALUE(SYN_MAX);
	VALUE(SYN_CNT);

	/* Device properties. */
	VALUE(INPUT_PROP_POINTER);
	VALUE(INPUT_PROP_DIRECT);
	VALUE(INPUT_PROP_MAX);
	VALUE(INPUT_PROP_CNT);

	/* Keys and buttons the test and QEMU's keyboard and tablet use. */
	VALUE(KEY_RESERVED);
	VALUE(KEY_ESC);
	VALUE(KEY_A);
	VALUE(BTN_MISC);
	VALUE(BTN_MOUSE);
	VALUE(BTN_LEFT);
	VALUE(BTN_RIGHT);
	VALUE(BTN_MIDDLE);
	VALUE(BTN_SIDE);
	VALUE(BTN_EXTRA);
	VALUE(BTN_TOUCH);
	VALUE(BTN_GEAR_DOWN);
	VALUE(BTN_GEAR_UP);
	VALUE(KEY_MAX);
	VALUE(KEY_CNT);

	/* Relative and absolute axes. */
	VALUE(REL_X);
	VALUE(REL_Y);
	VALUE(REL_HWHEEL);
	VALUE(REL_WHEEL);
	VALUE(REL_MAX);
	VALUE(REL_CNT);
	VALUE(ABS_X);
	VALUE(ABS_Y);
	VALUE(ABS_MT_SLOT);
	VALUE(ABS_MT_POSITION_X);
	VALUE(ABS_MT_POSITION_Y);
	VALUE(ABS_MT_TRACKING_ID);
	VALUE(ABS_MAX);
	VALUE(ABS_CNT);

	/* The other types' codes and bounds. */
	VALUE(MSC_SCAN);
	VALUE(MSC_MAX);
	VALUE(MSC_CNT);
	VALUE(SW_MAX);
	VALUE(SW_CNT);
	VALUE(LED_NUML);
	VALUE(LED_CAPSL);
	VALUE(LED_SCROLLL);
	VALUE(LED_MAX);
	VALUE(LED_CNT);
	VALUE(SND_MAX);
	VALUE(SND_CNT);
	VALUE(REP_DELAY);
	VALUE(REP_PERIOD);
	VALUE(REP_MAX);
	VALUE(REP_CNT);
	VALUE(FF_MAX);
	VALUE(FF_CNT);

	/* Device ids. */
	VALUE(ID_BUS);
	VALUE(ID_VENDOR);
	VALUE(ID_PRODUCT);
	VALUE(ID_VERSION);
	VALUE(BUS_USB);
	VALUE(BUS_VIRTUAL);

	/* The clocks `EVIOCSCLOCKID` accepts. */
	VALUE(CLOCK_REALTIME);
	VALUE(CLOCK_MONOTONIC);
	VALUE(CLOCK_BOOTTIME);

	/* Layouts. */
	input_event();

	SIZE(input_id);
	FIELD(input_id, bustype);
	FIELD(input_id, vendor);
	FIELD(input_id, product);
	FIELD(input_id, version);

	SIZE(input_absinfo);
	FIELD(input_absinfo, value);
	FIELD(input_absinfo, minimum);
	FIELD(input_absinfo, maximum);
	FIELD(input_absinfo, fuzz);
	FIELD(input_absinfo, flat);
	FIELD(input_absinfo, resolution);
	return 0;
}
#endif

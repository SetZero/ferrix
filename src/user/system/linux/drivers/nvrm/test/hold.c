/*
 * The init of `cargo xtask test-nvrm`'s boots: it holds the machine up
 * while nvrm, which devmgr started before init, says what it found. The
 * gate stops QEMU once it has read the line it waits for; this only keeps
 * the kernel from powering off first, as it does when init exits.
 */
#include <unistd.h>

int main(void)
{
	for (;;)
		sleep(3600);
}

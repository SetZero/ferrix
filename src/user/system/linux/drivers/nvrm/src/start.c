/*
 * nvrm's entry: from a native process's start to ferrousli's
 * __libc_start_main.
 *
 * devmgr starts nvrm as it starts every driver (docs/DEVMGR.md §3):
 * process_create from the image the kernel read out of the initramfs, in a
 * job of its own, and process_start with its bootstrap channel. That start
 * is a native one: the kernel enters here with the bootstrap handle in the
 * first argument register and an empty stack, with no argc, argv,
 * environment or auxiliary vector, which a Linux program's crt1.o expects.
 * So this builds them, on the stack, and hands them to ferrousli as
 * crt1.o would; from main on, nvrm is an ordinary static ferrousli program
 * that may also make native calls (docs/NVIDIA.md §4.1).
 *
 * What the vector holds is what ferrousli reads at start
 * (src/user/system/linux/ferrousli/src/start.rs): the program headers, for
 * the main thread's TLS; 16 random bytes, for the stack protector's canary
 * and the pointer guard; the page size; and the program's name. There is
 * no AT_SYSINFO_EHDR, since the kernel maps no vDSO into a native process,
 * and ferrousli then makes those calls itself.
 *
 * Built with -fno-stack-protector: nothing here may read the canary
 * through the thread pointer, which is not set until __libc_start_main.
 */
#include <elf.h>
#include <stdint.h>

/* The bootstrap channel's handle, for main. */
uint32_t nvrm_bootstrap;

/* The ELF header, where the linker put it: the program's first byte. */
extern const Elf64_Ehdr __ehdr_start;

extern int main(int argc, char **argv, char **envp);
extern void __libc_start_main(int (*main)(int, char **, char **), int argc,
			      char **argv, void (*init)(void),
			      void (*fini)(void), void (*rtld_fini)(void),
			      void *stack_end) __attribute__((noreturn));

/* The x86-64 getrandom(2), called directly: the C library is not set up. */
#define GETRANDOM 318

static long getrandom_raw(void *buffer, unsigned long length)
{
	long result = GETRANDOM;
	__asm__ volatile("syscall"
			 : "+a"(result)
			 : "D"(buffer), "S"(length), "d"(0)
			 : "rcx", "r11", "memory");
	return result;
}

__attribute__((noreturn, used)) void nvrm_start(unsigned long bootstrap)
{
	/* On this frame, which lives as long as the process: nothing returns
	 * from __libc_start_main. */
	char name[8] = "nvrm";
	unsigned char random[16] = { 0 };
	const Elf64_Ehdr *header = &__ehdr_start;
	uintptr_t phdr = (uintptr_t)header + header->e_phoff;
	/* argc, argv[0], NULL, an empty environment's NULL, then the vector. */
	uintptr_t stack[] = {
		1,
		(uintptr_t)name,
		0,
		0,
		AT_PHDR,   phdr,
		AT_PHENT,  header->e_phentsize,
		AT_PHNUM,  header->e_phnum,
		AT_PAGESZ, 4096,
		AT_ENTRY,  header->e_entry,
		AT_RANDOM, (uintptr_t)random,
		AT_EXECFN, (uintptr_t)name,
		AT_SECURE, 0,
		AT_NULL,   0,
	};

	nvrm_bootstrap = (uint32_t)bootstrap;
	/* A short read leaves zeros: a weaker canary, never no start. */
	(void)getrandom_raw(random, sizeof random);
	__libc_start_main(main, 1, (char **)&stack[1], 0, 0, 0, stack);
}

/*
 * The entry point, and the same first instructions as a native program's
 * (src/user/system/native/rt): the outermost frame, the stack aligned, and
 * a call that never returns. xtask's check of a native image reads them.
 */
__asm__(".text\n"
	".global _start\n"
	".type _start, @function\n"
	"_start:\n"
	"\txor %ebp, %ebp\n"
	"\tand $-16, %rsp\n"
	"\tcall nvrm_start\n"
	"\tud2\n");

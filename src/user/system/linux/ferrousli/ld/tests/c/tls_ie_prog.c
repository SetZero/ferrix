/* dlopen of a library whose TLS is read by initial-exec, from a thread that
   was running before it was loaded, with no C library: the program plays
   the C library's part, registering a walk over its two threads with the
   loader, as ferrousli's does. IE_LIBRARY is tls_ie.c's path. Each check
   exits with its own number. */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <stddef.h>

/* The loader's __ferrousli_loader, to revision 6: ld/src/interface.rs. */
struct loader {
	unsigned long version, tls_size, tls_align;
	void (*init_tls)(void *tp);
	void *run_program_init, *dlopen, *dlsym, *dlclose, *dlerror, *dladdr;
	void *dl_iterate_phdr, *auxv, *dlsym_from, *dlvsym_from;
	void (*catch_up_thread)(void *tp);
	void (*set_thread_walk)(void (*walk)(void (*visit)(void *tp)));
};

extern struct loader __ferrousli_loader;

/* The second thread's static TLS, below its pointer, and its stack. */
static char child_tls[65536] __attribute__((aligned(64)));
static char child_stack[65536] __attribute__((aligned(16)));
static void *child_tp;

static int go, done, child_value = -1;
static int (*ie_read)(void);

static void *main_tp(void) {
	void *tp;
	__asm__("mov %%fs:0, %0" : "=r"(tp));
	return tp;
}

static void walk(void (*visit)(void *tp)) {
	visit(main_tp());
	visit(child_tp);
}

/* The second thread: it waits for the library, then reads its variable. */
static void child(void) {
	while (!__atomic_load_n(&go, __ATOMIC_ACQUIRE))
		;
	__atomic_store_n(&child_value, ie_read(), __ATOMIC_RELAXED);
	__atomic_store_n(&done, 1, __ATOMIC_RELEASE);
}

/* clone(CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD |
   CLONE_SYSVSEM | CLONE_SETTLS) onto `stack` with thread pointer `tls`; the
   new thread calls `fn` and exits. Returns the new thread's id. */
long spawn(void *stack, void *tls, void (*fn)(void));
__asm__(".globl spawn\nspawn:\n"
	"\tmov %rdx, %r9\n"
	"\tmov %rsi, %r8\n"
	"\tmov %rdi, %rsi\n"
	"\tmov $0xd0f00, %edi\n"
	"\txor %edx, %edx\n"
	"\txor %r10d, %r10d\n"
	"\tmov $56, %eax\n"
	"\tsyscall\n"
	"\ttest %rax, %rax\n"
	"\tjnz 1f\n"
	"\tand $-16, %rsp\n"
	"\tcall *%r9\n"
	"\tmov $60, %eax\n"
	"\txor %edi, %edi\n"
	"\tsyscall\n"
	"1:\tret\n");

__attribute__((force_align_arg_pointer, used)) static void run(void) {
	long r = 42;
	struct loader *loader = &__ferrousli_loader;
	unsigned long below = (loader->tls_size + 63) & ~63ul;
	void *h;
	if (loader->version < 6 || below + 64 > sizeof child_tls) {
		r = 91; /* no thread walk, or more static TLS than the fixture has */
	} else {
		child_tp = child_tls + below;
		*(void **)child_tp = child_tp;
		loader->init_tls(child_tp);
		loader->catch_up_thread(child_tp);
		loader->set_thread_walk(walk);
		if (spawn(child_stack + sizeof child_stack, child_tp, child) <= 0) {
			r = 92; /* the second thread did not start */
		} else if (!(h = dlopen(IE_LIBRARY, RTLD_NOW))
			|| !(ie_read = (int (*)(void))dlsym(h, "ie_read"))) {
			r = 93; /* the library was refused: its initial-exec TLS */
		} else if (ie_read() != 40) {
			r = 94; /* this thread's block does not hold the image */
		} else {
			__atomic_store_n(&go, 1, __ATOMIC_RELEASE);
			while (!__atomic_load_n(&done, __ATOMIC_ACQUIRE))
				;
			if (child_value != 40)
				r = 95; /* the running thread's block was not given it */
		}
	}
	__asm__ volatile("syscall" :: "a"(231L), "D"(r) : "rcx", "r11", "memory");
	__builtin_unreachable();
}

__asm__(".globl _start\n_start:\n\tcall run\n\thlt\n");

/*
 * setjmp and longjmp: return values, longjmp with 0, volatile locals,
 * callee-saved registers, deep recursion, the underscore and glibc names,
 * sigsetjmp saving the mask only when asked, and the pointer mangling.
 *
 * The first part is adapted from libc-test's src/functional/setjmp.c (MIT).
 */

#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <string.h>
#include "check.h"

/* glibc's names, which its headers call. musl's headers do not declare them. */
int __sigsetjmp(sigjmp_buf, int) __attribute__((returns_twice));
_Noreturn void __longjmp_chk(jmp_buf, int);

/*
 * Sets every callee-saved register to a known value, calls setjmp, and jumps
 * back from a function that overwrites them all. Returns 1 if setjmp returned
 * 5 with every register as it was.
 */
int registers_survive(void);
_Noreturn void clobber_and_longjmp(jmp_buf, int);
__asm__(".text\n"
	".globl registers_survive\n"
	".type registers_survive, @function\n"
	"registers_survive:\n"
	"\tpush %rbx\n\tpush %rbp\n\tpush %r12\n\tpush %r13\n\tpush %r14\n\tpush %r15\n"
	"\tsub $216, %rsp\n"
	"\tmovabs $0x0123456789abcdef, %rbx\n"
	"\tmovabs $0x1122334455667788, %rbp\n"
	"\tmovabs $0x2233445566778899, %r12\n"
	"\tmovabs $0x33445566778899aa, %r13\n"
	"\tmovabs $0x445566778899aabb, %r14\n"
	"\tmovabs $0x5566778899aabbcc, %r15\n"
	"\tmov %rsp, %rdi\n"
	"\tcall setjmp\n"
	"\ttest %eax, %eax\n"
	"\tjnz 2f\n"
	"\tmov %rsp, %rdi\n"
	"\tmov $5, %esi\n"
	"\tcall clobber_and_longjmp\n"
	"2:\tmov %eax, %ecx\n"
	"\txor %eax, %eax\n"
	"\tcmp $5, %ecx\n\tjne 3f\n"
	"\tmovabs $0x0123456789abcdef, %rdx\n\tcmp %rdx, %rbx\n\tjne 3f\n"
	"\tmovabs $0x1122334455667788, %rdx\n\tcmp %rdx, %rbp\n\tjne 3f\n"
	"\tmovabs $0x2233445566778899, %rdx\n\tcmp %rdx, %r12\n\tjne 3f\n"
	"\tmovabs $0x33445566778899aa, %rdx\n\tcmp %rdx, %r13\n\tjne 3f\n"
	"\tmovabs $0x445566778899aabb, %rdx\n\tcmp %rdx, %r14\n\tjne 3f\n"
	"\tmovabs $0x5566778899aabbcc, %rdx\n\tcmp %rdx, %r15\n\tjne 3f\n"
	"\tmov $1, %eax\n"
	"3:\tadd $216, %rsp\n"
	"\tpop %r15\n\tpop %r14\n\tpop %r13\n\tpop %r12\n\tpop %rbp\n\tpop %rbx\n"
	"\tret\n"
	".globl clobber_and_longjmp\n"
	".type clobber_and_longjmp, @function\n"
	"clobber_and_longjmp:\n"
	"\tmov $-1, %rbx\n\tmov $-2, %rbp\n\tmov $-3, %r12\n"
	"\tmov $-4, %r13\n\tmov $-5, %r14\n\tmov $-6, %r15\n"
	"\tjmp longjmp\n");

/* The pointer guard glibc's layout keeps at %fs:0x30. */
unsigned long pointer_guard(void);
__asm__(".text\n"
	".globl pointer_guard\n"
	".type pointer_guard, @function\n"
	"pointer_guard:\n"
	"\tmovq %fs:0x30, %rax\n"
	"\tret\n");

static unsigned long demangle(unsigned long word)
{
	return ((word >> 17) | (word << 47)) ^ pointer_guard();
}

static jmp_buf deep;
static volatile int unwound;
/* Through a pointer, so the compiler does not call the recursion infinite. */
static void (*volatile jump)(struct __jmp_buf_tag *, int) = longjmp;

__attribute__((noinline))
static void recurse(int depth)
{
	volatile char pad[64];
	pad[0] = (char)depth;
	if (depth == 0) {
		jump(deep, 99);
		return;
	}
	recurse(depth - 1);
	/* Not reached; keeps the call from being a tail call. */
	unwound += pad[0];
}

static int blocked(int sig)
{
	sigset_t now;
	sigprocmask(SIG_BLOCK, 0, &now);
	return sigismember(&now, sig);
}

int main(void)
{
	volatile int x = 0, r;
	jmp_buf jb;
	sigjmp_buf sjb;
	volatile sigset_t oldset;
	sigset_t set, set2;

	/* libc-test. */
	if (!setjmp(jb)) {
		x = 1;
		longjmp(jb, 1);
	}
	CHECK(x == 1);

	x = 0;
	r = setjmp(jb);
	if (!x) {
		x = 1;
		longjmp(jb, 0);
	}
	CHECK(r == 1);

	sigemptyset(&set);
	sigaddset(&set, SIGUSR1);
	sigprocmask(SIG_UNBLOCK, &set, &set2);
	oldset = set2;

	/* Improve the chances of catching failure of sigsetjmp to
	 * properly save the signal mask in the sigjmb_buf. */
	memset(&sjb, -1, sizeof sjb);

	if (!sigsetjmp(sjb, 1)) {
		sigemptyset(&set);
		sigaddset(&set, SIGUSR1);
		sigprocmask(SIG_BLOCK, &set, 0);
		siglongjmp(sjb, 1);
	}
	set = oldset;
	sigprocmask(SIG_SETMASK, &set, &set2);
	CHECK(sigismember(&set2, SIGUSR1) == 0);

	sigemptyset(&set);
	sigaddset(&set, SIGUSR1);
	sigprocmask(SIG_UNBLOCK, &set, &set2);
	oldset = set2;

	if (!sigsetjmp(sjb, 0)) {
		sigemptyset(&set);
		sigaddset(&set, SIGUSR1);
		sigprocmask(SIG_BLOCK, &set, 0);
		siglongjmp(sjb, 1);
	}
	set = oldset;
	sigprocmask(SIG_SETMASK, &set, &set2);
	CHECK(sigismember(&set2, SIGUSR1) == 1);
	sigprocmask(SIG_UNBLOCK, &set, 0);

	/* Values pass through, negative ones too, and a volatile keeps its update. */
	x = 10;
	r = setjmp(jb);
	if (r == 0) {
		x = 20;
		longjmp(jb, -7);
	}
	CHECK(r == -7);
	CHECK(x == 20);

	/* The same buffer can be jumped to again and again. */
	x = 0;
	r = setjmp(jb);
	if (x < 3) {
		x = x + 1;
		longjmp(jb, x);
	}
	CHECK(r == 3);
	CHECK(x == 3);

	/* Every callee-saved register comes back. */
	CHECK(registers_survive() == 1);

	/* Out of deep recursion. */
	r = setjmp(deep);
	if (r == 0)
		recurse(10000);
	CHECK(r == 99);
	CHECK(unwound == 0);

	/* The underscore and glibc names. */
	r = _setjmp(jb);
	if (r == 0)
		_longjmp(jb, 0);
	CHECK(r == 1);
	r = setjmp(jb);
	if (r == 0)
		__longjmp_chk(jb, 42);
	CHECK(r == 42);

	/* __sigsetjmp saves the mask when asked, and longjmp restores it too. */
	sigemptyset(&set);
	sigaddset(&set, SIGUSR2);
	r = __sigsetjmp(sjb, 1);
	if (r == 0) {
		CHECK(sjb->__fl == 1);
		sigprocmask(SIG_BLOCK, &set, 0);
		longjmp(sjb, 2);
	}
	CHECK(r == 2);
	CHECK(blocked(SIGUSR2) == 0);

	/* setjmp saves no mask, so blocking survives the jump. */
	memset(&jb, -1, sizeof jb);
	r = setjmp(jb);
	if (r == 0) {
		CHECK(jb->__fl == 0);
		sigprocmask(SIG_BLOCK, &set, 0);
		longjmp(jb, 3);
	}
	CHECK(r == 3);
	CHECK(blocked(SIGUSR2) == 1);
	sigprocmask(SIG_UNBLOCK, &set, 0);

	/*
	 * The saved stack pointer and return address are mangled with the
	 * pointer guard, and demangle to this frame and this function.
	 */
	unsigned long frame = (unsigned long)__builtin_frame_address(0);
	if (setjmp(jb) == 0) {
		unsigned long sp = demangle(jb->__jb[6]);
		unsigned long pc = demangle(jb->__jb[7]);
		CHECK(pointer_guard() != 0);
		CHECK(jb->__jb[6] != sp);
		CHECK(sp < frame && frame - sp < 65536);
		CHECK(pc - (unsigned long)main + 65536 < 131072);
	}

	return t_status;
}

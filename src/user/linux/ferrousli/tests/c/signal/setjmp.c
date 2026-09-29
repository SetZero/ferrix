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
#if defined(__x86_64__)
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
#elif defined(__aarch64__)
/* x19 to x28 and d8 to d15, each set to a value of its own, then compared. */
__asm__(".text\n"
	".globl registers_survive\n"
	".type registers_survive, %function\n"
	"registers_survive:\n"
	"\tstp x29, x30, [sp, #-160]!\n"
	"\tmov x29, sp\n"
	"\tstp x19, x20, [sp, #16]\n\tstp x21, x22, [sp, #32]\n"
	"\tstp x23, x24, [sp, #48]\n\tstp x25, x26, [sp, #64]\n"
	"\tstp x27, x28, [sp, #80]\n"
	"\tstp d8, d9, [sp, #96]\n\tstp d10, d11, [sp, #112]\n"
	"\tstp d12, d13, [sp, #128]\n\tstp d14, d15, [sp, #144]\n"
	"\tsub sp, sp, #320\n"
	"\tmov x19, #0x119\n\tmov x20, #0x120\n\tmov x21, #0x121\n"
	"\tmov x22, #0x122\n\tmov x23, #0x123\n\tmov x24, #0x124\n"
	"\tmov x25, #0x125\n\tmov x26, #0x126\n\tmov x27, #0x127\n"
	"\tmov x28, #0x128\n"
	"\tfmov d8, #1.0\n\tfmov d9, #2.0\n\tfmov d10, #3.0\n\tfmov d11, #4.0\n"
	"\tfmov d12, #5.0\n\tfmov d13, #6.0\n\tfmov d14, #7.0\n\tfmov d15, #8.0\n"
	"\tmov x0, sp\n"
	"\tbl setjmp\n"
	"\tcbnz w0, 2f\n"
	"\tmov x0, sp\n"
	"\tmov w1, #5\n"
	"\tbl clobber_and_longjmp\n"
	"2:\tmov w9, w0\n"
	"\tmov w0, #0\n"
	"\tcmp w9, #5\n\tb.ne 3f\n"
	"\tcmp x19, #0x119\n\tb.ne 3f\n\tcmp x20, #0x120\n\tb.ne 3f\n"
	"\tcmp x21, #0x121\n\tb.ne 3f\n\tcmp x22, #0x122\n\tb.ne 3f\n"
	"\tcmp x23, #0x123\n\tb.ne 3f\n\tcmp x24, #0x124\n\tb.ne 3f\n"
	"\tcmp x25, #0x125\n\tb.ne 3f\n\tcmp x26, #0x126\n\tb.ne 3f\n"
	"\tcmp x27, #0x127\n\tb.ne 3f\n\tcmp x28, #0x128\n\tb.ne 3f\n"
	"\tfmov d0, #1.0\n\tfcmp d8, d0\n\tb.ne 3f\n"
	"\tfmov d0, #2.0\n\tfcmp d9, d0\n\tb.ne 3f\n"
	"\tfmov d0, #3.0\n\tfcmp d10, d0\n\tb.ne 3f\n"
	"\tfmov d0, #4.0\n\tfcmp d11, d0\n\tb.ne 3f\n"
	"\tfmov d0, #5.0\n\tfcmp d12, d0\n\tb.ne 3f\n"
	"\tfmov d0, #6.0\n\tfcmp d13, d0\n\tb.ne 3f\n"
	"\tfmov d0, #7.0\n\tfcmp d14, d0\n\tb.ne 3f\n"
	"\tfmov d0, #8.0\n\tfcmp d15, d0\n\tb.ne 3f\n"
	"\tmov w0, #1\n"
	"3:\tadd sp, sp, #320\n"
	"\tldp x19, x20, [sp, #16]\n\tldp x21, x22, [sp, #32]\n"
	"\tldp x23, x24, [sp, #48]\n\tldp x25, x26, [sp, #64]\n"
	"\tldp x27, x28, [sp, #80]\n"
	"\tldp d8, d9, [sp, #96]\n\tldp d10, d11, [sp, #112]\n"
	"\tldp d12, d13, [sp, #128]\n\tldp d14, d15, [sp, #144]\n"
	"\tldp x29, x30, [sp], #160\n"
	"\tret\n"
	".globl clobber_and_longjmp\n"
	".type clobber_and_longjmp, %function\n"
	"clobber_and_longjmp:\n"
	"\tmov x19, #-1\n\tmov x20, #-2\n\tmov x21, #-3\n\tmov x22, #-4\n"
	"\tmov x23, #-5\n\tmov x24, #-6\n\tmov x25, #-7\n\tmov x26, #-8\n"
	"\tmov x27, #-9\n\tmov x28, #-10\n"
	"\tfmov d8, #-1.0\n\tfmov d9, #-1.0\n\tfmov d10, #-1.0\n\tfmov d11, #-1.0\n"
	"\tfmov d12, #-1.0\n\tfmov d13, #-1.0\n\tfmov d14, #-1.0\n\tfmov d15, #-1.0\n"
	"\tb longjmp\n");
#elif defined(__arm__)
/* r4 to r11 and d8 to d15, each set to a value of its own, then compared. */
__asm__(".text\n"
	".arm\n"
	".globl registers_survive\n"
	".type registers_survive, %function\n"
	"registers_survive:\n"
	"\tpush {r4-r11, lr}\n"
	"\tvpush {d8-d15}\n"
	"\tsub sp, sp, #396\n"
	"\tmov r4, #0x14\n\tmov r5, #0x15\n\tmov r6, #0x16\n\tmov r7, #0x17\n"
	"\tmov r8, #0x18\n\tmov r9, #0x19\n\tmov r10, #0x1a\n\tmov r11, #0x1b\n"
	"\tvmov.f64 d8, #1.0\n\tvmov.f64 d9, #2.0\n\tvmov.f64 d10, #3.0\n"
	"\tvmov.f64 d11, #4.0\n\tvmov.f64 d12, #5.0\n\tvmov.f64 d13, #6.0\n"
	"\tvmov.f64 d14, #7.0\n\tvmov.f64 d15, #8.0\n"
	"\tmov r0, sp\n"
	"\tbl setjmp\n"
	"\tcmp r0, #0\n"
	"\tbne 2f\n"
	"\tmov r0, sp\n"
	"\tmov r1, #5\n"
	"\tbl clobber_and_longjmp\n"
	"2:\tmov r1, r0\n"
	"\tmov r0, #0\n"
	"\tcmp r1, #5\n\tbne 3f\n"
	"\tcmp r4, #0x14\n\tbne 3f\n\tcmp r5, #0x15\n\tbne 3f\n"
	"\tcmp r6, #0x16\n\tbne 3f\n\tcmp r7, #0x17\n\tbne 3f\n"
	"\tcmp r8, #0x18\n\tbne 3f\n\tcmp r9, #0x19\n\tbne 3f\n"
	"\tcmp r10, #0x1a\n\tbne 3f\n\tcmp r11, #0x1b\n\tbne 3f\n"
	"\tvmov.f64 d0, #1.0\n\tvcmp.f64 d8, d0\n\tvmrs APSR_nzcv, fpscr\n\tbne 3f\n"
	"\tvmov.f64 d0, #2.0\n\tvcmp.f64 d9, d0\n\tvmrs APSR_nzcv, fpscr\n\tbne 3f\n"
	"\tvmov.f64 d0, #3.0\n\tvcmp.f64 d10, d0\n\tvmrs APSR_nzcv, fpscr\n\tbne 3f\n"
	"\tvmov.f64 d0, #4.0\n\tvcmp.f64 d11, d0\n\tvmrs APSR_nzcv, fpscr\n\tbne 3f\n"
	"\tvmov.f64 d0, #5.0\n\tvcmp.f64 d12, d0\n\tvmrs APSR_nzcv, fpscr\n\tbne 3f\n"
	"\tvmov.f64 d0, #6.0\n\tvcmp.f64 d13, d0\n\tvmrs APSR_nzcv, fpscr\n\tbne 3f\n"
	"\tvmov.f64 d0, #7.0\n\tvcmp.f64 d14, d0\n\tvmrs APSR_nzcv, fpscr\n\tbne 3f\n"
	"\tvmov.f64 d0, #8.0\n\tvcmp.f64 d15, d0\n\tvmrs APSR_nzcv, fpscr\n\tbne 3f\n"
	"\tmov r0, #1\n"
	"3:\tadd sp, sp, #396\n"
	"\tvpop {d8-d15}\n"
	"\tpop {r4-r11, pc}\n"
	".globl clobber_and_longjmp\n"
	".type clobber_and_longjmp, %function\n"
	"clobber_and_longjmp:\n"
	"\tmvn r4, #0\n\tmvn r5, #1\n\tmvn r6, #2\n\tmvn r7, #3\n"
	"\tmvn r8, #4\n\tmvn r9, #5\n\tmvn r10, #6\n\tmvn r11, #7\n"
	"\tvmov.f64 d8, #-1.0\n\tvmov.f64 d9, #-1.0\n\tvmov.f64 d10, #-1.0\n"
	"\tvmov.f64 d11, #-1.0\n\tvmov.f64 d12, #-1.0\n\tvmov.f64 d13, #-1.0\n"
	"\tvmov.f64 d14, #-1.0\n\tvmov.f64 d15, #-1.0\n"
	"\tb longjmp\n");
#endif

#if defined(__x86_64__)
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
#endif

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

	unsigned long frame = (unsigned long)__builtin_frame_address(0);
#if defined(__x86_64__)
	/*
	 * The saved stack pointer and return address are mangled with the
	 * pointer guard, and demangle to this frame and this function.
	 */
	if (setjmp(jb) == 0) {
		unsigned long sp = demangle(jb->__jb[6]);
		unsigned long pc = demangle(jb->__jb[7]);
		CHECK(pointer_guard() != 0);
		CHECK(jb->__jb[6] != sp);
		CHECK(sp < frame && frame - sp < 65536);
		CHECK(pc - (unsigned long)main + 65536 < 131072);
	}
#elif defined(__aarch64__)
	/* Nothing is mangled: sp and the return address sit as they are. */
	if (setjmp(jb) == 0) {
		unsigned long sp = jb->__jb[13];
		unsigned long pc = jb->__jb[11];
		CHECK(sp <= frame && frame - sp < 65536);
		CHECK(pc - (unsigned long)main + 65536 < 131072);
	}
#elif defined(__arm__)
	/* Nothing is mangled: sp and lr follow r4 to r11, eight words in. */
	if (setjmp(jb) == 0) {
		unsigned long *words = (unsigned long *)jb->__jb;
		unsigned long sp = words[8];
		unsigned long pc = words[9];
		CHECK(sp <= frame && frame - sp < 65536);
		CHECK((pc & ~1UL) - (unsigned long)main + 65536 < 131072);
	}
#endif

	return t_status;
}

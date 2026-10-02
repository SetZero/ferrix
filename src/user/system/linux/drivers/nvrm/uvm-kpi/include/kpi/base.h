/*
 * uvm-kpi: types, compiler idioms, errors, printing, bit operations, lists
 * and trees.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef FERRIX_KPI_BASE_H
#define FERRIX_KPI_BASE_H

/* ---- Configuration: the "kernel" uvm-kpi stands for. ---- */
#define CONFIG_X86_64 1
#define CONFIG_X86 1
#define CONFIG_64BIT 1
#define CONFIG_SMP 1
#define CONFIG_NUMA 1
#define CONFIG_MMU 1
#define BITS_PER_LONG 64
#define LINUX_VERSION_CODE KERNEL_VERSION(6, 8, 0)
#define KERNEL_VERSION(a, b, c) (((a) << 16) + ((b) << 8) + ((c) > 255 ? 255 : (c)))
#define UTS_RELEASE "6.8.0-ferrix"

/* ---- Limits (the freestanding <limits.h> needs the C library's). ---- */
#define CHAR_BIT 8
#define SCHAR_MAX 127
#define UCHAR_MAX 255
#define SHRT_MAX 32767
#define USHRT_MAX 65535
#define INT_MAX 2147483647
#define INT_MIN (-INT_MAX - 1)
#define UINT_MAX 4294967295U
#define LONG_MAX 9223372036854775807L
#define LONG_MIN (-LONG_MAX - 1L)
#define ULONG_MAX 18446744073709551615UL
#define LLONG_MAX 9223372036854775807LL
#define ULLONG_MAX 18446744073709551615ULL
#define CONFIG_PCI 1
#define CONFIG_PROC_FS 1
#define SMP_CACHE_BYTES 64
#define L1_CACHE_BYTES 64
#define ____cacheline_aligned __attribute__((aligned(SMP_CACHE_BYTES)))
#define ____cacheline_aligned_in_smp ____cacheline_aligned
#define __cacheline_aligned ____cacheline_aligned

/* ---- Integer types. ---- */
typedef int8_t s8;
typedef uint8_t u8;
typedef int16_t s16;
typedef uint16_t u16;
typedef int32_t s32;
typedef uint32_t u32;
typedef long long s64;
typedef unsigned long long u64;
typedef s8 __s8;
typedef u8 __u8;
typedef s16 __s16;
typedef u16 __u16;
typedef s32 __s32;
typedef u32 __u32;
typedef s64 __s64;
typedef u64 __u64;
typedef u16 __le16;
typedef u32 __le32;
typedef u64 __le64;
typedef u16 __be16;
typedef u32 __be32;
typedef u64 __be64;
typedef long ssize_t;
typedef long long loff_t;
typedef unsigned int uint;
typedef unsigned long ulong;
typedef unsigned short ushort;
typedef unsigned char uchar;
typedef int pid_t;
typedef unsigned int uid_t;
typedef unsigned int gid_t;
typedef unsigned int mode_t;
typedef unsigned short umode_t;
typedef unsigned int dev_t;
typedef u64 dma_addr_t;
typedef u64 phys_addr_t;
typedef u64 resource_size_t;
typedef unsigned int gfp_t;
typedef unsigned int fmode_t;
typedef long long time64_t;
typedef s64 ktime_t;
typedef unsigned int vm_fault_t;
typedef unsigned long pgoff_t;
typedef int irqreturn_t;
typedef struct { u64 val; } kuid_t;

/* ---- Compiler idioms. ---- */
#define __user
#define __iomem
#define __kernel
#define __force
#define __rcu
#define __percpu
#define __init
#define __exit
#define __initdata
#define __read_mostly
#define __must_check __attribute__((warn_unused_result))
#define __always_inline inline __attribute__((always_inline))
#define noinline __attribute__((noinline))
#define __maybe_unused __attribute__((unused))
#define __always_unused __attribute__((unused))
#define __packed __attribute__((packed))
#define __aligned(x) __attribute__((aligned(x)))
#define __printf(a, b) __attribute__((format(printf, a, b)))
#define __cold __attribute__((cold))
#define __noreturn __attribute__((noreturn))
#define __weak __attribute__((weak))
#define __section(s) __attribute__((section(s)))
#define __used __attribute__((used))
#define __pure __attribute__((pure))
#define __acquires(x)
#define __releases(x)
#define __must_hold(x)
#define fallthrough __attribute__((fallthrough))
#define notrace
#define asmlinkage
#define likely(x) __builtin_expect(!!(x), 1)
#define unlikely(x) __builtin_expect(!!(x), 0)
#define barrier() __asm__ __volatile__("" ::: "memory")
#define READ_ONCE(x) (*(const volatile __typeof__(x) *)&(x))
#define WRITE_ONCE(x, v) do { *(volatile __typeof__(x) *)&(x) = (v); } while (0)
#define ACCESS_ONCE(x) (*(volatile __typeof__(x) *)&(x))
#define offsetofend(T, m) (offsetof(T, m) + sizeof(((T *)0)->m))
#define container_of(ptr, type, member) \
    ((type *)((char *)(ptr) - offsetof(type, member)))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define BUILD_BUG_ON(c) _Static_assert(!(c), "BUILD_BUG_ON(" #c ")")
#define BUILD_BUG_ON_MSG(c, m) _Static_assert(!(c), m)
#define BUILD_BUG_ON_ZERO(c) (0)
#define BUILD_BUG() do { } while (0)
#define __stringify_1(x) #x
#define __stringify(x) __stringify_1(x)
#define typecheck(type, x) ({ type __d1; __typeof__(x) __d2; (void)(&__d1 == &__d2); 1; })
#define __same_type(a, b) __builtin_types_compatible_p(__typeof__(a), __typeof__(b))
#define __is_constexpr(x) __builtin_constant_p(x)
#define EXPORT_SYMBOL(s)
#define EXPORT_SYMBOL_GPL(s)
#define THIS_MODULE ((struct module *)0)
#define MODULE_LICENSE(s)
#define MODULE_INFO(a, b)
#define MODULE_VERSION(s)
#define MODULE_ALIAS_CHARDEV_MAJOR(m)
#define MODULE_SOFTDEP(s)
#define MODULE_IMPORT_NS(s)
#define MODULE_DESCRIPTION(s)
#define MODULE_AUTHOR(s)
/* The module's entry points, under names the host calls. */
#define module_init(f) int kpi_module_init(void) __attribute__((alias(#f)));
#define module_exit(f) void kpi_module_exit(void) __attribute__((alias(#f)));
struct module;

/* Module parameters register themselves, so the host can set them by name
 * before UVM starts (kpi_param_set). */
struct kpi_param {
    const char *name;
    char type; /* 'b' bool, 'i' int, 'u' uint, 'l' ulong, 's' charp */
    void *value;
};
#define KPI_PARAM(n, t, v)                                                        \
    static const struct kpi_param __kpi_param_##n                                  \
        __attribute__((used, section("kpi_params"), aligned(8))) = {#n, t, &(v)}
#define KPI_PARAM_TYPE_bool 'b'
#define KPI_PARAM_TYPE_int 'i'
#define KPI_PARAM_TYPE_uint 'u'
#define KPI_PARAM_TYPE_ulong 'l'
#define KPI_PARAM_TYPE_charp 's'
#define module_param(n, t, p) KPI_PARAM(n, KPI_PARAM_TYPE_##t, n)
#define module_param_named(n, v, t, p) KPI_PARAM(n, KPI_PARAM_TYPE_##t, v)
#define MODULE_PARM_DESC(n, d)
int kpi_param_set(const char *name, const char *value);

/* ---- Arithmetic helpers. ---- */
#define min(a, b) ({ __typeof__(a) _a = (a); __typeof__(b) _b = (b); _a < _b ? _a : _b; })
#define max(a, b) ({ __typeof__(a) _a = (a); __typeof__(b) _b = (b); _a > _b ? _a : _b; })
#define min3(a, b, c) min(min(a, b), c)
#define max3(a, b, c) max(max(a, b), c)
#define min_t(t, a, b) ({ t _a = (a); t _b = (b); _a < _b ? _a : _b; })
#define max_t(t, a, b) ({ t _a = (a); t _b = (b); _a > _b ? _a : _b; })
#define clamp(v, lo, hi) min(max(v, lo), hi)
#define clamp_t(t, v, lo, hi) min_t(t, max_t(t, v, lo), hi)
#define swap(a, b) do { __typeof__(a) _t = (a); (a) = (b); (b) = _t; } while (0)
#define abs(x) ({ __typeof__(x) _x = (x); _x < 0 ? -_x : _x; })
#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define DIV_ROUND_UP_ULL(n, d) DIV_ROUND_UP((unsigned long long)(n), (d))
#define roundup(x, y) ((((x) + ((y) - 1)) / (y)) * (y))
#define rounddown(x, y) ((x) - ((x) % (y)))
#define __ALIGN_MASK(x, m) (((x) + (m)) & ~(m))
#define ALIGN(x, a) __ALIGN_MASK(x, (__typeof__(x))(a) - 1)
#define ALIGN_DOWN(x, a) ((x) & ~((__typeof__(x))(a) - 1))
#define PTR_ALIGN(p, a) ((__typeof__(p))ALIGN((unsigned long)(p), (a)))
#define IS_ALIGNED(x, a) (((x) & ((__typeof__(x))(a) - 1)) == 0)
#define round_up(x, y) ((((x) - 1) | ((__typeof__(x))((y) - 1))) + 1)
#define round_down(x, y) ((x) & ~((__typeof__(x))((y) - 1)))
#define do_div(n, base) ({ u32 _b = (base); u32 _r = (u32)((n) % _b); (n) /= _b; _r; })
static inline u64 div_u64(u64 a, u32 b) { return a / b; }
static inline s64 div_s64(s64 a, s32 b) { return a / b; }
static inline u64 div64_u64(u64 a, u64 b) { return a / b; }
static inline u64 div_u64_rem(u64 a, u32 b, u32 *r) { *r = a % b; return a / b; }
#define U8_MAX ((u8)~0U)
#define U16_MAX ((u16)~0U)
#define U32_MAX ((u32)~0U)
#define U64_MAX ((u64)~0ULL)
#define S32_MAX ((s32)(U32_MAX >> 1))
#define S64_MAX ((s64)(U64_MAX >> 1))
#define upper_32_bits(n) ((u32)(((n) >> 16) >> 16))
#define lower_32_bits(n) ((u32)((n) & 0xffffffff))

/* ---- Errors. ---- */
#define EPERM 1
#define ENOENT 2
#define ESRCH 3
#define EINTR 4
#define EIO 5
#define ENXIO 6
#define E2BIG 7
#define ENOEXEC 8
#define EBADF 9
#define ECHILD 10
#define EAGAIN 11
#define ENOMEM 12
#define EACCES 13
#define EFAULT 14
#define EBUSY 16
#define EEXIST 17
#define EXDEV 18
#define ENODEV 19
#define ENOTDIR 20
#define EISDIR 21
#define EINVAL 22
#define ENFILE 23
#define EMFILE 24
#define ENOTTY 25
#define EFBIG 27
#define ENOSPC 28
#define ESPIPE 29
#define EROFS 30
#define EPIPE 32
#define EDOM 33
#define ERANGE 34
#define EDEADLK 35
#define ENAMETOOLONG 36
#define ENOSYS 38
#define ENODATA 61
#define ETIME 62
#define EPROTO 71
#define EOVERFLOW 75
#define EBADFD 77
#define EILSEQ 84
#define EOPNOTSUPP 95
#define EADDRINUSE 98
#define ETIMEDOUT 110
#define EALREADY 114
#define EINPROGRESS 115
#define ECANCELED 125
#define EHWPOISON 133
#define ERESTARTSYS 512
#define ENOTSUPP 524
#define MAX_ERRNO 4095
#define IS_ERR_VALUE(x) unlikely((unsigned long)(void *)(x) >= (unsigned long)-MAX_ERRNO)
static inline void *ERR_PTR(long e) { return (void *)e; }
static inline long PTR_ERR(const void *p) { return (long)p; }
static inline bool IS_ERR(const void *p) { return IS_ERR_VALUE((unsigned long)p); }
static inline bool IS_ERR_OR_NULL(const void *p) { return !p || IS_ERR(p); }

/* ---- Strings and memory (the C library's, declared here because UVM's
 *      units never see the library's headers). ---- */
void *memcpy(void *, const void *, size_t);
void *memmove(void *, const void *, size_t);
void *memset(void *, int, size_t);
int memcmp(const void *, const void *, size_t);
size_t strlen(const char *);
size_t strnlen(const char *, size_t);
int strcmp(const char *, const char *);
int strncmp(const char *, const char *, size_t);
char *strcpy(char *, const char *);
char *strncpy(char *, const char *, size_t);
char *strchr(const char *, int);
char *strrchr(const char *, int);
char *strstr(const char *, const char *);
size_t kpi_strlcpy(char *, const char *, size_t);
#define strlcpy kpi_strlcpy
size_t kpi_strscpy(char *, const char *, size_t);
#define strscpy(d, s, n) ((ssize_t)kpi_strscpy(d, s, n))
int sprintf(char *, const char *, ...) __printf(2, 3);
static inline const char *kbasename(const char *p) { const char *t = strrchr(p, '/'); return t ? t + 1 : p; }
int snprintf(char *, size_t, const char *, ...) __printf(3, 4);
int vsnprintf(char *, size_t, const char *, va_list);
int scnprintf(char *, size_t, const char *, ...) __printf(3, 4);
int sscanf(const char *, const char *, ...);
unsigned long simple_strtoul(const char *, char **, unsigned int);
int kstrtoint(const char *, unsigned int, int *);
int kstrtouint(const char *, unsigned int, unsigned int *);
int kstrtoul(const char *, unsigned int, unsigned long *);
int kstrtoull(const char *, unsigned int, unsigned long long *);
#define memset_io(p, c, n) memset((void *)(p), c, n)
#define memcpy_fromio(d, s, n) memcpy(d, (const void *)(s), n)
#define memcpy_toio(d, s, n) memcpy((void *)(d), s, n)
static inline int isspace(int c) { return c == ' ' || (c >= '\t' && c <= '\r'); }
static inline int isdigit(int c) { return c >= '0' && c <= '9'; }
static inline int isxdigit(int c) { return isdigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'); }
static inline int isalpha(int c) { return (c | 32) >= 'a' && (c | 32) <= 'z'; }
static inline int isupper(int c) { return c >= 'A' && c <= 'Z'; }
static inline int tolower(int c) { return isupper(c) ? c + 32 : c; }
static inline int toupper(int c) { return (c >= 'a' && c <= 'z') ? c - 32 : c; }
void sort(void *base, size_t num, size_t size,
          int (*cmp)(const void *, const void *),
          void (*swap)(void *, void *, int));

/* ---- Printing, assertions. ---- */
#define KERN_SOH "\001"
#define KERN_EMERG KERN_SOH "0"
#define KERN_ALERT KERN_SOH "1"
#define KERN_CRIT KERN_SOH "2"
#define KERN_ERR KERN_SOH "3"
#define KERN_WARNING KERN_SOH "4"
#define KERN_NOTICE KERN_SOH "5"
#define KERN_INFO KERN_SOH "6"
#define KERN_DEBUG KERN_SOH "7"
#define KERN_CONT KERN_SOH "c"
#define KERN_DEFAULT ""
int printk(const char *fmt, ...) __printf(1, 2);
int vprintk(const char *fmt, va_list);
#ifndef pr_fmt
#define pr_fmt(fmt) fmt
#endif
#define pr_emerg(fmt, ...) printk(KERN_EMERG pr_fmt(fmt), ##__VA_ARGS__)
#define pr_alert(fmt, ...) printk(KERN_ALERT pr_fmt(fmt), ##__VA_ARGS__)
#define pr_crit(fmt, ...) printk(KERN_CRIT pr_fmt(fmt), ##__VA_ARGS__)
#define pr_err(fmt, ...) printk(KERN_ERR pr_fmt(fmt), ##__VA_ARGS__)
#define pr_warn(fmt, ...) printk(KERN_WARNING pr_fmt(fmt), ##__VA_ARGS__)
#define pr_warning pr_warn
#define pr_notice(fmt, ...) printk(KERN_NOTICE pr_fmt(fmt), ##__VA_ARGS__)
#define pr_info(fmt, ...) printk(KERN_INFO pr_fmt(fmt), ##__VA_ARGS__)
#define pr_cont(fmt, ...) printk(KERN_CONT fmt, ##__VA_ARGS__)
#define pr_devel(fmt, ...) do { if (0) printk(fmt, ##__VA_ARGS__); } while (0)
#define pr_debug(fmt, ...) do { if (0) printk(fmt, ##__VA_ARGS__); } while (0)
#define pr_info_ratelimited pr_info
#define pr_err_ratelimited pr_err
#define printk_ratelimited printk
#define printk_ratelimit() 1
struct ratelimit_state { int interval, burst, printed, missed; u64 begin; };
#define DEFINE_RATELIMIT_STATE(n, i, b) struct ratelimit_state n = {(i), (b), 0, 0, 0}
#define RATELIMIT_STATE_INIT(n, i, b) {(i), (b), 0, 0, 0}
#define DEFAULT_RATELIMIT_INTERVAL (5 * HZ)
#define DEFAULT_RATELIMIT_BURST 10
int ___ratelimit(struct ratelimit_state *, const char *);
#define __ratelimit(s) ___ratelimit(s, __func__)
void dump_stack(void);
__noreturn void panic(const char *fmt, ...) __printf(1, 2);
__noreturn void kpi_bug(const char *file, int line);
#define BUG() kpi_bug(__FILE__, __LINE__)
#define BUG_ON(c) do { if (unlikely(c)) BUG(); } while (0)
void kpi_warn(const char *file, int line);
#define WARN_ON(c) ({ int _c = !!(c); if (unlikely(_c)) kpi_warn(__FILE__, __LINE__); unlikely(_c); })
#define WARN_ON_ONCE(c) WARN_ON(c)
#define WARN(c, fmt, ...) ({ int _c = !!(c); if (unlikely(_c)) { printk(fmt, ##__VA_ARGS__); kpi_warn(__FILE__, __LINE__); } unlikely(_c); })
#define WARN_ONCE WARN
#define VM_BUG_ON(c) do { } while (0)
#define VM_WARN_ON(c) do { } while (0)

/* ---- Bits. ---- */
#define BIT(n) (1UL << (n))
#define BIT_ULL(n) (1ULL << (n))
#define BIT_MASK(n) (1UL << ((n) % BITS_PER_LONG))
#define BIT_WORD(n) ((n) / BITS_PER_LONG)
#define BITS_PER_BYTE 8
#define BITS_TO_LONGS(n) DIV_ROUND_UP(n, BITS_PER_LONG)
#define BITS_PER_TYPE(t) (sizeof(t) * BITS_PER_BYTE)
#define GENMASK(h, l) (((~0UL) << (l)) & (~0UL >> (BITS_PER_LONG - 1 - (h))))
#define GENMASK_ULL(h, l) (((~0ULL) << (l)) & (~0ULL >> (63 - (h))))
#define DECLARE_BITMAP(n, bits) unsigned long n[BITS_TO_LONGS(bits)]

static inline void set_bit(long n, volatile unsigned long *a) { __atomic_fetch_or(&a[BIT_WORD(n)], BIT_MASK(n), __ATOMIC_SEQ_CST); }
static inline void clear_bit(long n, volatile unsigned long *a) { __atomic_fetch_and(&a[BIT_WORD(n)], ~BIT_MASK(n), __ATOMIC_SEQ_CST); }
static inline void change_bit(long n, volatile unsigned long *a) { __atomic_fetch_xor(&a[BIT_WORD(n)], BIT_MASK(n), __ATOMIC_SEQ_CST); }
static inline void __set_bit(long n, volatile unsigned long *a) { a[BIT_WORD(n)] |= BIT_MASK(n); }
static inline void __clear_bit(long n, volatile unsigned long *a) { a[BIT_WORD(n)] &= ~BIT_MASK(n); }
static inline void __change_bit(long n, volatile unsigned long *a) { a[BIT_WORD(n)] ^= BIT_MASK(n); }
static inline bool test_bit(long n, const volatile unsigned long *a) { return (a[BIT_WORD(n)] >> (n % BITS_PER_LONG)) & 1; }
static inline bool test_and_set_bit(long n, volatile unsigned long *a) { return (__atomic_fetch_or(&a[BIT_WORD(n)], BIT_MASK(n), __ATOMIC_SEQ_CST) & BIT_MASK(n)) != 0; }
static inline bool test_and_clear_bit(long n, volatile unsigned long *a) { return (__atomic_fetch_and(&a[BIT_WORD(n)], ~BIT_MASK(n), __ATOMIC_SEQ_CST) & BIT_MASK(n)) != 0; }
static inline bool __test_and_set_bit(long n, volatile unsigned long *a) { bool o = test_bit(n, a); __set_bit(n, a); return o; }
static inline bool __test_and_clear_bit(long n, volatile unsigned long *a) { bool o = test_bit(n, a); __clear_bit(n, a); return o; }
static inline bool test_and_set_bit_lock(long n, volatile unsigned long *a) { return test_and_set_bit(n, a); }
static inline void clear_bit_unlock(long n, volatile unsigned long *a) { __atomic_thread_fence(__ATOMIC_RELEASE); clear_bit(n, a); }
#define smp_mb__before_atomic() __atomic_thread_fence(__ATOMIC_SEQ_CST)
#define smp_mb__after_atomic() __atomic_thread_fence(__ATOMIC_SEQ_CST)

static inline unsigned long __ffs(unsigned long w) { return __builtin_ctzl(w); }
static inline unsigned long __fls(unsigned long w) { return BITS_PER_LONG - 1 - __builtin_clzl(w); }
static inline int ffs(int x) { return __builtin_ffs(x); }
static inline int fls(unsigned int x) { return x ? 32 - __builtin_clz(x) : 0; }
static inline int fls64(u64 x) { return x ? 64 - __builtin_clzll(x) : 0; }
static inline unsigned long ffz(unsigned long w) { return __ffs(~w); }
static inline unsigned int hweight32(u32 w) { return __builtin_popcount(w); }
static inline unsigned long hweight64(u64 w) { return __builtin_popcountll(w); }
static inline unsigned long hweight_long(unsigned long w) { return __builtin_popcountl(w); }
/* Constant-foldable, so they may size bit-fields and arrays. */
#define ilog2(n) ((int)(63 - __builtin_clzll((u64)(n) | 1)))
#define order_base_2(n) ((n) > 1 ? ilog2((u64)(n) - 1) + 1 : 0)
#define hweight8(w) __builtin_popcount((u8)(w))
#define hweight16(w) __builtin_popcount((u16)(w))
#define BUILD_BUG_ON_NOT_POWER_OF_2(n) BUILD_BUG_ON((n) == 0 || (((n) & ((n) - 1)) != 0))
static inline bool is_power_of_2(unsigned long n) { return n != 0 && (n & (n - 1)) == 0; }
#define roundup_pow_of_two(n) ((unsigned long)(n) <= 1 ? 1UL : 1UL << (64 - __builtin_clzll((u64)(n) - 1)))
static inline unsigned long rounddown_pow_of_two(unsigned long n) { return 1UL << ilog2(n); }
static inline int get_order(unsigned long size) { return size <= 4096 ? 0 : fls64((size - 1) >> 12); }
static inline u32 rol32(u32 w, unsigned int s) { return (w << (s & 31)) | (w >> ((-s) & 31)); }

unsigned long find_first_bit(const unsigned long *, unsigned long);
unsigned long find_first_zero_bit(const unsigned long *, unsigned long);
unsigned long find_last_bit(const unsigned long *, unsigned long);
unsigned long find_next_bit(const unsigned long *, unsigned long, unsigned long);
unsigned long find_next_zero_bit(const unsigned long *, unsigned long, unsigned long);
#define for_each_set_bit(b, a, n) \
    for ((b) = find_first_bit((a), (n)); (b) < (n); (b) = find_next_bit((a), (n), (b) + 1))
#define for_each_clear_bit(b, a, n) \
    for ((b) = find_first_zero_bit((a), (n)); (b) < (n); (b) = find_next_zero_bit((a), (n), (b) + 1))
#define for_each_set_bit_from(b, a, n) \
    for ((b) = find_next_bit((a), (n), (b)); (b) < (n); (b) = find_next_bit((a), (n), (b) + 1))
#define for_each_clear_bit_from(b, a, n) \
    for ((b) = find_next_zero_bit((a), (n), (b)); (b) < (n); (b) = find_next_zero_bit((a), (n), (b) + 1))

void bitmap_zero(unsigned long *, unsigned int);
void bitmap_fill(unsigned long *, unsigned int);
void bitmap_copy(unsigned long *, const unsigned long *, unsigned int);
bool bitmap_and(unsigned long *, const unsigned long *, const unsigned long *, unsigned int);
bool bitmap_andnot(unsigned long *, const unsigned long *, const unsigned long *, unsigned int);
void bitmap_or(unsigned long *, const unsigned long *, const unsigned long *, unsigned int);
void bitmap_xor(unsigned long *, const unsigned long *, const unsigned long *, unsigned int);
void bitmap_complement(unsigned long *, const unsigned long *, unsigned int);
bool bitmap_equal(const unsigned long *, const unsigned long *, unsigned int);
bool bitmap_intersects(const unsigned long *, const unsigned long *, unsigned int);
bool bitmap_subset(const unsigned long *, const unsigned long *, unsigned int);
bool bitmap_empty(const unsigned long *, unsigned int);
bool bitmap_full(const unsigned long *, unsigned int);
unsigned int bitmap_weight(const unsigned long *, unsigned int);
void bitmap_set(unsigned long *, unsigned int start, unsigned int n);
void bitmap_clear(unsigned long *, unsigned int start, unsigned int n);
void bitmap_shift_left(unsigned long *, const unsigned long *, unsigned int shift, unsigned int n);
void bitmap_shift_right(unsigned long *, const unsigned long *, unsigned int shift, unsigned int n);

/* ---- Doubly linked lists. ---- */
struct list_head { struct list_head *next, *prev; };
struct hlist_node { struct hlist_node *next, **pprev; };
struct hlist_head { struct hlist_node *first; };
#define LIST_HEAD_INIT(n) { &(n), &(n) }
#define LIST_HEAD(n) struct list_head n = LIST_HEAD_INIT(n)
static inline void INIT_LIST_HEAD(struct list_head *l) { l->next = l; l->prev = l; }
static inline void __list_add(struct list_head *n, struct list_head *p, struct list_head *x) { x->prev = n; n->next = x; n->prev = p; p->next = n; }
static inline void list_add(struct list_head *n, struct list_head *h) { __list_add(n, h, h->next); }
static inline void list_add_tail(struct list_head *n, struct list_head *h) { __list_add(n, h->prev, h); }
static inline void __list_del(struct list_head *p, struct list_head *n) { n->prev = p; p->next = n; }
static inline void list_del(struct list_head *e) { __list_del(e->prev, e->next); e->next = (void *)0x100; e->prev = (void *)0x122; }
static inline void list_del_init(struct list_head *e) { __list_del(e->prev, e->next); INIT_LIST_HEAD(e); }
static inline void list_move(struct list_head *e, struct list_head *h) { __list_del(e->prev, e->next); list_add(e, h); }
static inline void list_move_tail(struct list_head *e, struct list_head *h) { __list_del(e->prev, e->next); list_add_tail(e, h); }
static inline void list_replace(struct list_head *o, struct list_head *n) { n->next = o->next; n->next->prev = n; n->prev = o->prev; n->prev->next = n; }
static inline void list_replace_init(struct list_head *o, struct list_head *n) { list_replace(o, n); INIT_LIST_HEAD(o); }
static inline bool list_empty(const struct list_head *h) { return READ_ONCE(h->next) == h; }
static inline bool list_empty_careful(const struct list_head *h) { return h->next == h && h->prev == h; }
static inline bool list_is_last(const struct list_head *l, const struct list_head *h) { return l->next == h; }
static inline bool list_is_first(const struct list_head *l, const struct list_head *h) { return l->prev == h; }
static inline bool list_is_singular(const struct list_head *h) { return !list_empty(h) && h->next == h->prev; }
static inline void __list_splice(const struct list_head *l, struct list_head *p, struct list_head *n) { struct list_head *f = l->next, *t = l->prev; f->prev = p; p->next = f; t->next = n; n->prev = t; }
static inline void list_splice(const struct list_head *l, struct list_head *h) { if (!list_empty(l)) __list_splice(l, h, h->next); }
static inline void list_splice_tail(struct list_head *l, struct list_head *h) { if (!list_empty(l)) __list_splice(l, h->prev, h); }
static inline void list_splice_init(struct list_head *l, struct list_head *h) { if (!list_empty(l)) { __list_splice(l, h, h->next); INIT_LIST_HEAD(l); } }
static inline void list_splice_tail_init(struct list_head *l, struct list_head *h) { if (!list_empty(l)) { __list_splice(l, h->prev, h); INIT_LIST_HEAD(l); } }
static inline void list_cut_position(struct list_head *l, struct list_head *h, struct list_head *e)
{
    if (list_empty(h) || (list_is_singular(h) && h->next != e && h != e)) return;
    if (e == h) { INIT_LIST_HEAD(l); return; }
    l->next = h->next; l->next->prev = l; l->prev = e; h->next = e->next; h->next->prev = h; e->next = l;
}
static inline size_t list_count_nodes(struct list_head *h) { size_t n = 0; struct list_head *p; for (p = h->next; p != h; p = p->next) n++; return n; }
#define list_entry(p, t, m) container_of(p, t, m)
#define list_first_entry(p, t, m) list_entry((p)->next, t, m)
#define list_last_entry(p, t, m) list_entry((p)->prev, t, m)
#define list_first_entry_or_null(p, t, m) ({ struct list_head *_h = (p), *_n = READ_ONCE(_h->next); _n != _h ? list_entry(_n, t, m) : NULL; })
#define list_next_entry(p, m) list_entry((p)->m.next, __typeof__(*(p)), m)
#define list_prev_entry(p, m) list_entry((p)->m.prev, __typeof__(*(p)), m)
#define list_entry_is_head(p, h, m) (&(p)->m == (h))
#define list_for_each(p, h) for (p = (h)->next; p != (h); p = p->next)
#define list_for_each_safe(p, n, h) for (p = (h)->next, n = p->next; p != (h); p = n, n = p->next)
#define list_for_each_entry(p, h, m) \
    for (p = list_first_entry(h, __typeof__(*p), m); !list_entry_is_head(p, h, m); p = list_next_entry(p, m))
#define list_for_each_entry_reverse(p, h, m) \
    for (p = list_last_entry(h, __typeof__(*p), m); !list_entry_is_head(p, h, m); p = list_prev_entry(p, m))
#define list_for_each_entry_safe(p, n, h, m) \
    for (p = list_first_entry(h, __typeof__(*p), m), n = list_next_entry(p, m); !list_entry_is_head(p, h, m); p = n, n = list_next_entry(n, m))
#define list_for_each_entry_safe_reverse(p, n, h, m) \
    for (p = list_last_entry(h, __typeof__(*p), m), n = list_prev_entry(p, m); !list_entry_is_head(p, h, m); p = n, n = list_prev_entry(n, m))
#define list_for_each_entry_continue(p, h, m) \
    for (p = list_next_entry(p, m); !list_entry_is_head(p, h, m); p = list_next_entry(p, m))
#define list_for_each_entry_from(p, h, m) \
    for (; !list_entry_is_head(p, h, m); p = list_next_entry(p, m))
#define list_for_each_entry_safe_from(p, n, h, m) \
    for (n = list_next_entry(p, m); !list_entry_is_head(p, h, m); p = n, n = list_next_entry(n, m))
#define list_for_each_entry_rcu(p, h, m, ...) list_for_each_entry(p, h, m)
#define list_add_rcu list_add
#define list_add_tail_rcu list_add_tail
#define list_del_rcu list_del
#define INIT_HLIST_HEAD(h) ((h)->first = NULL)
#define HLIST_HEAD_INIT { .first = NULL }
static inline void INIT_HLIST_NODE(struct hlist_node *n) { n->next = NULL; n->pprev = NULL; }
static inline bool hlist_unhashed(const struct hlist_node *n) { return !n->pprev; }
static inline bool hlist_empty(const struct hlist_head *h) { return !READ_ONCE(h->first); }
static inline void hlist_add_head(struct hlist_node *n, struct hlist_head *h) { struct hlist_node *f = h->first; n->next = f; if (f) f->pprev = &n->next; h->first = n; n->pprev = &h->first; }
static inline void hlist_del(struct hlist_node *n) { struct hlist_node *x = n->next; struct hlist_node **p = n->pprev; *p = x; if (x) x->pprev = p; n->next = NULL; n->pprev = NULL; }
static inline void hlist_del_init(struct hlist_node *n) { if (!hlist_unhashed(n)) hlist_del(n); }
#define hlist_entry(p, t, m) container_of(p, t, m)
#define hlist_entry_safe(p, t, m) ({ __typeof__(p) _p = (p); _p ? hlist_entry(_p, t, m) : NULL; })
#define hlist_for_each_entry(p, h, m) \
    for (p = hlist_entry_safe((h)->first, __typeof__(*(p)), m); p; p = hlist_entry_safe((p)->m.next, __typeof__(*(p)), m))
#define hlist_for_each_entry_safe(p, n, h, m) \
    for (p = hlist_entry_safe((h)->first, __typeof__(*p), m); p && ({ n = p->m.next; 1; }); p = hlist_entry_safe(n, __typeof__(*p), m))

/* ---- Red-black trees: an implementation of uvm-kpi's own (kpi/rbtree.c). ---- */
struct rb_node {
    unsigned long __rb_parent_color;
    struct rb_node *rb_right;
    struct rb_node *rb_left;
} __attribute__((aligned(sizeof(long))));
struct rb_root { struct rb_node *rb_node; };
struct rb_root_cached { struct rb_root rb_root; struct rb_node *rb_leftmost; };
#define RB_ROOT (struct rb_root){ NULL }
#define RB_ROOT_CACHED (struct rb_root_cached){ { NULL }, NULL }
#define rb_parent(r) ((struct rb_node *)((r)->__rb_parent_color & ~3UL))
#define rb_entry(p, t, m) container_of(p, t, m)
#define rb_entry_safe(p, t, m) ({ __typeof__(p) _p = (p); _p ? rb_entry(_p, t, m) : NULL; })
#define RB_EMPTY_ROOT(r) (READ_ONCE((r)->rb_node) == NULL)
#define RB_EMPTY_NODE(n) ((n)->__rb_parent_color == (unsigned long)(n))
#define RB_CLEAR_NODE(n) ((n)->__rb_parent_color = (unsigned long)(n))
static inline void rb_link_node(struct rb_node *n, struct rb_node *parent, struct rb_node **link)
{ n->__rb_parent_color = (unsigned long)parent; n->rb_left = n->rb_right = NULL; *link = n; }
void rb_insert_color(struct rb_node *, struct rb_root *);
void rb_erase(struct rb_node *, struct rb_root *);
struct rb_node *rb_next(const struct rb_node *);
struct rb_node *rb_prev(const struct rb_node *);
struct rb_node *rb_first(const struct rb_root *);
struct rb_node *rb_last(const struct rb_root *);
void rb_replace_node(struct rb_node *victim, struct rb_node *n, struct rb_root *root);

/* ---- Radix tree (a sorted map, kpi/radix.c). ---- */
struct radix_tree_root { void *kpi_map; gfp_t gfp; };
#define RADIX_TREE_INIT(mask) { NULL, (mask) }
#define RADIX_TREE(n, m) struct radix_tree_root n = RADIX_TREE_INIT(m)
#define INIT_RADIX_TREE(r, m) do { (r)->kpi_map = NULL; (r)->gfp = (m); } while (0)
int radix_tree_insert(struct radix_tree_root *, unsigned long, void *);
void *radix_tree_lookup(const struct radix_tree_root *, unsigned long);
void *radix_tree_delete(struct radix_tree_root *, unsigned long);
unsigned int radix_tree_gang_lookup(const struct radix_tree_root *, void **, unsigned long first, unsigned int max);
struct radix_tree_iter { unsigned long index; };
void **kpi_radix_next_slot(const struct radix_tree_root *, struct radix_tree_iter *, unsigned long start);
#define radix_tree_for_each_slot(slot, root, iter, start) \
    for ((slot) = kpi_radix_next_slot(root, iter, start); (slot); (slot) = kpi_radix_next_slot(root, iter, (iter)->index + 1))
static inline bool radix_tree_empty(const struct radix_tree_root *r) { return r->kpi_map == NULL; }
static inline int radix_tree_preload(gfp_t g) { (void)g; return 0; }
static inline void radix_tree_preload_end(void) { }

/* ---- Hashing. ---- */
static inline u32 jhash(const void *key, u32 length, u32 initval)
{
    /* FNV-1a; UVM only needs a well-spread hash, not Linux's values. */
    const u8 *k = key; u32 h = 2166136261u ^ initval;
    while (length--) { h ^= *k++; h *= 16777619u; }
    return h;
}
static inline u32 jhash_1word(u32 a, u32 initval) { return jhash(&a, sizeof(a), initval); }
static inline u32 jhash_2words(u32 a, u32 b, u32 initval) { u32 v[2] = {a, b}; return jhash(v, sizeof(v), initval); }
static inline u32 hash_32(u32 v, unsigned int bits) { return (v * 0x61C88647u) >> (32 - bits); }
static inline u32 hash_64(u64 v, unsigned int bits) { return (u32)((v * 0x61C8864680B583EBull) >> (64 - bits)); }
#define hash_long(v, b) hash_64((u64)(v), b)
#define hash_ptr(p, b) hash_64((u64)(unsigned long)(p), b)

/* ---- Byte order (x86-64 is little-endian). ---- */
#define cpu_to_le32(x) ((u32)(x))
#define le32_to_cpu(x) ((u32)(x))
#define cpu_to_le64(x) ((u64)(x))
#define le64_to_cpu(x) ((u64)(x))

#endif

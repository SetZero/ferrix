/*
 * The C library and system calls the kpi implementation uses.
 *
 * kpi's own units are compiled like UVM's, against uvm-kpi and not the C
 * library's headers, so that both see one definition of every kernel type.
 * These are the few library functions they call, with the prototypes of
 * the x86-64 Linux ABI, which glibc and ferrousli share.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef FERRIX_KPI_LIBC_H
#define FERRIX_KPI_LIBC_H

void *malloc(size_t);
void *calloc(size_t, size_t);
void *realloc(void *, size_t);
void free(void *);
int posix_memalign(void **, size_t, size_t);
void abort(void) __noreturn;
long syscall(long, ...);
int sched_yield(void);
int *__errno_location(void);
long write(int, const void *, size_t);
void *mmap(void *, size_t, int, int, int, long);
int munmap(void *, size_t);
int ftruncate(int, long);
int memfd_create(const char *, unsigned int);
long getrandom(void *, size_t, unsigned int);
int get_nprocs(void);
int sched_getcpu(void);
int getpid(void);
int clock_nanosleep(int, int, const struct timespec64 *, struct timespec64 *);
int clock_gettime(int, struct timespec64 *);
void qsort(void *, size_t, size_t, int (*)(const void *, const void *));
unsigned long strtoul(const char *, char **, int);
long strtol(const char *, char **, int);
unsigned long long strtoull(const char *, char **, int);

typedef unsigned long kpi_pthread_t;
int pthread_create(kpi_pthread_t *, const void *, void *(*)(void *), void *);
int pthread_join(kpi_pthread_t, void **);
int pthread_detach(kpi_pthread_t);

#define KPI_PROT_NONE 0
#define KPI_PROT_READ 1
#define KPI_PROT_WRITE 2
#define KPI_MAP_SHARED 0x01
#define KPI_MAP_PRIVATE 0x02
#define KPI_MAP_FIXED 0x10
#define KPI_MAP_ANONYMOUS 0x20
#define KPI_MAP_NORESERVE 0x4000
#define KPI_MAP_FAILED ((void *)-1)
#define KPI_CLOCK_REALTIME 0
#define KPI_CLOCK_MONOTONIC 1
#define KPI_SYS_futex 202
#define KPI_SYS_gettid 186
#define KPI_FUTEX_WAIT_PRIVATE 128
#define KPI_FUTEX_WAKE_PRIVATE 129
#define KPI_MFD_CLOEXEC 1U

#endif

/*
 * uvm-kpi: atomics, locks, waiting, threads, work queues and time.
 *
 * The locks are futex-based (kpi/sync.c). A spinlock is a lock that
 * spins briefly and then sleeps: in nvrm nothing runs with interrupts off,
 * the interrupt "context" is the interrupt thread, so a holder may be
 * preempted and a waiter must not spin forever.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef FERRIX_KPI_SYNC_H
#define FERRIX_KPI_SYNC_H

/* ---- Atomics. ---- */
typedef struct { int counter; } atomic_t;
typedef struct { s64 counter; } atomic64_t;
typedef struct { long counter; } atomic_long_t;
#define ATOMIC_INIT(i) { (i) }
#define ATOMIC64_INIT(i) { (i) }
#define ATOMIC_LONG_INIT(i) { (i) }

#define KPI_ATOMIC_OPS(pfx, T, V)                                                                       \
    static inline V pfx##_read(const T *v) { return __atomic_load_n(&v->counter, __ATOMIC_RELAXED); }   \
    static inline void pfx##_set(T *v, V i) { __atomic_store_n(&v->counter, i, __ATOMIC_RELAXED); }     \
    static inline V pfx##_read_acquire(const T *v) { return __atomic_load_n(&v->counter, __ATOMIC_ACQUIRE); } \
    static inline void pfx##_set_release(T *v, V i) { __atomic_store_n(&v->counter, i, __ATOMIC_RELEASE); } \
    static inline void pfx##_add(V i, T *v) { __atomic_fetch_add(&v->counter, i, __ATOMIC_RELAXED); }   \
    static inline void pfx##_sub(V i, T *v) { __atomic_fetch_sub(&v->counter, i, __ATOMIC_RELAXED); }   \
    static inline void pfx##_inc(T *v) { pfx##_add(1, v); }                                              \
    static inline void pfx##_dec(T *v) { pfx##_sub(1, v); }                                              \
    static inline V pfx##_add_return(V i, T *v) { return __atomic_add_fetch(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline V pfx##_sub_return(V i, T *v) { return __atomic_sub_fetch(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline V pfx##_inc_return(T *v) { return pfx##_add_return(1, v); }                           \
    static inline V pfx##_dec_return(T *v) { return pfx##_sub_return(1, v); }                           \
    static inline V pfx##_fetch_add(V i, T *v) { return __atomic_fetch_add(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline V pfx##_fetch_sub(V i, T *v) { return __atomic_fetch_sub(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline V pfx##_fetch_or(V i, T *v) { return __atomic_fetch_or(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline V pfx##_fetch_and(V i, T *v) { return __atomic_fetch_and(&v->counter, i, __ATOMIC_SEQ_CST); } \
    static inline void pfx##_or(V i, T *v) { __atomic_fetch_or(&v->counter, i, __ATOMIC_SEQ_CST); }      \
    static inline void pfx##_and(V i, T *v) { __atomic_fetch_and(&v->counter, i, __ATOMIC_SEQ_CST); }    \
    static inline bool pfx##_dec_and_test(T *v) { return pfx##_sub_return(1, v) == 0; }                  \
    static inline bool pfx##_inc_and_test(T *v) { return pfx##_add_return(1, v) == 0; }                  \
    static inline bool pfx##_sub_and_test(V i, T *v) { return pfx##_sub_return(i, v) == 0; }             \
    static inline V pfx##_xchg(T *v, V n) { return __atomic_exchange_n(&v->counter, n, __ATOMIC_SEQ_CST); } \
    static inline V pfx##_cmpxchg(T *v, V o, V n)                                                       \
    { __atomic_compare_exchange_n(&v->counter, &o, n, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); return o; } \
    static inline bool pfx##_try_cmpxchg(T *v, V *o, V n)                                               \
    { return __atomic_compare_exchange_n(&v->counter, o, n, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); } \
    static inline V pfx##_add_unless(T *v, V a, V u)                                                    \
    { V c = pfx##_read(v); while (c != u && !pfx##_try_cmpxchg(v, &c, c + a)) { } return c; }            \
    static inline bool pfx##_inc_not_zero(T *v) { return pfx##_add_unless(v, 1, 0) != 0; }
KPI_ATOMIC_OPS(atomic, atomic_t, int)
#define atomic_dec_if_positive(v) ({ int _c = atomic_read(v); while (_c > 0 && !atomic_try_cmpxchg(v, &_c, _c - 1)) { } _c - 1; })
KPI_ATOMIC_OPS(atomic64, atomic64_t, s64)
KPI_ATOMIC_OPS(atomic_long, atomic_long_t, long)
#undef KPI_ATOMIC_OPS

#define xchg(p, n) __atomic_exchange_n((p), (n), __ATOMIC_SEQ_CST)
#define cmpxchg(p, o, n) ({ __typeof__(*(p)) _o = (o); __atomic_compare_exchange_n((p), &_o, (n), false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); _o; })
#define try_cmpxchg(p, po, n) __atomic_compare_exchange_n((p), (po), (n), false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)
#define smp_mb() __atomic_thread_fence(__ATOMIC_SEQ_CST)
#define smp_rmb() __atomic_thread_fence(__ATOMIC_ACQUIRE)
#define smp_wmb() __atomic_thread_fence(__ATOMIC_RELEASE)
#define mb() __asm__ __volatile__("mfence" ::: "memory")
#define rmb() __asm__ __volatile__("lfence" ::: "memory")
#define wmb() __asm__ __volatile__("sfence" ::: "memory")
#define dma_rmb() barrier()
#define dma_wmb() barrier()
#define smp_load_acquire(p) __atomic_load_n((p), __ATOMIC_ACQUIRE)
#define smp_store_release(p, v) __atomic_store_n((p), (v), __ATOMIC_RELEASE)
#define smp_store_mb(v, x) do { WRITE_ONCE(v, x); smp_mb(); } while (0)
#define cpu_relax() __asm__ __volatile__("pause" ::: "memory")

/* ---- The current task. ---- */
#define TASK_COMM_LEN 16
#define TASK_RUNNING 0
#define TASK_INTERRUPTIBLE 1
#define TASK_UNINTERRUPTIBLE 2
#define TASK_KILLABLE 0x102
#define TASK_IDLE 0x402
#define PF_KTHREAD 0x00200000
#define PF_MEMALLOC 0x00000800
#define PF_EXITING 0x00000004
struct mm_struct;
struct files_struct;
struct nsproxy;
struct task_struct {
    pid_t pid;
    pid_t tgid;
    unsigned int flags;
    char comm[TASK_COMM_LEN];
    struct mm_struct *mm;
    struct files_struct *files;
    struct nsproxy *nsproxy;
    int state;
    void *kpi_thread;   /* the host thread behind it */
    void *kpi_kthread;  /* kthread_create's state */
    struct task_struct *group_leader;
    void *stack;
};
#define THREAD_SIZE (16 * 1024)
#define KSTK_EIP(t) 0UL
#define task_stack_page(t) ((t)->stack)
struct task_struct *kpi_current(void);
#define current kpi_current()
#define get_current() kpi_current()
static inline pid_t task_pid_nr(struct task_struct *t) { return t->pid; }
static inline pid_t task_tgid_nr(struct task_struct *t) { return t->tgid; }
#define task_pid_vnr task_pid_nr
#define task_tgid_vnr task_tgid_nr
static inline bool fatal_signal_pending(struct task_struct *t) { (void)t; return false; }
static inline bool signal_pending(struct task_struct *t) { (void)t; return false; }
static inline void get_task_struct(struct task_struct *t) { (void)t; }
static inline void put_task_struct(struct task_struct *t) { (void)t; }
#define in_interrupt() 0
#define in_irq() 0
#define in_atomic() 0
#define in_task() 1
#define irqs_disabled() 0
#define preempt_disable() barrier()
#define preempt_enable() barrier()
#define preempt_enable_no_resched() barrier()
#define preemptible() 1
#define preempt_count() 0
#define might_sleep() do { } while (0)
#define might_sleep_if(c) do { } while (0)
#define might_resched() do { } while (0)
#define local_irq_save(f) do { (f) = 0; } while (0)
#define local_irq_restore(f) do { (void)(f); } while (0)
#define local_irq_disable() do { } while (0)
#define local_irq_enable() do { } while (0)
#define local_bh_disable() do { } while (0)
#define local_bh_enable() do { } while (0)
void cond_resched(void);
void schedule(void);
long schedule_timeout(long);
void yield(void);
#define set_current_state(s) do { } while (0)
#define __set_current_state(s) do { } while (0)

/* ---- CPUs and NUMA: one node. ---- */
#define NR_CPUS 256
#define MAX_NUMNODES 1
#define NUMA_NO_NODE (-1)
#define nr_cpu_ids kpi_nr_cpu_ids()
unsigned int kpi_nr_cpu_ids(void);
int raw_smp_processor_id(void);
#define smp_processor_id() raw_smp_processor_id()
#define get_cpu() raw_smp_processor_id()
#define put_cpu() do { } while (0)
#define num_online_cpus() kpi_nr_cpu_ids()
#define num_possible_cpus() kpi_nr_cpu_ids()
struct cpumask { DECLARE_BITMAP(bits, NR_CPUS); };
typedef struct cpumask cpumask_t;
typedef struct cpumask *cpumask_var_t;
#define cpumask_bits(m) ((m)->bits)
const struct cpumask *kpi_cpu_online_mask(void);
#define cpu_online_mask kpi_cpu_online_mask()
#define cpu_possible_mask kpi_cpu_online_mask()
#define cpu_present_mask kpi_cpu_online_mask()
#define for_each_cpu(c, m) for_each_set_bit(c, cpumask_bits(m), nr_cpu_ids)
#define for_each_online_cpu(c) for_each_cpu(c, cpu_online_mask)
#define for_each_possible_cpu(c) for_each_cpu(c, cpu_possible_mask)
static inline void cpumask_clear(struct cpumask *m) { bitmap_zero(m->bits, NR_CPUS); }
static inline void cpumask_set_cpu(unsigned int c, struct cpumask *m) { set_bit(c, m->bits); }
static inline void cpumask_clear_cpu(unsigned int c, struct cpumask *m) { clear_bit(c, m->bits); }
static inline bool cpumask_test_cpu(unsigned int c, const struct cpumask *m) { return test_bit(c, m->bits); }
static inline unsigned int cpumask_weight(const struct cpumask *m) { return bitmap_weight(m->bits, nr_cpu_ids); }
static inline unsigned int cpumask_first(const struct cpumask *m) { return find_first_bit(m->bits, nr_cpu_ids); }
static inline void cpumask_copy(struct cpumask *d, const struct cpumask *s) { bitmap_copy(d->bits, s->bits, NR_CPUS); }
static inline bool cpumask_empty(const struct cpumask *m) { return bitmap_empty(m->bits, nr_cpu_ids); }
static inline bool cpumask_subset(const struct cpumask *a, const struct cpumask *b) { return bitmap_subset(a->bits, b->bits, NR_CPUS); }
static inline bool cpumask_and(struct cpumask *d, const struct cpumask *a, const struct cpumask *b) { return bitmap_and(d->bits, a->bits, b->bits, NR_CPUS); }
bool zalloc_cpumask_var(cpumask_var_t *, gfp_t);
bool alloc_cpumask_var(cpumask_var_t *, gfp_t);
void free_cpumask_var(cpumask_var_t);
static inline const struct cpumask *cpumask_of_node(int n) { (void)n; return cpu_online_mask; }
int set_cpus_allowed_ptr(struct task_struct *, const struct cpumask *);
static inline int numa_node_id(void) { return 0; }
static inline int cpu_to_node(int c) { (void)c; return 0; }
static inline int numa_mem_id(void) { return 0; }
static inline int node_distance(int a, int b) { return a == b ? 10 : 20; }
typedef struct { DECLARE_BITMAP(bits, MAX_NUMNODES); } nodemask_t;
#define node_online(n) ((n) == 0)
#define node_possible(n) ((n) == 0)
#define node_state(n, s) ((n) == 0)
#define num_online_nodes() 1
#define num_possible_nodes() 1
#define nr_node_ids 1
#define first_node(m) 0
#define first_online_node 0
#define for_each_node(n) for ((n) = 0; (n) < 1; (n)++)
#define for_each_online_node(n) for_each_node(n)
#define for_each_node_state(n, s) for_each_node(n)
#define for_each_node_mask(n, m) for ((n) = 0; (n) < 1; (n)++) if (test_bit(n, (m).bits))
#define node_isset(n, m) test_bit(n, (m).bits)
extern nodemask_t node_possible_map;
extern nodemask_t node_online_map;
#define node_states_memory node_online_map
#define __nodes_weight(m, n) bitmap_weight((m)->bits, n)
#define nodes_weight(m) bitmap_weight((m).bits, MAX_NUMNODES)
#define nodes_empty(m) bitmap_empty((m).bits, MAX_NUMNODES)
#define node_clear(n, m) clear_bit(n, (m).bits)
#define next_node(n, m) MAX_NUMNODES
#define node_start_pfn(n) 0UL
#define node_end_pfn(n) (~0UL >> PAGE_SHIFT)
#define dev_to_node(d) 0
#define node_set(n, m) set_bit(n, (m).bits)
#define nodes_clear(m) bitmap_zero((m).bits, MAX_NUMNODES)
#define N_MEMORY 0
#define N_CPU 1
#define N_ONLINE 2
#define N_POSSIBLE 3

/* ---- Spinlocks and read-write spinlocks. ---- */
typedef struct { int kpi_lock; } spinlock_t;
typedef struct { int kpi_lock; } raw_spinlock_t;
typedef struct { int kpi_state; } rwlock_t;
#define __SPIN_LOCK_UNLOCKED(n) { 0 }
#define DEFINE_SPINLOCK(n) spinlock_t n = { 0 }
void kpi_spin_lock(int *);
void kpi_spin_unlock(int *);
int kpi_spin_trylock(int *);
static inline void spin_lock_init(spinlock_t *l) { l->kpi_lock = 0; }
static inline void spin_lock(spinlock_t *l) { kpi_spin_lock(&l->kpi_lock); }
static inline void spin_unlock(spinlock_t *l) { kpi_spin_unlock(&l->kpi_lock); }
static inline int spin_trylock(spinlock_t *l) { return kpi_spin_trylock(&l->kpi_lock); }
#define spin_lock_irqsave(l, f) do { (f) = 0; spin_lock(l); } while (0)
#define spin_unlock_irqrestore(l, f) do { (void)(f); spin_unlock(l); } while (0)
#define spin_lock_irq(l) spin_lock(l)
#define spin_unlock_irq(l) spin_unlock(l)
#define spin_lock_bh(l) spin_lock(l)
#define spin_unlock_bh(l) spin_unlock(l)
static inline bool spin_is_locked(spinlock_t *l) { return __atomic_load_n(&l->kpi_lock, __ATOMIC_RELAXED) != 0; }
#define assert_spin_locked(l) do { } while (0)
#define lockdep_assert_held(l) do { } while (0)
#define raw_spin_lock_init(l) ((l)->kpi_lock = 0)
#define raw_spin_lock(l) kpi_spin_lock(&(l)->kpi_lock)
#define raw_spin_unlock(l) kpi_spin_unlock(&(l)->kpi_lock)
#define raw_spin_lock_irqsave(l, f) do { (f) = 0; raw_spin_lock(l); } while (0)
#define raw_spin_unlock_irqrestore(l, f) do { (void)(f); raw_spin_unlock(l); } while (0)
void kpi_rwlock_read(int *);
void kpi_rwlock_read_unlock(int *);
void kpi_rwlock_write(int *);
void kpi_rwlock_write_unlock(int *);
#define rwlock_init(l) ((l)->kpi_state = 0)
#define read_lock(l) kpi_rwlock_read(&(l)->kpi_state)
#define read_unlock(l) kpi_rwlock_read_unlock(&(l)->kpi_state)
#define write_lock(l) kpi_rwlock_write(&(l)->kpi_state)
#define write_unlock(l) kpi_rwlock_write_unlock(&(l)->kpi_state)
#define read_lock_irqsave(l, f) do { (f) = 0; read_lock(l); } while (0)
#define read_unlock_irqrestore(l, f) do { (void)(f); read_unlock(l); } while (0)
#define write_lock_irqsave(l, f) do { (f) = 0; write_lock(l); } while (0)
#define write_unlock_irqrestore(l, f) do { (void)(f); write_unlock(l); } while (0)

/* ---- Sleeping locks. ---- */
struct mutex { int kpi_state; void *kpi_owner; };
#define __MUTEX_INITIALIZER(n) { 0, NULL }
#define DEFINE_MUTEX(n) struct mutex n = __MUTEX_INITIALIZER(n)
static inline void mutex_init(struct mutex *m) { m->kpi_state = 0; m->kpi_owner = NULL; }
void mutex_lock(struct mutex *);
int mutex_lock_interruptible(struct mutex *);
#define mutex_lock_killable mutex_lock_interruptible
#define mutex_lock_nested(m, s) mutex_lock(m)
int mutex_trylock(struct mutex *);
void mutex_unlock(struct mutex *);
static inline bool mutex_is_locked(struct mutex *m) { return __atomic_load_n(&m->kpi_state, __ATOMIC_RELAXED) != 0; }
static inline void mutex_destroy(struct mutex *m) { (void)m; }

/* A reader-writer semaphore. kpi_count: >0 readers, -1 a writer. */
struct rw_semaphore { int kpi_count; int kpi_seq; int kpi_writers_waiting; };
#define __RWSEM_INITIALIZER(n) { 0, 0, 0 }
#define DECLARE_RWSEM(n) struct rw_semaphore n = __RWSEM_INITIALIZER(n)
static inline void init_rwsem(struct rw_semaphore *s) { s->kpi_count = 0; s->kpi_seq = 0; s->kpi_writers_waiting = 0; }
void down_read(struct rw_semaphore *);
int down_read_trylock(struct rw_semaphore *);
void up_read(struct rw_semaphore *);
void down_write(struct rw_semaphore *);
int down_write_trylock(struct rw_semaphore *);
void up_write(struct rw_semaphore *);
void downgrade_write(struct rw_semaphore *);
#define down_read_nested(s, c) down_read(s)
#define down_write_nested(s, c) down_write(s)
static inline int down_read_killable(struct rw_semaphore *s) { down_read(s); return 0; }
static inline int down_write_killable(struct rw_semaphore *s) { down_write(s); return 0; }
static inline int rwsem_is_locked(struct rw_semaphore *s) { return __atomic_load_n(&s->kpi_count, __ATOMIC_RELAXED) != 0; }
#define lockdep_assert_held_write(s) do { } while (0)
#define lockdep_assert_held_read(s) do { } while (0)

struct semaphore { int kpi_count; };
#define __SEMAPHORE_INITIALIZER(n, c) { (c) }
#define DEFINE_SEMAPHORE(n, c) struct semaphore n = __SEMAPHORE_INITIALIZER(n, c)
static inline void sema_init(struct semaphore *s, int v) { s->kpi_count = v; }
void down(struct semaphore *);
int down_interruptible(struct semaphore *);
int down_trylock(struct semaphore *);
void up(struct semaphore *);

/* ---- Wait queues: a sequence counter waiters sleep on. ---- */
struct wait_queue_entry;
typedef int (*wait_queue_func_t)(struct wait_queue_entry *, unsigned, int, void *);
struct wait_queue_entry { unsigned int flags; void *private; wait_queue_func_t func; struct list_head entry; };
typedef struct wait_queue_entry wait_queue_entry_t;
typedef struct wait_queue_entry wait_queue_t;
struct wait_queue_head { int kpi_seq; spinlock_t lock; struct list_head head; };
typedef struct wait_queue_head wait_queue_head_t;
#define __WAIT_QUEUE_HEAD_INITIALIZER(n) { 0, { 0 }, LIST_HEAD_INIT(n.head) }
#define DECLARE_WAIT_QUEUE_HEAD(n) wait_queue_head_t n = __WAIT_QUEUE_HEAD_INITIALIZER(n)
static inline void init_waitqueue_head(wait_queue_head_t *q) { q->kpi_seq = 0; spin_lock_init(&q->lock); INIT_LIST_HEAD(&q->head); }
void kpi_wake_all(int *seq);
/* Waits until *seq differs from seen, or timeout_ns passes (<0: forever).
 * Returns false on timeout. */
bool kpi_wait_seq(int *seq, int seen, s64 timeout_ns);
static inline void wake_up(wait_queue_head_t *q) { kpi_wake_all(&q->kpi_seq); }
#define wake_up_all wake_up
#define wake_up_interruptible wake_up
#define wake_up_interruptible_all wake_up
#define wake_up_locked wake_up
static inline bool waitqueue_active(wait_queue_head_t *q) { (void)q; return true; }
#define wait_event(q, cond)                                                          \
    do {                                                                             \
        for (;;) {                                                                   \
            int _seen = __atomic_load_n(&(q).kpi_seq, __ATOMIC_ACQUIRE);              \
            if (cond) break;                                                         \
            kpi_wait_seq(&(q).kpi_seq, _seen, -1);                                    \
        }                                                                            \
    } while (0)
#define wait_event_interruptible(q, cond) ({ wait_event(q, cond); 0; })
#define wait_event_killable(q, cond) ({ wait_event(q, cond); 0; })
#define wait_event_interruptible_exclusive(q, cond) wait_event_interruptible(q, cond)
#define wait_event_timeout(q, cond, timeout)                                         \
    ({                                                                               \
        long _left = (timeout);                                                      \
        u64 _end = kpi_jiffies() + _left;                                            \
        for (;;) {                                                                   \
            int _seen = __atomic_load_n(&(q).kpi_seq, __ATOMIC_ACQUIRE);              \
            if (cond) { _left = max_t(long, 1, (long)(_end - kpi_jiffies())); break; } \
            if ((s64)(_end - kpi_jiffies()) <= 0) { _left = 0; break; }               \
            kpi_wait_seq(&(q).kpi_seq, _seen, (s64)(_end - kpi_jiffies()) * (1000000000 / HZ)); \
        }                                                                            \
        _left;                                                                       \
    })
#define wait_event_interruptible_timeout wait_event_timeout

struct completion { unsigned int done; wait_queue_head_t wait; };
#define COMPLETION_INITIALIZER(n) { 0, __WAIT_QUEUE_HEAD_INITIALIZER((n).wait) }
#define DECLARE_COMPLETION(n) struct completion n = COMPLETION_INITIALIZER(n)
#define DECLARE_COMPLETION_ONSTACK(n) DECLARE_COMPLETION(n)
static inline void init_completion(struct completion *c) { c->done = 0; init_waitqueue_head(&c->wait); }
static inline void reinit_completion(struct completion *c) { c->done = 0; }
static inline void complete(struct completion *c) { __atomic_add_fetch(&c->done, 1, __ATOMIC_SEQ_CST); wake_up(&c->wait); }
static inline void complete_all(struct completion *c) { __atomic_store_n(&c->done, UINT_MAX / 2, __ATOMIC_SEQ_CST); wake_up(&c->wait); }
static inline bool kpi_completion_take(struct completion *c)
{
    unsigned int d = __atomic_load_n(&c->done, __ATOMIC_ACQUIRE);
    while (d) {
        if (__atomic_compare_exchange_n(&c->done, &d, d - 1, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST))
            return true;
    }
    return false;
}
#define wait_for_completion(c) wait_event((c)->wait, kpi_completion_take(c))
#define wait_for_completion_interruptible(c) ({ wait_for_completion(c); 0; })
#define wait_for_completion_timeout(c, t) wait_event_timeout((c)->wait, kpi_completion_take(c), t)
static inline bool completion_done(struct completion *c) { return __atomic_load_n(&c->done, __ATOMIC_ACQUIRE) != 0; }
static inline bool try_wait_for_completion(struct completion *c) { return kpi_completion_take(c); }

/* Bit waits: one global sequence counter. */
extern int kpi_bit_wait_seq;
#define wait_on_bit_lock(word, bit, mode)                                             \
    ({                                                                               \
        for (;;) {                                                                   \
            int _seen = __atomic_load_n(&kpi_bit_wait_seq, __ATOMIC_ACQUIRE);         \
            if (!test_and_set_bit(bit, word)) break;                                 \
            kpi_wait_seq(&kpi_bit_wait_seq, _seen, -1);                               \
        }                                                                            \
        0;                                                                           \
    })
#define wait_on_bit(word, bit, mode)                                                  \
    ({                                                                               \
        for (;;) {                                                                   \
            int _seen = __atomic_load_n(&kpi_bit_wait_seq, __ATOMIC_ACQUIRE);         \
            if (!test_bit(bit, word)) break;                                         \
            kpi_wait_seq(&kpi_bit_wait_seq, _seen, -1);                               \
        }                                                                            \
        0;                                                                           \
    })
static inline void wake_up_bit(void *word, int bit) { (void)word; (void)bit; kpi_wake_all(&kpi_bit_wait_seq); }

/* ---- RCU: readers are lockless only in name; synchronize waits for none. ---- */
#define rcu_read_lock() barrier()
#define rcu_read_unlock() barrier()
#define rcu_dereference(p) READ_ONCE(p)
#define rcu_dereference_protected(p, c) (p)
#define rcu_assign_pointer(p, v) smp_store_release(&(p), (v))
#define RCU_INIT_POINTER(p, v) ((p) = (v))
void synchronize_rcu(void);

/* ---- Time. ---- */
#define HZ 250
#define NSEC_PER_USEC 1000L
#define NSEC_PER_MSEC 1000000L
#define NSEC_PER_SEC 1000000000L
#define USEC_PER_MSEC 1000L
#define USEC_PER_SEC 1000000L
#define MSEC_PER_SEC 1000L
#define MAX_JIFFY_OFFSET ((LONG_MAX >> 1) - 1)
#define MAX_SCHEDULE_TIMEOUT LONG_MAX
struct timespec64 { time64_t tv_sec; long tv_nsec; };
struct timeval { long tv_sec; long tv_usec; };
u64 kpi_monotonic_ns(void);
u64 kpi_realtime_ns(void);
static inline u64 kpi_jiffies(void) { return kpi_monotonic_ns() / (NSEC_PER_SEC / HZ); }
/* A variable, because NVIDIA's code names locals "jiffies" too; kpi's
 * ticker thread advances it (kpi/time.c). */
extern volatile unsigned long jiffies;
#define jiffies_64 kpi_jiffies()
static inline u64 get_jiffies_64(void) { return kpi_jiffies(); }
static inline unsigned long msecs_to_jiffies(unsigned int m) { return DIV_ROUND_UP(m, 1000 / HZ); }
static inline unsigned long usecs_to_jiffies(unsigned int u) { return DIV_ROUND_UP(u, 1000000 / HZ); }
static inline unsigned long nsecs_to_jiffies(u64 n) { return n / (NSEC_PER_SEC / HZ); }
static inline unsigned int jiffies_to_msecs(unsigned long j) { return j * (1000 / HZ); }
static inline unsigned int jiffies_to_usecs(unsigned long j) { return j * (1000000 / HZ); }
#define time_after(a, b) ((long)((b) - (a)) < 0)
#define time_before(a, b) time_after(b, a)
#define time_after_eq(a, b) ((long)((a) - (b)) >= 0)
#define time_before_eq(a, b) time_after_eq(b, a)
static inline s64 timespec64_to_ns(const struct timespec64 *t) { return (s64)t->tv_sec * NSEC_PER_SEC + t->tv_nsec; }
static inline struct timespec64 ns_to_timespec64(s64 n) { struct timespec64 t = { n / NSEC_PER_SEC, n % NSEC_PER_SEC }; return t; }
static inline struct timespec64 timespec64_add(struct timespec64 a, struct timespec64 b) { return ns_to_timespec64(timespec64_to_ns(&a) + timespec64_to_ns(&b)); }
static inline struct timespec64 timespec64_sub(struct timespec64 a, struct timespec64 b) { return ns_to_timespec64(timespec64_to_ns(&a) - timespec64_to_ns(&b)); }
static inline void ktime_get_raw_ts64(struct timespec64 *t) { *t = ns_to_timespec64(kpi_monotonic_ns()); }
static inline void ktime_get_ts64(struct timespec64 *t) { *t = ns_to_timespec64(kpi_monotonic_ns()); }
static inline void ktime_get_real_ts64(struct timespec64 *t) { *t = ns_to_timespec64(kpi_realtime_ns()); }
static inline ktime_t ktime_get(void) { return kpi_monotonic_ns(); }
static inline ktime_t ktime_get_raw(void) { return kpi_monotonic_ns(); }
static inline u64 ktime_get_ns(void) { return kpi_monotonic_ns(); }
static inline u64 ktime_get_raw_ns(void) { return kpi_monotonic_ns(); }
static inline u64 ktime_get_real_ns(void) { return kpi_realtime_ns(); }
static inline s64 ktime_to_ns(ktime_t k) { return k; }
static inline s64 ktime_to_us(ktime_t k) { return k / 1000; }
static inline ktime_t ns_to_ktime(u64 n) { return n; }
#define ktime_sub(a, b) ((a) - (b))
#define ktime_add_ns(a, n) ((a) + (n))
static inline time64_t ktime_get_real_seconds(void) { return kpi_realtime_ns() / NSEC_PER_SEC; }
void kpi_sleep_ns(u64);
static inline void udelay(unsigned long us) { kpi_sleep_ns(us * 1000); }
static inline void ndelay(unsigned long ns) { kpi_sleep_ns(ns); }
static inline void mdelay(unsigned long ms) { kpi_sleep_ns(ms * NSEC_PER_MSEC); }
static inline void msleep(unsigned int ms) { kpi_sleep_ns((u64)ms * NSEC_PER_MSEC); }
static inline unsigned long msleep_interruptible(unsigned int ms) { msleep(ms); return 0; }
static inline void usleep_range(unsigned long lo, unsigned long hi) { (void)hi; kpi_sleep_ns(lo * 1000); }
static inline void fsleep(unsigned long us) { kpi_sleep_ns(us * 1000); }

/* ---- Timers: run on the kpi timer thread. ---- */
struct timer_list {
    struct list_head kpi_entry;
    unsigned long expires;
    void (*function)(struct timer_list *);
    u32 flags;
    int kpi_pending;
};
void timer_setup(struct timer_list *, void (*)(struct timer_list *), unsigned int);
int mod_timer(struct timer_list *, unsigned long expires);
void add_timer(struct timer_list *);
int del_timer(struct timer_list *);
int del_timer_sync(struct timer_list *);
#define timer_delete del_timer
#define timer_delete_sync del_timer_sync
static inline int timer_pending(const struct timer_list *t) { return __atomic_load_n(&t->kpi_pending, __ATOMIC_ACQUIRE); }
#define from_timer(var, t, field) container_of(t, __typeof__(*var), field)
#define timer_container_of from_timer

/* ---- Kernel threads. ---- */
struct task_struct *kthread_create_on_node(int (*fn)(void *), void *data, int node, const char *fmt, ...) __printf(4, 5);
#define kthread_create(fn, data, fmt, ...) kthread_create_on_node(fn, data, NUMA_NO_NODE, fmt, ##__VA_ARGS__)
#define kthread_run(fn, data, fmt, ...)                                                       \
    ({ struct task_struct *_k = kthread_create(fn, data, fmt, ##__VA_ARGS__);                \
       if (!IS_ERR(_k)) wake_up_process(_k); _k; })
int wake_up_process(struct task_struct *);
int kthread_stop(struct task_struct *);
bool kthread_should_stop(void);
void kthread_bind(struct task_struct *, unsigned int cpu);

/* ---- Work queues: one pool of kpi worker threads. ---- */
struct work_struct;
typedef void (*work_func_t)(struct work_struct *);
struct work_struct { struct list_head entry; work_func_t func; int kpi_state; };
struct delayed_work { struct work_struct work; struct timer_list timer; struct workqueue_struct *wq; };
struct workqueue_struct;
#define INIT_WORK(w, f) do { INIT_LIST_HEAD(&(w)->entry); (w)->func = (f); (w)->kpi_state = 0; } while (0)
#define INIT_DELAYED_WORK(d, f) kpi_init_delayed_work(d, f)
void kpi_init_delayed_work(struct delayed_work *, work_func_t);
#define DECLARE_WORK(n, f) struct work_struct n = { LIST_HEAD_INIT(n.entry), (f), 0 }
static inline struct delayed_work *to_delayed_work(struct work_struct *w) { return container_of(w, struct delayed_work, work); }
extern struct workqueue_struct *system_wq;
#define system_unbound_wq system_wq
#define system_long_wq system_wq
bool queue_work(struct workqueue_struct *, struct work_struct *);
bool queue_delayed_work(struct workqueue_struct *, struct delayed_work *, unsigned long);
bool mod_delayed_work(struct workqueue_struct *, struct delayed_work *, unsigned long);
static inline bool schedule_work(struct work_struct *w) { return queue_work(system_wq, w); }
static inline bool schedule_delayed_work(struct delayed_work *d, unsigned long t) { return queue_delayed_work(system_wq, d, t); }
bool cancel_work_sync(struct work_struct *);
bool cancel_delayed_work(struct delayed_work *);
bool cancel_delayed_work_sync(struct delayed_work *);
bool flush_work(struct work_struct *);
void flush_workqueue(struct workqueue_struct *);
void flush_scheduled_work(void);
struct workqueue_struct *alloc_workqueue(const char *fmt, unsigned int flags, int max, ...);
#define create_singlethread_workqueue(n) alloc_workqueue("%s", 0, 1, n)
#define alloc_ordered_workqueue(fmt, f, ...) alloc_workqueue(fmt, f, 1, ##__VA_ARGS__)
void destroy_workqueue(struct workqueue_struct *);
#define WQ_UNBOUND 0x2
#define WQ_MEM_RECLAIM 0x8
#define WQ_HIGHPRI 0x10

/* ---- Per-CPU variables: plain variables. ---- */
/* Dynamic per-CPU data is an array of NR_CPUS; the "current CPU" is the
 * one the calling thread last ran on, so callers that need exclusion
 * still take their own locks, as on Linux with preemption on. */
#define get_cpu_var(v) ((v)[raw_smp_processor_id()])
#define put_cpu_var(v) do { } while (0)
#define DEFINE_PER_CPU(t, n) t n[NR_CPUS]
#define DECLARE_PER_CPU(t, n) extern t n[NR_CPUS]
#define per_cpu(v, c) ((v)[c])
#define per_cpu_ptr(p, c) ((p) + (c))
#define this_cpu_ptr(p) ((p) + raw_smp_processor_id())
#define alloc_percpu(t) ((t *)kcalloc(NR_CPUS, sizeof(t), GFP_KERNEL))
#define free_percpu(p) kfree(p)

#endif

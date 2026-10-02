/*
 * uvm-kpi: locks, waiting, the current task, threads, work queues, timers
 * and time, over futexes and the C library's threads.
 *
 * SPDX-License-Identifier: MIT
 */
#include "libc.h"

/* ---- Futexes. ---- */
static void futex_wait(int *addr, int val, s64 timeout_ns)
{
    struct timespec64 ts, *tp = NULL;

    if (timeout_ns >= 0) {
        ts = ns_to_timespec64(timeout_ns);
        tp = &ts;
    }
    syscall(KPI_SYS_futex, addr, KPI_FUTEX_WAIT_PRIVATE, val, tp, NULL, 0);
}

static void futex_wake(int *addr, int n)
{
    syscall(KPI_SYS_futex, addr, KPI_FUTEX_WAKE_PRIVATE, n, NULL, NULL, 0);
}

/* ---- A lock word: 0 free, 1 held, 2 held with waiters. ---- */
static bool lock_try(int *l)
{
    int expected = 0;
    return __atomic_compare_exchange_n(l, &expected, 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED);
}

static void lock_slow(int *l, int spins)
{
    int i;

    for (i = 0; i < spins; i++) {
        if (lock_try(l))
            return;
        cpu_relax();
    }
    while (__atomic_exchange_n(l, 2, __ATOMIC_ACQUIRE) != 0)
        futex_wait(l, 2, -1);
}

static void lock_release(int *l)
{
    if (__atomic_exchange_n(l, 0, __ATOMIC_RELEASE) == 2)
        futex_wake(l, 1);
}

void kpi_spin_lock(int *l)
{
    if (!lock_try(l))
        lock_slow(l, 1000);
}

void kpi_spin_unlock(int *l)
{
    lock_release(l);
}

int kpi_spin_trylock(int *l)
{
    return lock_try(l);
}

void mutex_lock(struct mutex *m)
{
    if (!lock_try(&m->kpi_state))
        lock_slow(&m->kpi_state, 100);
    m->kpi_owner = kpi_current();
}

int mutex_lock_interruptible(struct mutex *m)
{
    mutex_lock(m);
    return 0;
}

int mutex_trylock(struct mutex *m)
{
    if (!lock_try(&m->kpi_state))
        return 0;
    m->kpi_owner = kpi_current();
    return 1;
}

void mutex_unlock(struct mutex *m)
{
    m->kpi_owner = NULL;
    lock_release(&m->kpi_state);
}

/* ---- Reader-writer semaphores. kpi_count > 0: readers; -1: a writer.
 *      Waiters sleep on kpi_seq. A waiting writer holds back new readers,
 *      as Linux's does, so writers are not starved. ---- */
static void rwsem_wake(struct rw_semaphore *s)
{
    __atomic_add_fetch(&s->kpi_seq, 1, __ATOMIC_RELEASE);
    futex_wake(&s->kpi_seq, INT_MAX);
}

int down_read_trylock(struct rw_semaphore *s)
{
    int c = __atomic_load_n(&s->kpi_count, __ATOMIC_RELAXED);

    while (c >= 0 && __atomic_load_n(&s->kpi_writers_waiting, __ATOMIC_RELAXED) == 0) {
        if (__atomic_compare_exchange_n(&s->kpi_count, &c, c + 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED))
            return 1;
    }
    return 0;
}

void down_read(struct rw_semaphore *s)
{
    for (;;) {
        int seq = __atomic_load_n(&s->kpi_seq, __ATOMIC_ACQUIRE);
        if (down_read_trylock(s))
            return;
        futex_wait(&s->kpi_seq, seq, -1);
    }
}

void up_read(struct rw_semaphore *s)
{
    if (__atomic_sub_fetch(&s->kpi_count, 1, __ATOMIC_RELEASE) == 0)
        rwsem_wake(s);
}

int down_write_trylock(struct rw_semaphore *s)
{
    int expected = 0;
    return __atomic_compare_exchange_n(&s->kpi_count, &expected, -1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED);
}

void down_write(struct rw_semaphore *s)
{
    if (down_write_trylock(s))
        return;
    __atomic_add_fetch(&s->kpi_writers_waiting, 1, __ATOMIC_RELAXED);
    for (;;) {
        int seq = __atomic_load_n(&s->kpi_seq, __ATOMIC_ACQUIRE);
        if (down_write_trylock(s))
            break;
        futex_wait(&s->kpi_seq, seq, -1);
    }
    __atomic_sub_fetch(&s->kpi_writers_waiting, 1, __ATOMIC_RELAXED);
}

void up_write(struct rw_semaphore *s)
{
    __atomic_store_n(&s->kpi_count, 0, __ATOMIC_RELEASE);
    rwsem_wake(s);
}

void downgrade_write(struct rw_semaphore *s)
{
    __atomic_store_n(&s->kpi_count, 1, __ATOMIC_RELEASE);
    rwsem_wake(s);
}

/* ---- Read-write spinlocks: the same scheme on one word. ---- */
void kpi_rwlock_read(int *w)
{
    for (;;) {
        int c = __atomic_load_n(w, __ATOMIC_RELAXED);
        if (c >= 0 && __atomic_compare_exchange_n(w, &c, c + 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED))
            return;
        sched_yield();
    }
}

void kpi_rwlock_read_unlock(int *w)
{
    __atomic_sub_fetch(w, 1, __ATOMIC_RELEASE);
}

void kpi_rwlock_write(int *w)
{
    for (;;) {
        int expected = 0;
        if (__atomic_compare_exchange_n(w, &expected, -1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED))
            return;
        sched_yield();
    }
}

void kpi_rwlock_write_unlock(int *w)
{
    __atomic_store_n(w, 0, __ATOMIC_RELEASE);
}

/* ---- Counting semaphores. ---- */
int down_trylock(struct semaphore *s)
{
    int c = __atomic_load_n(&s->kpi_count, __ATOMIC_RELAXED);

    while (c > 0) {
        if (__atomic_compare_exchange_n(&s->kpi_count, &c, c - 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED))
            return 0;
    }
    return 1;
}

void down(struct semaphore *s)
{
    for (;;) {
        int c = __atomic_load_n(&s->kpi_count, __ATOMIC_RELAXED);
        if (c > 0) {
            if (__atomic_compare_exchange_n(&s->kpi_count, &c, c - 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED))
                return;
            continue;
        }
        futex_wait(&s->kpi_count, c, -1);
    }
}

int down_interruptible(struct semaphore *s)
{
    down(s);
    return 0;
}

void up(struct semaphore *s)
{
    __atomic_add_fetch(&s->kpi_count, 1, __ATOMIC_RELEASE);
    futex_wake(&s->kpi_count, 1);
}

/* ---- Wait queues and bit waits. ---- */
int kpi_bit_wait_seq;

void kpi_wake_all(int *seq)
{
    __atomic_add_fetch(seq, 1, __ATOMIC_RELEASE);
    futex_wake(seq, INT_MAX);
}

bool kpi_wait_seq(int *seq, int seen, s64 timeout_ns)
{
    if (__atomic_load_n(seq, __ATOMIC_ACQUIRE) != seen)
        return true;
    if (timeout_ns == 0)
        return false;
    futex_wait(seq, seen, timeout_ns);
    return true;
}

void synchronize_rcu(void)
{
    smp_mb();
}

/* ---- Time. ---- */
volatile unsigned long jiffies;

u64 kpi_monotonic_ns(void)
{
    struct timespec64 t;
    clock_gettime(KPI_CLOCK_MONOTONIC, &t);
    return timespec64_to_ns(&t);
}

u64 kpi_realtime_ns(void)
{
    struct timespec64 t;
    clock_gettime(KPI_CLOCK_REALTIME, &t);
    return timespec64_to_ns(&t);
}

void kpi_sleep_ns(u64 ns)
{
    struct timespec64 t = ns_to_timespec64(ns);
    clock_nanosleep(KPI_CLOCK_MONOTONIC, 0, &t, NULL);
}

void cond_resched(void)
{
    sched_yield();
}

void schedule(void)
{
    sched_yield();
}

void yield(void)
{
    sched_yield();
}

long schedule_timeout(long timeout)
{
    kpi_sleep_ns((u64)timeout * (NSEC_PER_SEC / HZ));
    return 0;
}

/* ---- The current task: one per host thread, made on first use.
 *      Threads that are not kthreads stand for a client in a request, and
 *      see that client's mm; in the self-test build the client is this
 *      process, one mm for all its threads. In nvrm the request bridge
 *      sets current->mm to the requesting client's for the request. ---- */
static __thread struct task_struct *this_task;
static struct mm_struct process_mm = { .mm_users = { 1 }, .mm_count = { 1 }, .task_size = 1UL << 47 };
static int next_cpu;

struct task_struct *kpi_current(void)
{
    if (!this_task) {
        struct task_struct *t = calloc(1, sizeof(*t));
        if (!t)
            abort();
        t->pid = (pid_t)syscall(KPI_SYS_gettid);
        t->tgid = getpid();
        t->group_leader = t;
        t->mm = &process_mm;
        memcpy(t->comm, "nvrm", 5);
        this_task = t;
    }
    return this_task;
}

unsigned int kpi_nr_cpu_ids(void)
{
    static unsigned int n;
    if (!n) {
        int c = get_nprocs();
        n = c < 1 ? 1 : (c > NR_CPUS ? NR_CPUS : (unsigned int)c);
    }
    return n;
}

int raw_smp_processor_id(void)
{
    int c = sched_getcpu();
    if (c < 0 || (unsigned int)c >= kpi_nr_cpu_ids())
        c = __atomic_fetch_add(&next_cpu, 1, __ATOMIC_RELAXED) % (int)kpi_nr_cpu_ids();
    return c;
}

const struct cpumask *kpi_cpu_online_mask(void)
{
    static struct cpumask mask;
    static int done;
    if (!__atomic_load_n(&done, __ATOMIC_ACQUIRE)) {
        unsigned int i;
        for (i = 0; i < kpi_nr_cpu_ids(); i++)
            set_bit(i, mask.bits);
        __atomic_store_n(&done, 1, __ATOMIC_RELEASE);
    }
    return &mask;
}

nodemask_t node_possible_map = { { 1 } };
nodemask_t node_online_map = { { 1 } };

bool zalloc_cpumask_var(cpumask_var_t *m, gfp_t f)
{
    *m = kzalloc(sizeof(struct cpumask), f);
    return *m != NULL;
}

bool alloc_cpumask_var(cpumask_var_t *m, gfp_t f)
{
    return zalloc_cpumask_var(m, f);
}

void free_cpumask_var(cpumask_var_t m)
{
    kfree(m);
}

int set_cpus_allowed_ptr(struct task_struct *t, const struct cpumask *m)
{
    (void)t;
    (void)m;
    return 0;
}

/* ---- Kernel threads. ---- */
struct kthread {
    int (*fn)(void *);
    void *data;
    int started;     /* futex: set by wake_up_process */
    int should_stop;
    int done;        /* futex */
    int result;
    kpi_pthread_t thread;
    struct task_struct *task;
};

static void *kthread_main(void *arg)
{
    struct kthread *k = arg;

    this_task = k->task;
    this_task->pid = (pid_t)syscall(KPI_SYS_gettid);
    while (!__atomic_load_n(&k->started, __ATOMIC_ACQUIRE))
        futex_wait(&k->started, 0, -1);
    k->result = k->fn(k->data);
    __atomic_store_n(&k->done, 1, __ATOMIC_RELEASE);
    futex_wake(&k->done, INT_MAX);
    return NULL;
}

struct task_struct *kthread_create_on_node(int (*fn)(void *), void *data, int node, const char *fmt, ...)
{
    struct kthread *k = calloc(1, sizeof(*k));
    struct task_struct *t = calloc(1, sizeof(*t));
    va_list ap;

    (void)node;
    if (!k || !t) {
        free(k);
        free(t);
        return ERR_PTR(-ENOMEM);
    }
    k->fn = fn;
    k->data = data;
    k->task = t;
    t->kpi_kthread = k;
    t->tgid = getpid();
    t->flags = PF_KTHREAD;
    t->group_leader = t;
    va_start(ap, fmt);
    vsnprintf(t->comm, sizeof(t->comm), fmt, ap);
    va_end(ap);
    if (pthread_create(&k->thread, NULL, kthread_main, k) != 0) {
        free(k);
        free(t);
        return ERR_PTR(-EAGAIN);
    }
    return t;
}

int wake_up_process(struct task_struct *t)
{
    struct kthread *k = t->kpi_kthread;
    if (!k)
        return 0;
    __atomic_store_n(&k->started, 1, __ATOMIC_RELEASE);
    futex_wake(&k->started, INT_MAX);
    return 1;
}

bool kthread_should_stop(void)
{
    struct kthread *k = kpi_current()->kpi_kthread;
    return k && __atomic_load_n(&k->should_stop, __ATOMIC_ACQUIRE);
}

int kthread_stop(struct task_struct *t)
{
    struct kthread *k = t->kpi_kthread;
    int r;

    __atomic_store_n(&k->should_stop, 1, __ATOMIC_RELEASE);
    wake_up_process(t);
    while (!__atomic_load_n(&k->done, __ATOMIC_ACQUIRE))
        futex_wait(&k->done, 0, -1);
    pthread_join(k->thread, NULL);
    r = k->result;
    free(k);
    free(t);
    return r;
}

void kthread_bind(struct task_struct *t, unsigned int cpu)
{
    (void)t;
    (void)cpu;
}

/* ---- Timers and work: one service thread runs both, in deadline order
 *      for timers and FIFO order for work. Work items may block, so a
 *      second thread is not needed for correctness of UVM's own users
 *      (they flush or cancel, never wait on each other). ---- */
struct workqueue_struct { int unused; };
static struct workqueue_struct the_wq;
struct workqueue_struct *system_wq = &the_wq;

static int svc_lock;
static int svc_seq;
static int svc_started;
static LIST_HEAD(svc_timers);
static LIST_HEAD(svc_work);
static struct work_struct *svc_running;
static int svc_done_seq;

#define WORK_IDLE 0
#define WORK_QUEUED 1

static void *svc_main(void *arg)
{
    (void)arg;
    for (;;) {
        int seq = __atomic_load_n(&svc_seq, __ATOMIC_ACQUIRE);
        u64 now = kpi_jiffies();
        s64 wait_ns = -1;
        struct timer_list *t, *due = NULL;
        struct work_struct *w = NULL;

        jiffies = (unsigned long)now;
        kpi_spin_lock(&svc_lock);
        list_for_each_entry(t, &svc_timers, kpi_entry) {
            if ((long)(t->expires - now) <= 0) {
                due = t;
                break;
            }
        }
        if (due) {
            list_del_init(&due->kpi_entry);
            __atomic_store_n(&due->kpi_pending, 0, __ATOMIC_RELEASE);
        } else if (!list_empty(&svc_work)) {
            w = list_first_entry(&svc_work, struct work_struct, entry);
            list_del_init(&w->entry);
            w->kpi_state = WORK_IDLE;
            svc_running = w;
        } else {
            list_for_each_entry(t, &svc_timers, kpi_entry) {
                s64 left = (s64)(t->expires - now) * (NSEC_PER_SEC / HZ);
                if (wait_ns < 0 || left < wait_ns)
                    wait_ns = left;
            }
        }
        kpi_spin_unlock(&svc_lock);

        if (due) {
            due->function(due);
            continue;
        }
        if (w) {
            w->func(w);
            kpi_spin_lock(&svc_lock);
            svc_running = NULL;
            kpi_spin_unlock(&svc_lock);
            kpi_wake_all(&svc_done_seq);
            continue;
        }
        /* jiffies advances at least every tick while anyone may look. */
        if (wait_ns < 0 || wait_ns > NSEC_PER_SEC / HZ)
            wait_ns = NSEC_PER_SEC / HZ;
        kpi_wait_seq(&svc_seq, seq, wait_ns);
    }
    return NULL;
}

static void svc_kick(void)
{
    int expected = 0;

    if (__atomic_compare_exchange_n(&svc_started, &expected, 1, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
        kpi_pthread_t th;
        jiffies = (unsigned long)kpi_jiffies();
        if (pthread_create(&th, NULL, svc_main, NULL) != 0)
            abort();
        pthread_detach(th);
    }
    kpi_wake_all(&svc_seq);
}

void kpi_start_services(void)
{
    svc_kick();
}

void timer_setup(struct timer_list *t, void (*fn)(struct timer_list *), unsigned int flags)
{
    INIT_LIST_HEAD(&t->kpi_entry);
    t->function = fn;
    t->flags = flags;
    t->kpi_pending = 0;
}

int mod_timer(struct timer_list *t, unsigned long expires)
{
    int was;

    kpi_spin_lock(&svc_lock);
    was = !list_empty(&t->kpi_entry);
    if (was)
        list_del_init(&t->kpi_entry);
    t->expires = expires;
    list_add_tail(&t->kpi_entry, &svc_timers);
    __atomic_store_n(&t->kpi_pending, 1, __ATOMIC_RELEASE);
    kpi_spin_unlock(&svc_lock);
    svc_kick();
    return was;
}

void add_timer(struct timer_list *t)
{
    mod_timer(t, t->expires);
}

int del_timer(struct timer_list *t)
{
    int was;

    kpi_spin_lock(&svc_lock);
    was = !list_empty(&t->kpi_entry);
    if (was)
        list_del_init(&t->kpi_entry);
    __atomic_store_n(&t->kpi_pending, 0, __ATOMIC_RELEASE);
    kpi_spin_unlock(&svc_lock);
    return was;
}

int del_timer_sync(struct timer_list *t)
{
    /* The service thread runs a timer with svc_lock released; a timer
     * function that is running when this is called has already been taken
     * off the list, and the caller's own synchronisation (as on Linux)
     * keeps it from being re-armed. */
    return del_timer(t);
}

bool queue_work(struct workqueue_struct *wq, struct work_struct *w)
{
    bool queued = false;

    (void)wq;
    kpi_spin_lock(&svc_lock);
    if (w->kpi_state == WORK_IDLE) {
        w->kpi_state = WORK_QUEUED;
        list_add_tail(&w->entry, &svc_work);
        queued = true;
    }
    kpi_spin_unlock(&svc_lock);
    if (queued)
        svc_kick();
    return queued;
}

static void delayed_work_timer(struct timer_list *t)
{
    struct delayed_work *d = container_of(t, struct delayed_work, timer);
    queue_work(d->wq ? d->wq : system_wq, &d->work);
}

void kpi_init_delayed_work(struct delayed_work *d, work_func_t f)
{
    INIT_WORK(&d->work, f);
    timer_setup(&d->timer, delayed_work_timer, 0);
    d->wq = NULL;
}

bool queue_delayed_work(struct workqueue_struct *wq, struct delayed_work *d, unsigned long delay)
{
    if (timer_pending(&d->timer) || d->work.kpi_state != WORK_IDLE)
        return false;
    d->wq = wq;
    if (delay == 0)
        return queue_work(wq, &d->work);
    mod_timer(&d->timer, jiffies + delay);
    return true;
}

bool mod_delayed_work(struct workqueue_struct *wq, struct delayed_work *d, unsigned long delay)
{
    bool was = cancel_delayed_work(d);
    queue_delayed_work(wq, d, delay);
    return was;
}

static bool work_unqueue(struct work_struct *w)
{
    bool was = false;

    kpi_spin_lock(&svc_lock);
    if (w->kpi_state == WORK_QUEUED) {
        list_del_init(&w->entry);
        w->kpi_state = WORK_IDLE;
        was = true;
    }
    kpi_spin_unlock(&svc_lock);
    return was;
}

bool flush_work(struct work_struct *w)
{
    bool waited = false;

    for (;;) {
        int seq = __atomic_load_n(&svc_done_seq, __ATOMIC_ACQUIRE);
        bool busy;
        kpi_spin_lock(&svc_lock);
        busy = w->kpi_state == WORK_QUEUED || svc_running == w;
        kpi_spin_unlock(&svc_lock);
        if (!busy)
            return waited;
        waited = true;
        kpi_wait_seq(&svc_done_seq, seq, NSEC_PER_SEC / HZ);
    }
}

bool cancel_work_sync(struct work_struct *w)
{
    bool was = work_unqueue(w);
    flush_work(w);
    return was;
}

bool cancel_delayed_work(struct delayed_work *d)
{
    bool a = del_timer(&d->timer);
    bool b = work_unqueue(&d->work);
    return a || b;
}

bool cancel_delayed_work_sync(struct delayed_work *d)
{
    bool was = cancel_delayed_work(d);
    flush_work(&d->work);
    return was;
}

void flush_workqueue(struct workqueue_struct *wq)
{
    (void)wq;
    for (;;) {
        int seq = __atomic_load_n(&svc_done_seq, __ATOMIC_ACQUIRE);
        bool busy;
        kpi_spin_lock(&svc_lock);
        busy = !list_empty(&svc_work) || svc_running;
        kpi_spin_unlock(&svc_lock);
        if (!busy)
            return;
        kpi_wait_seq(&svc_done_seq, seq, NSEC_PER_SEC / HZ);
    }
}

void flush_scheduled_work(void)
{
    flush_workqueue(system_wq);
}

struct workqueue_struct *alloc_workqueue(const char *fmt, unsigned int flags, int max, ...)
{
    (void)fmt;
    (void)flags;
    (void)max;
    return system_wq;
}

void destroy_workqueue(struct workqueue_struct *wq)
{
    flush_workqueue(wq);
}

/*
 * uvm-kpi: printing, assertions, module parameters, strings, sorting,
 * bitmaps, and the red-black and radix trees.
 *
 * The trees are uvm-kpi's own: the red-black tree is the textbook
 * algorithm (CLRS ch. 13) over Linux's node layout, the "radix tree" a
 * sorted array, which is all UVM's few, small uses need.
 *
 * SPDX-License-Identifier: MIT
 */
#include "libc.h"

/* ---- Printing. ---- */
static void put_log(const char *s, size_t n)
{
    write(2, s, n);
}

int vprintk(const char *fmt, va_list ap)
{
    char buf[1024];
    int n;

    /* Drop Linux's level prefix, KERN_SOH and a level character. */
    if (fmt[0] == KERN_SOH[0] && fmt[1])
        fmt += 2;
    n = vsnprintf(buf, sizeof(buf), fmt, ap);
    if (n < 0)
        return n;
    put_log(buf, min_t(size_t, (size_t)n, sizeof(buf) - 1));
    return n;
}

int printk(const char *fmt, ...)
{
    va_list ap;
    int n;

    va_start(ap, fmt);
    n = vprintk(fmt, ap);
    va_end(ap);
    return n;
}

int scnprintf(char *buf, size_t size, const char *fmt, ...)
{
    va_list ap;
    int n;

    if (size == 0)
        return 0;
    va_start(ap, fmt);
    n = vsnprintf(buf, size, fmt, ap);
    va_end(ap);
    if (n < 0)
        return 0;
    return (size_t)n >= size ? (int)size - 1 : n;
}

void dump_stack(void)
{
    printk("uvm-kpi: (no stack trace)\n");
}

void panic(const char *fmt, ...)
{
    va_list ap;

    va_start(ap, fmt);
    vprintk(fmt, ap);
    va_end(ap);
    printk("\n");
    abort();
}

void kpi_bug(const char *file, int line)
{
    panic("uvm-kpi: BUG at %s:%d", file, line);
}

void kpi_warn(const char *file, int line)
{
    printk("uvm-kpi: WARNING at %s:%d\n", file, line);
}

int ___ratelimit(struct ratelimit_state *rs, const char *func)
{
    (void)rs;
    (void)func;
    return 1;
}

/* ---- Module parameters. The linker gathers KPI_PARAM entries. ---- */
extern const struct kpi_param __start_kpi_params[] __attribute__((weak));
extern const struct kpi_param __stop_kpi_params[] __attribute__((weak));

int kpi_param_set(const char *name, const char *value)
{
    const struct kpi_param *p;

    for (p = __start_kpi_params; p && p < __stop_kpi_params; p++) {
        if (strcmp(p->name, name) != 0)
            continue;
        switch (p->type) {
        case 'b':
            *(bool *)p->value = value[0] == '1' || value[0] == 'y' || value[0] == 'Y';
            return 0;
        case 'i':
            *(int *)p->value = (int)strtol(value, NULL, 0);
            return 0;
        case 'u':
            *(unsigned int *)p->value = (unsigned int)strtoul(value, NULL, 0);
            return 0;
        case 'l':
            *(unsigned long *)p->value = strtoul(value, NULL, 0);
            return 0;
        case 's':
            *(const char **)p->value = value;
            return 0;
        }
    }
    return -ENOENT;
}

/* ---- Strings. ---- */
size_t kpi_strlcpy(char *d, const char *s, size_t n)
{
    size_t len = strlen(s);

    if (n) {
        size_t c = len >= n ? n - 1 : len;
        memcpy(d, s, c);
        d[c] = '\0';
    }
    return len;
}

size_t kpi_strscpy(char *d, const char *s, size_t n)
{
    size_t len = strnlen(s, n);

    if (n == 0)
        return (size_t)-E2BIG;
    if (len == n) {
        memcpy(d, s, n - 1);
        d[n - 1] = '\0';
        return (size_t)-E2BIG;
    }
    memcpy(d, s, len + 1);
    return len;
}

unsigned long simple_strtoul(const char *s, char **end, unsigned int base)
{
    return strtoul(s, end, (int)base);
}

int kstrtoull(const char *s, unsigned int base, unsigned long long *r)
{
    char *end;
    *r = strtoull(s, &end, (int)base);
    if (end == s || (*end && *end != '\n'))
        return -EINVAL;
    return 0;
}

int kstrtoul(const char *s, unsigned int base, unsigned long *r)
{
    unsigned long long v;
    int e = kstrtoull(s, base, &v);
    *r = (unsigned long)v;
    return e;
}

int kstrtouint(const char *s, unsigned int base, unsigned int *r)
{
    unsigned long long v;
    int e = kstrtoull(s, base, &v);
    if (!e && v > UINT_MAX)
        return -ERANGE;
    *r = (unsigned int)v;
    return e;
}

int kstrtoint(const char *s, unsigned int base, int *r)
{
    char *end;
    long v = strtol(s, &end, (int)base);
    if (end == s || (*end && *end != '\n'))
        return -EINVAL;
    *r = (int)v;
    return 0;
}

/* ---- Sorting: an in-place heapsort, so it never allocates. ---- */
static void sort_swap(void *a, void *b, int size, void (*swap_fn)(void *, void *, int))
{
    if (swap_fn) {
        swap_fn(a, b, size);
    } else {
        char *x = a, *y = b, t;
        while (size--) {
            t = *x;
            *x++ = *y;
            *y++ = t;
        }
    }
}

static void sift_down(char *base, size_t root, size_t n, size_t size,
                      int (*cmp)(const void *, const void *), void (*swap_fn)(void *, void *, int))
{
    for (;;) {
        size_t child = 2 * root + 1;
        if (child >= n)
            return;
        if (child + 1 < n && cmp(base + child * size, base + (child + 1) * size) < 0)
            child++;
        if (cmp(base + root * size, base + child * size) >= 0)
            return;
        sort_swap(base + root * size, base + child * size, (int)size, swap_fn);
        root = child;
    }
}

void sort(void *base, size_t num, size_t size, int (*cmp)(const void *, const void *),
          void (*swap_fn)(void *, void *, int))
{
    char *b = base;
    size_t i;

    if (num < 2)
        return;
    for (i = num / 2; i-- > 0;)
        sift_down(b, i, num, size, cmp, swap_fn);
    for (i = num - 1; i > 0; i--) {
        sort_swap(b, b + i * size, (int)size, swap_fn);
        sift_down(b, 0, i, size, cmp, swap_fn);
    }
}

/* ---- Bitmaps. ---- */
static unsigned long last_word_mask(unsigned int n)
{
    return (n % BITS_PER_LONG) ? (1UL << (n % BITS_PER_LONG)) - 1 : ~0UL;
}

#define WORDS(n) BITS_TO_LONGS(n)

void bitmap_zero(unsigned long *d, unsigned int n) { memset(d, 0, WORDS(n) * sizeof(long)); }
void bitmap_fill(unsigned long *d, unsigned int n) { memset(d, 0xff, WORDS(n) * sizeof(long)); }
void bitmap_copy(unsigned long *d, const unsigned long *s, unsigned int n) { memcpy(d, s, WORDS(n) * sizeof(long)); }

bool bitmap_and(unsigned long *d, const unsigned long *a, const unsigned long *b, unsigned int n)
{
    unsigned long any = 0;
    unsigned int i;
    for (i = 0; i < WORDS(n); i++) {
        d[i] = a[i] & b[i];
        any |= (i == WORDS(n) - 1) ? d[i] & last_word_mask(n) : d[i];
    }
    return any != 0;
}

bool bitmap_andnot(unsigned long *d, const unsigned long *a, const unsigned long *b, unsigned int n)
{
    unsigned long any = 0;
    unsigned int i;
    for (i = 0; i < WORDS(n); i++) {
        d[i] = a[i] & ~b[i];
        any |= (i == WORDS(n) - 1) ? d[i] & last_word_mask(n) : d[i];
    }
    return any != 0;
}

void bitmap_or(unsigned long *d, const unsigned long *a, const unsigned long *b, unsigned int n)
{
    unsigned int i;
    for (i = 0; i < WORDS(n); i++)
        d[i] = a[i] | b[i];
}

void bitmap_xor(unsigned long *d, const unsigned long *a, const unsigned long *b, unsigned int n)
{
    unsigned int i;
    for (i = 0; i < WORDS(n); i++)
        d[i] = a[i] ^ b[i];
}

void bitmap_complement(unsigned long *d, const unsigned long *s, unsigned int n)
{
    unsigned int i;
    for (i = 0; i < WORDS(n); i++)
        d[i] = ~s[i];
}

bool bitmap_equal(const unsigned long *a, const unsigned long *b, unsigned int n)
{
    unsigned int i;
    for (i = 0; i < WORDS(n); i++) {
        unsigned long m = (i == WORDS(n) - 1) ? last_word_mask(n) : ~0UL;
        if ((a[i] ^ b[i]) & m)
            return false;
    }
    return true;
}

bool bitmap_intersects(const unsigned long *a, const unsigned long *b, unsigned int n)
{
    unsigned int i;
    for (i = 0; i < WORDS(n); i++) {
        unsigned long m = (i == WORDS(n) - 1) ? last_word_mask(n) : ~0UL;
        if (a[i] & b[i] & m)
            return true;
    }
    return false;
}

bool bitmap_subset(const unsigned long *a, const unsigned long *b, unsigned int n)
{
    unsigned int i;
    for (i = 0; i < WORDS(n); i++) {
        unsigned long m = (i == WORDS(n) - 1) ? last_word_mask(n) : ~0UL;
        if (a[i] & ~b[i] & m)
            return false;
    }
    return true;
}

bool bitmap_empty(const unsigned long *a, unsigned int n)
{
    return find_first_bit(a, n) >= n;
}

bool bitmap_full(const unsigned long *a, unsigned int n)
{
    return find_first_zero_bit(a, n) >= n;
}

unsigned int bitmap_weight(const unsigned long *a, unsigned int n)
{
    unsigned int i, w = 0;
    for (i = 0; i < WORDS(n); i++) {
        unsigned long m = (i == WORDS(n) - 1) ? last_word_mask(n) : ~0UL;
        w += (unsigned int)__builtin_popcountl(a[i] & m);
    }
    return w;
}

void bitmap_set(unsigned long *d, unsigned int start, unsigned int n)
{
    while (n--)
        __set_bit(start++, d);
}

void bitmap_clear(unsigned long *d, unsigned int start, unsigned int n)
{
    while (n--)
        __clear_bit(start++, d);
}

void bitmap_shift_left(unsigned long *d, const unsigned long *s, unsigned int shift, unsigned int n)
{
    unsigned int i;
    unsigned long *t = calloc(WORDS(n), sizeof(long));
    if (!t)
        abort();
    for (i = 0; i + shift < n; i++)
        if (test_bit(i, s))
            __set_bit(i + shift, t);
    memcpy(d, t, WORDS(n) * sizeof(long));
    free(t);
}

void bitmap_shift_right(unsigned long *d, const unsigned long *s, unsigned int shift, unsigned int n)
{
    unsigned int i;
    unsigned long *t = calloc(WORDS(n), sizeof(long));
    if (!t)
        abort();
    for (i = shift; i < n; i++)
        if (test_bit(i, s))
            __set_bit(i - shift, t);
    memcpy(d, t, WORDS(n) * sizeof(long));
    free(t);
}

static unsigned long find_bit(const unsigned long *a, unsigned long n, unsigned long start, unsigned long invert)
{
    unsigned long w;

    if (start >= n)
        return n;
    w = (a[start / BITS_PER_LONG] ^ invert) & (~0UL << (start % BITS_PER_LONG));
    start = ALIGN_DOWN(start, BITS_PER_LONG);
    while (!w) {
        start += BITS_PER_LONG;
        if (start >= n)
            return n;
        w = a[start / BITS_PER_LONG] ^ invert;
    }
    return min(start + __ffs(w), n);
}

unsigned long find_next_bit(const unsigned long *a, unsigned long n, unsigned long s) { return find_bit(a, n, s, 0); }
unsigned long find_next_zero_bit(const unsigned long *a, unsigned long n, unsigned long s) { return find_bit(a, n, s, ~0UL); }
unsigned long find_first_bit(const unsigned long *a, unsigned long n) { return find_bit(a, n, 0, 0); }
unsigned long find_first_zero_bit(const unsigned long *a, unsigned long n) { return find_bit(a, n, 0, ~0UL); }

unsigned long find_last_bit(const unsigned long *a, unsigned long n)
{
    unsigned long i = n;
    while (i-- > 0)
        if (test_bit(i, a))
            return i;
    return n;
}

/* ---- Red-black trees. Colour in bit 0 of __rb_parent_color: 0 red,
 *      1 black. ---- */
#define RB_RED 0UL
#define RB_BLACK 1UL
#define rb_color(n) ((n)->__rb_parent_color & 1UL)
#define rb_is_black(n) (!(n) || rb_color(n) == RB_BLACK)
#define rb_is_red(n) (!rb_is_black(n))

static void rb_set_parent(struct rb_node *n, struct rb_node *p)
{
    n->__rb_parent_color = rb_color(n) | (unsigned long)p;
}

static void rb_set_color(struct rb_node *n, unsigned long c)
{
    n->__rb_parent_color = (n->__rb_parent_color & ~1UL) | c;
}

static void rb_replace_child(struct rb_node *old, struct rb_node *new, struct rb_node *parent, struct rb_root *root)
{
    if (!parent)
        root->rb_node = new;
    else if (parent->rb_left == old)
        parent->rb_left = new;
    else
        parent->rb_right = new;
}

static void rb_rotate_left(struct rb_node *x, struct rb_root *root)
{
    struct rb_node *y = x->rb_right, *p = rb_parent(x);
    x->rb_right = y->rb_left;
    if (y->rb_left)
        rb_set_parent(y->rb_left, x);
    rb_set_parent(y, p);
    rb_replace_child(x, y, p, root);
    y->rb_left = x;
    rb_set_parent(x, y);
}

static void rb_rotate_right(struct rb_node *x, struct rb_root *root)
{
    struct rb_node *y = x->rb_left, *p = rb_parent(x);
    x->rb_left = y->rb_right;
    if (y->rb_right)
        rb_set_parent(y->rb_right, x);
    rb_set_parent(y, p);
    rb_replace_child(x, y, p, root);
    y->rb_right = x;
    rb_set_parent(x, y);
}

void rb_insert_color(struct rb_node *z, struct rb_root *root)
{
    struct rb_node *p, *g, *u;

    rb_set_color(z, RB_RED);
    while ((p = rb_parent(z)) && rb_is_red(p)) {
        g = rb_parent(p);
        if (p == g->rb_left) {
            u = g->rb_right;
            if (rb_is_red(u)) {
                rb_set_color(p, RB_BLACK);
                rb_set_color(u, RB_BLACK);
                rb_set_color(g, RB_RED);
                z = g;
                continue;
            }
            if (z == p->rb_right) {
                rb_rotate_left(p, root);
                z = p;
                p = rb_parent(z);
            }
            rb_set_color(p, RB_BLACK);
            rb_set_color(g, RB_RED);
            rb_rotate_right(g, root);
        } else {
            u = g->rb_left;
            if (rb_is_red(u)) {
                rb_set_color(p, RB_BLACK);
                rb_set_color(u, RB_BLACK);
                rb_set_color(g, RB_RED);
                z = g;
                continue;
            }
            if (z == p->rb_left) {
                rb_rotate_right(p, root);
                z = p;
                p = rb_parent(z);
            }
            rb_set_color(p, RB_BLACK);
            rb_set_color(g, RB_RED);
            rb_rotate_left(g, root);
        }
    }
    rb_set_color(root->rb_node, RB_BLACK);
}

static struct rb_node *rb_min(struct rb_node *n)
{
    while (n->rb_left)
        n = n->rb_left;
    return n;
}

/* Put v in u's place under u's parent. */
static void rb_transplant(struct rb_node *u, struct rb_node *v, struct rb_root *root)
{
    struct rb_node *p = rb_parent(u);
    rb_replace_child(u, v, p, root);
    if (v)
        rb_set_parent(v, p);
}

void rb_erase(struct rb_node *z, struct rb_root *root)
{
    struct rb_node *x, *xp, *y = z;
    unsigned long y_color = rb_color(y);

    if (!z->rb_left) {
        x = z->rb_right;
        xp = rb_parent(z);
        rb_transplant(z, z->rb_right, root);
    } else if (!z->rb_right) {
        x = z->rb_left;
        xp = rb_parent(z);
        rb_transplant(z, z->rb_left, root);
    } else {
        y = rb_min(z->rb_right);
        y_color = rb_color(y);
        x = y->rb_right;
        if (rb_parent(y) == z) {
            xp = y;
        } else {
            xp = rb_parent(y);
            rb_transplant(y, y->rb_right, root);
            y->rb_right = z->rb_right;
            rb_set_parent(y->rb_right, y);
        }
        rb_transplant(z, y, root);
        y->rb_left = z->rb_left;
        rb_set_parent(y->rb_left, y);
        rb_set_color(y, rb_color(z));
    }
    if (y_color != RB_BLACK)
        return;
    /* Fix the extra black at x (possibly NULL) under xp. */
    while (x != root->rb_node && rb_is_black(x)) {
        struct rb_node *w;
        if (x == xp->rb_left) {
            w = xp->rb_right;
            if (rb_is_red(w)) {
                rb_set_color(w, RB_BLACK);
                rb_set_color(xp, RB_RED);
                rb_rotate_left(xp, root);
                w = xp->rb_right;
            }
            if (rb_is_black(w->rb_left) && rb_is_black(w->rb_right)) {
                rb_set_color(w, RB_RED);
                x = xp;
                xp = rb_parent(x);
            } else {
                if (rb_is_black(w->rb_right)) {
                    rb_set_color(w->rb_left, RB_BLACK);
                    rb_set_color(w, RB_RED);
                    rb_rotate_right(w, root);
                    w = xp->rb_right;
                }
                rb_set_color(w, rb_color(xp));
                rb_set_color(xp, RB_BLACK);
                if (w->rb_right)
                    rb_set_color(w->rb_right, RB_BLACK);
                rb_rotate_left(xp, root);
                x = root->rb_node;
                break;
            }
        } else {
            w = xp->rb_left;
            if (rb_is_red(w)) {
                rb_set_color(w, RB_BLACK);
                rb_set_color(xp, RB_RED);
                rb_rotate_right(xp, root);
                w = xp->rb_left;
            }
            if (rb_is_black(w->rb_left) && rb_is_black(w->rb_right)) {
                rb_set_color(w, RB_RED);
                x = xp;
                xp = rb_parent(x);
            } else {
                if (rb_is_black(w->rb_left)) {
                    rb_set_color(w->rb_right, RB_BLACK);
                    rb_set_color(w, RB_RED);
                    rb_rotate_left(w, root);
                    w = xp->rb_left;
                }
                rb_set_color(w, rb_color(xp));
                rb_set_color(xp, RB_BLACK);
                if (w->rb_left)
                    rb_set_color(w->rb_left, RB_BLACK);
                rb_rotate_right(xp, root);
                x = root->rb_node;
                break;
            }
        }
    }
    if (x)
        rb_set_color(x, RB_BLACK);
}

struct rb_node *rb_first(const struct rb_root *root)
{
    return root->rb_node ? rb_min(root->rb_node) : NULL;
}

struct rb_node *rb_last(const struct rb_root *root)
{
    struct rb_node *n = root->rb_node;
    if (!n)
        return NULL;
    while (n->rb_right)
        n = n->rb_right;
    return n;
}

struct rb_node *rb_next(const struct rb_node *n)
{
    struct rb_node *p;

    if (n->rb_right)
        return rb_min(n->rb_right);
    while ((p = rb_parent(n)) && n == p->rb_right)
        n = p;
    return p;
}

struct rb_node *rb_prev(const struct rb_node *n)
{
    struct rb_node *p;

    if (n->rb_left) {
        n = n->rb_left;
        while (n->rb_right)
            n = n->rb_right;
        return (struct rb_node *)n;
    }
    while ((p = rb_parent(n)) && n == p->rb_left)
        n = p;
    return p;
}

void rb_replace_node(struct rb_node *victim, struct rb_node *n, struct rb_root *root)
{
    struct rb_node *p = rb_parent(victim);

    *n = *victim;
    if (victim->rb_left)
        rb_set_parent(victim->rb_left, n);
    if (victim->rb_right)
        rb_set_parent(victim->rb_right, n);
    rb_replace_child(victim, n, p, root);
}

/* ---- "Radix tree": a sorted array of (index, item). ---- */
struct rmap {
    unsigned long n, cap;
    unsigned long *index;
    void **item;
};

static unsigned long rmap_find(const struct rmap *m, unsigned long index)
{
    unsigned long lo = 0, hi = m->n;
    while (lo < hi) {
        unsigned long mid = (lo + hi) / 2;
        if (m->index[mid] < index)
            lo = mid + 1;
        else
            hi = mid;
    }
    return lo;
}

int radix_tree_insert(struct radix_tree_root *r, unsigned long index, void *item)
{
    struct rmap *m = r->kpi_map;
    unsigned long at;

    if (!m) {
        m = calloc(1, sizeof(*m));
        if (!m)
            return -ENOMEM;
        r->kpi_map = m;
    }
    at = rmap_find(m, index);
    if (at < m->n && m->index[at] == index)
        return -EEXIST;
    if (m->n == m->cap) {
        unsigned long cap = m->cap ? m->cap * 2 : 16;
        unsigned long *ni = realloc(m->index, cap * sizeof(*ni));
        void **nv;
        if (!ni)
            return -ENOMEM;
        m->index = ni;
        nv = realloc(m->item, cap * sizeof(*nv));
        if (!nv)
            return -ENOMEM;
        m->item = nv;
        m->cap = cap;
    }
    memmove(&m->index[at + 1], &m->index[at], (m->n - at) * sizeof(*m->index));
    memmove(&m->item[at + 1], &m->item[at], (m->n - at) * sizeof(*m->item));
    m->index[at] = index;
    m->item[at] = item;
    m->n++;
    return 0;
}

void *radix_tree_lookup(const struct radix_tree_root *r, unsigned long index)
{
    const struct rmap *m = r->kpi_map;
    unsigned long at;

    if (!m)
        return NULL;
    at = rmap_find(m, index);
    return (at < m->n && m->index[at] == index) ? m->item[at] : NULL;
}

void *radix_tree_delete(struct radix_tree_root *r, unsigned long index)
{
    struct rmap *m = r->kpi_map;
    unsigned long at;
    void *item;

    if (!m)
        return NULL;
    at = rmap_find(m, index);
    if (at >= m->n || m->index[at] != index)
        return NULL;
    item = m->item[at];
    memmove(&m->index[at], &m->index[at + 1], (m->n - at - 1) * sizeof(*m->index));
    memmove(&m->item[at], &m->item[at + 1], (m->n - at - 1) * sizeof(*m->item));
    if (--m->n == 0) {
        free(m->index);
        free(m->item);
        free(m);
        r->kpi_map = NULL;
    }
    return item;
}

unsigned int radix_tree_gang_lookup(const struct radix_tree_root *r, void **results,
                                    unsigned long first, unsigned int max)
{
    const struct rmap *m = r->kpi_map;
    unsigned long at;
    unsigned int n = 0;

    if (!m)
        return 0;
    for (at = rmap_find(m, first); at < m->n && n < max; at++)
        results[n++] = m->item[at];
    return n;
}

void **kpi_radix_next_slot(const struct radix_tree_root *r, struct radix_tree_iter *it, unsigned long start)
{
    const struct rmap *m = r->kpi_map;
    unsigned long at;

    if (!m)
        return NULL;
    at = rmap_find(m, start);
    if (at >= m->n)
        return NULL;
    it->index = m->index[at];
    return &m->item[at];
}

/* ---- Randomness. ---- */
void get_random_bytes(void *buf, size_t n)
{
    char *p = buf;
    while (n) {
        long r = getrandom(p, n, 0);
        if (r <= 0)
            abort();
        p += r;
        n -= (size_t)r;
    }
}

u32 get_random_u32(void)
{
    u32 v;
    get_random_bytes(&v, sizeof(v));
    return v;
}

u64 get_random_u64(void)
{
    u64 v;
    get_random_bytes(&v, sizeof(v));
    return v;
}

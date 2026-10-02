/*
 * uvm-kpi: memory. The heap, slab caches, vmalloc, and the page pool.
 *
 * The page pool is one shared-memory object (a memfd here; a VMO pinned
 * into the card's domain in nvrm, docs/NVIDIA.md §4.3) mapped whole at a
 * fixed reservation, so page_address() is arithmetic and vmap() can map
 * any set of pool pages contiguously somewhere else by mapping the same
 * object again. Pages are handed out first-fit from a bitmap, aligned to
 * their order, as a buddy allocator would align them.
 *
 * In the self-test build there is no device: a page's "physical" address
 * is its offset in the pool plus KPI_FAKE_PHYS_BASE, which is enough for
 * UVM's bookkeeping and never reaches hardware.
 *
 * SPDX-License-Identifier: MIT
 */
#include "libc.h"

/* ---- The heap. Each block carries its size, for ksize and krealloc, in
 *      a header that keeps the payload cache-line aligned: UVM puts
 *      ____cacheline_aligned types in kmalloc memory. ---- */
#define KBLOCK_HEADER 64

struct kblock {
    size_t size;
};

void *kmalloc(size_t size, gfp_t flags)
{
    void *raw = NULL;
    struct kblock *b;

    if (size == 0)
        return ZERO_SIZE_PTR;
    if (posix_memalign(&raw, KBLOCK_HEADER, KBLOCK_HEADER + size) != 0)
        return NULL;
    if (flags & __GFP_ZERO)
        memset(raw, 0, KBLOCK_HEADER + size);
    b = raw;
    b->size = size;
    return (char *)raw + KBLOCK_HEADER;
}

void *kzalloc(size_t size, gfp_t flags)
{
    return kmalloc(size, flags | __GFP_ZERO);
}

void kfree(const void *p)
{
    if (ZERO_OR_NULL_PTR(p))
        return;
    free((char *)p - KBLOCK_HEADER);
}

size_t ksize(const void *p)
{
    return ZERO_OR_NULL_PTR(p) ? 0 : ((const struct kblock *)((const char *)p - KBLOCK_HEADER))->size;
}

void *krealloc(const void *p, size_t size, gfp_t flags)
{
    void *n;

    if (ZERO_OR_NULL_PTR(p))
        return kmalloc(size, flags);
    if (size == 0) {
        kfree(p);
        return ZERO_SIZE_PTR;
    }
    if (size <= ksize(p))
        return (void *)p;
    n = kmalloc(size, flags);
    if (!n)
        return NULL;
    memcpy(n, p, ksize(p));
    if (flags & __GFP_ZERO)
        memset((char *)n + ksize(p), 0, size - ksize(p));
    kfree(p);
    return n;
}

/* vmalloc memory comes from the heap too, page-aligned, and is told apart
 * from kmalloc memory by a registry (is_vmalloc_addr). */
struct vblock {
    struct list_head entry;
    void *addr;
    size_t size;
    struct page **pages; /* set by vmap */
    unsigned int count;
};
static int vlock;
static LIST_HEAD(vblocks);

static struct vblock *vfind(const void *a)
{
    struct vblock *v;
    list_for_each_entry(v, &vblocks, entry) {
        if ((const char *)a >= (const char *)v->addr && (const char *)a < (const char *)v->addr + v->size)
            return v;
    }
    return NULL;
}

void *vmalloc(unsigned long size)
{
    struct vblock *v = malloc(sizeof(*v));
    void *p = NULL;

    if (!v)
        return NULL;
    size = PAGE_ALIGN(size ? size : 1);
    if (posix_memalign(&p, PAGE_SIZE, size) != 0) {
        free(v);
        return NULL;
    }
    v->addr = p;
    v->size = size;
    v->pages = NULL;
    v->count = 0;
    kpi_spin_lock(&vlock);
    list_add(&v->entry, &vblocks);
    kpi_spin_unlock(&vlock);
    return p;
}

void *vzalloc(unsigned long size)
{
    void *p = vmalloc(size);
    if (p)
        memset(p, 0, PAGE_ALIGN(size ? size : 1));
    return p;
}

bool is_vmalloc_addr(const void *a)
{
    bool r;
    kpi_spin_lock(&vlock);
    r = vfind(a) != NULL;
    kpi_spin_unlock(&vlock);
    return r;
}

void vfree(const void *p)
{
    struct vblock *v;

    if (!p)
        return;
    kpi_spin_lock(&vlock);
    v = vfind(p);
    if (v)
        list_del(&v->entry);
    kpi_spin_unlock(&vlock);
    if (!v)
        panic("uvm-kpi: vfree of %p, which vmalloc did not give", p);
    free(v->addr);
    free(v);
}

void kvfree(const void *p)
{
    if (is_vmalloc_addr(p))
        vfree(p);
    else
        kfree(p);
}

/* ---- Slab caches: sized heap blocks. ---- */
struct kmem_cache {
    size_t size;
    size_t align;
    void (*ctor)(void *);
};

struct kmem_cache *kmem_cache_create(const char *name, unsigned int size, unsigned int align,
                                     unsigned long flags, void (*ctor)(void *))
{
    struct kmem_cache *c = malloc(sizeof(*c));

    (void)name;
    (void)flags;
    if (!c)
        return NULL;
    c->size = size;
    c->align = align;
    c->ctor = ctor;
    return c;
}

void *kmem_cache_alloc(struct kmem_cache *c, gfp_t flags)
{
    void *p = kmalloc(c->size, flags);
    if (p && c->ctor)
        c->ctor(p);
    return p;
}

void *kmem_cache_zalloc(struct kmem_cache *c, gfp_t flags)
{
    return kmem_cache_alloc(c, flags | __GFP_ZERO);
}

void kmem_cache_free(struct kmem_cache *c, void *p)
{
    (void)c;
    kfree(p);
}

void kmem_cache_destroy(struct kmem_cache *c)
{
    free(c);
}

/* ---- The page pool. ---- */
#define POOL_MAX_PAGES (1UL << 22)              /* 16 GiB of reservation */
#define KPI_FAKE_PHYS_BASE (1ULL << 40)

static int pool_lock;
static int pool_fd = -1;
static char *pool_base;
static unsigned long pool_pages;                /* pages backed so far */
static unsigned long *pool_used;                /* one bit per page */
static struct page *pool_page;                  /* one descriptor per page */
static struct page *zero_page;

static int pool_init(void)
{
    if (pool_base)
        return 0;
    pool_fd = memfd_create("uvm-kpi-pool", KPI_MFD_CLOEXEC);
    if (pool_fd < 0)
        return -ENOMEM;
    pool_base = mmap(NULL, POOL_MAX_PAGES * PAGE_SIZE, KPI_PROT_NONE,
                     KPI_MAP_PRIVATE | KPI_MAP_ANONYMOUS | KPI_MAP_NORESERVE, -1, 0);
    if (pool_base == KPI_MAP_FAILED) {
        pool_base = NULL;
        return -ENOMEM;
    }
    pool_used = calloc(BITS_TO_LONGS(POOL_MAX_PAGES), sizeof(unsigned long));
    pool_page = mmap(NULL, POOL_MAX_PAGES * sizeof(struct page), KPI_PROT_READ | KPI_PROT_WRITE,
                     KPI_MAP_PRIVATE | KPI_MAP_ANONYMOUS | KPI_MAP_NORESERVE, -1, 0);
    if (!pool_used || pool_page == KPI_MAP_FAILED)
        return -ENOMEM;
    return 0;
}

/* Back the pool up to `pages` pages, in 64 MiB steps. */
static int pool_grow(unsigned long pages)
{
    unsigned long target = ALIGN(pages, 16384UL);

    if (target <= pool_pages)
        return 0;
    if (target > POOL_MAX_PAGES)
        return -ENOMEM;
    if (ftruncate(pool_fd, (long)(target * PAGE_SIZE)) != 0)
        return -ENOMEM;
    if (mmap(pool_base + pool_pages * PAGE_SIZE, (target - pool_pages) * PAGE_SIZE,
             KPI_PROT_READ | KPI_PROT_WRITE, KPI_MAP_SHARED | KPI_MAP_FIXED, pool_fd,
             (long)(pool_pages * PAGE_SIZE)) == KPI_MAP_FAILED)
        return -ENOMEM;
    pool_pages = target;
    return 0;
}

struct page *alloc_pages(gfp_t flags, unsigned int order)
{
    unsigned long n = 1UL << order, i, start;
    struct page *head = NULL;

    kpi_spin_lock(&pool_lock);
    if (pool_init() != 0)
        goto out;
    for (start = 0;; start += n) {
        if (start + n > pool_pages && pool_grow(start + n) != 0)
            goto out;
        if (find_next_bit(pool_used, start + n, start) >= start + n)
            break;
    }
    for (i = 0; i < n; i++) {
        struct page *p = &pool_page[start + i];
        set_bit(start + i, pool_used);
        memset(p, 0, sizeof(*p));
        p->kpi_address = pool_base + (start + i) * PAGE_SIZE;
        p->kpi_phys = KPI_FAKE_PHYS_BASE + (start + i) * PAGE_SIZE;
        p->kpi_head = i ? &pool_page[start] : NULL;
        INIT_LIST_HEAD(&p->lru);
        atomic_set(&p->_mapcount, -1);
    }
    head = &pool_page[start];
    head->kpi_order = order;
    atomic_set(&head->_refcount, 1);
out:
    kpi_spin_unlock(&pool_lock);
    if (head && (flags & __GFP_ZERO))
        memset(page_address(head), 0, n * PAGE_SIZE);
    return head;
}

void __free_pages(struct page *p, unsigned int order)
{
    unsigned long first, n = 1UL << order, i;

    if (!p)
        return;
    if (atomic_dec_return(&p->_refcount) > 0)
        return;
    first = (unsigned long)(p - pool_page);
    kpi_spin_lock(&pool_lock);
    for (i = 0; i < n; i++) {
        pool_page[first + i].kpi_address = NULL;
        clear_bit(first + i, pool_used);
    }
    kpi_spin_unlock(&pool_lock);
}

void put_page(struct page *p)
{
    struct page *h = compound_head(p);
    __free_pages(h, h->kpi_order);
}

unsigned long __get_free_pages(gfp_t flags, unsigned int order)
{
    struct page *p = alloc_pages(flags, order);
    return p ? (unsigned long)page_address(p) : 0;
}

void free_pages(unsigned long addr, unsigned int order)
{
    if (addr)
        __free_pages(virt_to_page((void *)addr), order);
}

struct page *virt_to_page(const void *a)
{
    const char *c = a;
    struct vblock *v;
    struct page *p = NULL;

    if (pool_base && c >= pool_base && c < pool_base + pool_pages * PAGE_SIZE)
        return &pool_page[(c - pool_base) >> PAGE_SHIFT];
    kpi_spin_lock(&vlock);
    v = vfind(a);
    if (v && v->pages)
        p = v->pages[(c - (const char *)v->addr) >> PAGE_SHIFT];
    kpi_spin_unlock(&vlock);
    return p;
}

struct page *vmalloc_to_page(const void *a)
{
    return virt_to_page(a);
}

struct page *pfn_to_page(unsigned long pfn)
{
    u64 phys = (u64)pfn << PAGE_SHIFT;

    if (phys < KPI_FAKE_PHYS_BASE || phys >= KPI_FAKE_PHYS_BASE + pool_pages * PAGE_SIZE)
        return NULL;
    return &pool_page[(phys - KPI_FAKE_PHYS_BASE) >> PAGE_SHIFT];
}

void *phys_to_virt(u64 phys)
{
    struct page *p = pfn_to_page(phys >> PAGE_SHIFT);
    return p ? (char *)page_address(p) + offset_in_page(phys) : NULL;
}

struct page *kpi_zero_page(void)
{
    if (!zero_page)
        zero_page = alloc_pages(GFP_KERNEL | __GFP_ZERO, 0);
    return zero_page;
}

/* vmap: map the pages' pool offsets contiguously at a fresh address. */
void *vmap(struct page **pages, unsigned int count, unsigned long flags, pgprot_t prot)
{
    struct vblock *v = malloc(sizeof(*v));
    char *base;
    unsigned int i;

    (void)flags;
    (void)prot;
    if (!v)
        return NULL;
    base = mmap(NULL, (size_t)count * PAGE_SIZE, KPI_PROT_NONE,
                KPI_MAP_PRIVATE | KPI_MAP_ANONYMOUS | KPI_MAP_NORESERVE, -1, 0);
    if (base == KPI_MAP_FAILED) {
        free(v);
        return NULL;
    }
    for (i = 0; i < count; i++) {
        long off = (long)((char *)page_address(pages[i]) - pool_base);
        if (mmap(base + (size_t)i * PAGE_SIZE, PAGE_SIZE, KPI_PROT_READ | KPI_PROT_WRITE,
                 KPI_MAP_SHARED | KPI_MAP_FIXED, pool_fd, off) == KPI_MAP_FAILED) {
            munmap(base, (size_t)count * PAGE_SIZE);
            free(v);
            return NULL;
        }
    }
    v->addr = base;
    v->size = (size_t)count * PAGE_SIZE;
    v->pages = malloc(count * sizeof(*pages));
    v->count = count;
    if (!v->pages) {
        munmap(base, v->size);
        free(v);
        return NULL;
    }
    memcpy(v->pages, pages, count * sizeof(*pages));
    kpi_spin_lock(&vlock);
    list_add(&v->entry, &vblocks);
    kpi_spin_unlock(&vlock);
    return base;
}

void vunmap(const void *a)
{
    struct vblock *v;

    if (!a)
        return;
    kpi_spin_lock(&vlock);
    v = vfind(a);
    if (v)
        list_del(&v->entry);
    kpi_spin_unlock(&vlock);
    if (!v || !v->pages)
        panic("uvm-kpi: vunmap of %p, which vmap did not give", a);
    munmap(v->addr, v->size);
    free(v->pages);
    free(v);
}

unsigned int memalloc_noreclaim_save(void)
{
    return 0;
}

void memalloc_noreclaim_restore(unsigned int f)
{
    (void)f;
}

unsigned long kpi_totalram_pages(void)
{
    return POOL_MAX_PAGES;
}

void si_meminfo(struct sysinfo *i)
{
    memset(i, 0, sizeof(*i));
    i->totalram = POOL_MAX_PAGES;
    i->freeram = POOL_MAX_PAGES - pool_pages;
    i->mem_unit = PAGE_SIZE;
}

struct cpuinfo_x86 boot_cpu_data = { .x86 = 6 };

/* ---- DMA: the pool's address is the device's (identity domain). ---- */
dma_addr_t dma_map_page_attrs(struct device *d, struct page *p, size_t offset, size_t size,
                              enum dma_data_direction dir, unsigned long attrs)
{
    (void)d;
    (void)size;
    (void)dir;
    (void)attrs;
    return page_to_phys(p) + offset;
}

void dma_unmap_page_attrs(struct device *d, dma_addr_t a, size_t size,
                          enum dma_data_direction dir, unsigned long attrs)
{
    (void)d;
    (void)a;
    (void)size;
    (void)dir;
    (void)attrs;
}

void *dma_alloc_coherent(struct device *d, size_t size, dma_addr_t *handle, gfp_t flags)
{
    struct page *p = alloc_pages(flags | __GFP_ZERO, get_order(size));

    (void)d;
    if (!p)
        return NULL;
    *handle = page_to_phys(p);
    return page_address(p);
}

void dma_free_coherent(struct device *d, size_t size, void *cpu, dma_addr_t handle)
{
    (void)d;
    (void)handle;
    free_pages((unsigned long)cpu, get_order(size));
}

int sg_alloc_table(struct sg_table *t, unsigned int nents, gfp_t flags)
{
    t->sgl = kzalloc(nents * sizeof(*t->sgl), flags);
    if (!t->sgl)
        return -ENOMEM;
    t->nents = t->orig_nents = nents;
    return 0;
}

int sg_alloc_table_from_pages(struct sg_table *t, struct page **pages, unsigned int n,
                              unsigned int offset, unsigned long size, gfp_t flags)
{
    unsigned int i;
    int r = sg_alloc_table(t, n, flags);

    if (r)
        return r;
    for (i = 0; i < n; i++) {
        unsigned int len = (unsigned int)min_t(unsigned long, size, PAGE_SIZE - (i ? 0 : offset));
        sg_set_page(&t->sgl[i], pages[i], len, i ? 0 : offset);
        size -= len;
    }
    return 0;
}

void sg_free_table(struct sg_table *t)
{
    kfree(t->sgl);
    t->sgl = NULL;
}

int dma_map_sg_attrs(struct device *d, struct scatterlist *sg, int nents,
                     enum dma_data_direction dir, unsigned long attrs)
{
    int i;

    (void)d;
    (void)dir;
    (void)attrs;
    for (i = 0; i < nents; i++) {
        sg[i].dma_address = page_to_phys(sg_page(&sg[i])) + sg[i].offset;
        sg[i].dma_length = sg[i].length;
    }
    return nents;
}

void dma_unmap_sg_attrs(struct device *d, struct scatterlist *sg, int nents,
                        enum dma_data_direction dir, unsigned long attrs)
{
    (void)d;
    (void)sg;
    (void)nents;
    (void)dir;
    (void)attrs;
}

bool kpi_sg_dma_page_next(struct sg_dma_page_iter *it)
{
    if (!it->nents)
        return false;
    it->page_off++;
    if (it->page_off >= DIV_ROUND_UP(sg_dma_len(it->sg), PAGE_SIZE)) {
        if (--it->nents == 0)
            return false;
        it->sg = sg_next(it->sg);
        it->page_off = 0;
    }
    return true;
}

/* ---- Device windows: none without a device. ---- */
struct resource iomem_resource = { 0, ~0ULL, "PCI mem", 0 };

void __iomem *ioremap(phys_addr_t phys, size_t size)
{
    (void)phys;
    (void)size;
    return NULL;
}

void iounmap(volatile void __iomem *a)
{
    (void)a;
}

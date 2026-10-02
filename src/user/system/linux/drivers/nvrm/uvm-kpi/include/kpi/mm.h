/*
 * uvm-kpi: memory. Allocation, pages, the client's mappings, user copies
 * and DMA.
 *
 * A struct page describes one 4 KiB page of nvrm's page pool (kpi/page.c).
 * page_address() is where nvrm maps it, page_to_phys() its address for
 * the device. vm_area_struct, mm_struct and file stand for a client's
 * mapping, address space and open file as nvrm's request bridge describes
 * them (docs/NVIDIA.md §11.4); in the self-test build there is no client.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef FERRIX_KPI_MM_H
#define FERRIX_KPI_MM_H

/* ---- Allocation flags. Only __GFP_ZERO and the no-wait bits change what
 *      kpi does; the rest are accepted and ignored. ---- */
#define ___GFP_ZERO 0x100u
#define __GFP_DMA 0x01u
#define __GFP_HIGHMEM 0x02u
#define __GFP_DMA32 0x04u
#define __GFP_MOVABLE 0x08u
#define __GFP_RECLAIMABLE 0x10u
#define __GFP_HIGH 0x20u
#define __GFP_IO 0x40u
#define __GFP_FS 0x80u
#define __GFP_ZERO ___GFP_ZERO
#define __GFP_ATOMIC 0x200u
#define __GFP_DIRECT_RECLAIM 0x400u
#define __GFP_KSWAPD_RECLAIM 0x800u
#define __GFP_WRITE 0x1000u
#define __GFP_NOWARN 0x2000u
#define __GFP_RETRY_MAYFAIL 0x4000u
#define __GFP_NOFAIL 0x8000u
#define __GFP_NORETRY 0x10000u
#define __GFP_MEMALLOC 0x20000u
#define __GFP_COMP 0x40000u
#define __GFP_NOMEMALLOC 0x80000u
#define __GFP_HARDWALL 0x100000u
#define __GFP_THISNODE 0x200000u
#define __GFP_ACCOUNT 0x400000u
#define __GFP_RECLAIM (__GFP_DIRECT_RECLAIM | __GFP_KSWAPD_RECLAIM)
#define GFP_ATOMIC (__GFP_HIGH | __GFP_ATOMIC | __GFP_KSWAPD_RECLAIM)
#define GFP_NOWAIT (__GFP_KSWAPD_RECLAIM)
#define GFP_KERNEL (__GFP_RECLAIM | __GFP_IO | __GFP_FS)
#define GFP_NOIO (__GFP_RECLAIM)
#define GFP_NOFS (__GFP_RECLAIM | __GFP_IO)
#define GFP_USER (GFP_KERNEL | __GFP_HARDWALL)
#define GFP_HIGHUSER (GFP_USER | __GFP_HIGHMEM)
#define GFP_HIGHUSER_MOVABLE (GFP_HIGHUSER | __GFP_MOVABLE)
#define GFP_DMA32 __GFP_DMA32
#define gfpflags_allow_blocking(f) (((f) & __GFP_DIRECT_RECLAIM) != 0)
unsigned int memalloc_noreclaim_save(void);
void memalloc_noreclaim_restore(unsigned int);
#define memalloc_nofs_save() 0u
#define memalloc_nofs_restore(f) ((void)(f))
#define memalloc_noio_save() 0u
#define memalloc_noio_restore(f) ((void)(f))

/* ---- Heap. ---- */
void *kmalloc(size_t, gfp_t);
void *kzalloc(size_t, gfp_t);
void *krealloc(const void *, size_t, gfp_t);
void kfree(const void *);
void kvfree(const void *);
size_t ksize(const void *);
#define kmalloc_node(s, f, n) kmalloc(s, f)
#define kzalloc_node(s, f, n) kzalloc(s, f)
#define kcalloc(n, s, f) kzalloc((n) * (s), f)
#define kmalloc_array(n, s, f) kmalloc((n) * (s), f)
#define kvmalloc(s, f) kmalloc(s, f)
#define kvzalloc(s, f) kzalloc(s, f)
#define kvmalloc_array(n, s, f) kmalloc((n) * (s), f)
#define kvcalloc(n, s, f) kzalloc((n) * (s), f)
#define kfree_sensitive kfree
#define ZERO_OR_NULL_PTR(p) ((unsigned long)(p) <= (unsigned long)ZERO_SIZE_PTR)
#define kmemdup(p, s, f) ({ void *_d = kmalloc(s, f); if (_d) memcpy(_d, p, s); _d; })
#define ZERO_SIZE_PTR ((void *)16)
void *vmalloc(unsigned long);
void *vzalloc(unsigned long);
void vfree(const void *);
bool is_vmalloc_addr(const void *);
#define vmalloc_node(s, n) vmalloc(s)
#define __vmalloc(s, f) (((f) & __GFP_ZERO) ? vzalloc(s) : vmalloc(s))
struct kmem_cache;
struct kmem_cache *kmem_cache_create(const char *, unsigned int size, unsigned int align,
                                     unsigned long flags, void (*ctor)(void *));
#define kmem_cache_create_usercopy(n, s, a, f, uo, us, c) kmem_cache_create(n, s, a, f, c)
void *kmem_cache_alloc(struct kmem_cache *, gfp_t);
void *kmem_cache_zalloc(struct kmem_cache *, gfp_t);
#define kmem_cache_alloc_node(c, f, n) kmem_cache_alloc(c, f)
void kmem_cache_free(struct kmem_cache *, void *);
void kmem_cache_destroy(struct kmem_cache *);
#define SLAB_HWCACHE_ALIGN 0x2000UL
#define SLAB_ACCOUNT 0x4000000UL
#define SLAB_RECLAIM_ACCOUNT 0x20000UL
#define KMEM_CACHE(s, f) kmem_cache_create(#s, sizeof(struct s), __alignof__(struct s), f, NULL)

/* ---- Pages. ---- */
#define PAGE_SHIFT 12
#define PAGE_SIZE (1UL << PAGE_SHIFT)
#define PAGE_MASK (~(PAGE_SIZE - 1))
#define PAGE_ALIGN(x) ALIGN(x, PAGE_SIZE)
#define PAGE_ALIGNED(x) IS_ALIGNED((unsigned long)(x), PAGE_SIZE)
#define offset_in_page(p) ((unsigned long)(p) & ~PAGE_MASK)
#define HPAGE_SHIFT 21
#define HPAGE_SIZE (1UL << HPAGE_SHIFT)
#define PMD_SHIFT 21
#define PMD_SIZE (1UL << PMD_SHIFT)
#define PUD_SIZE (1UL << 30)
#define MAX_ORDER 10
#define MAX_PAGE_ORDER MAX_ORDER
#define TASK_SIZE (1UL << 47)
struct address_space;
struct page {
    unsigned long flags;
    atomic_t _refcount;
    atomic_t _mapcount;
    unsigned long private;
    struct address_space *mapping;
    pgoff_t index;
    struct list_head lru;
    void *kpi_address;       /* where nvrm maps it */
    u64 kpi_phys;            /* its device address */
    unsigned int kpi_order;  /* set on the head of a compound allocation */
    struct page *kpi_head;
    void *zone_device_data;
};
struct dev_pagemap;
static inline struct dev_pagemap *page_pgmap(const struct page *p) { (void)p; return NULL; }
struct folio { struct page page; };
#define page_folio(p) ((struct folio *)compound_head(p))
#define folio_page(f, n) (&(f)->page + (n))
#define PG_locked 0
#define PG_dirty 4
#define PG_head 6
#define PG_private 13
struct page *alloc_pages(gfp_t, unsigned int order);
#define alloc_page(f) alloc_pages(f, 0)
#define alloc_pages_node(n, f, o) alloc_pages(f, o)
#define __alloc_pages_node(n, f, o) alloc_pages(f, o)
void __free_pages(struct page *, unsigned int order);
#define __free_page(p) __free_pages(p, 0)
#define free_page(a) free_pages(a, 0)
unsigned long __get_free_pages(gfp_t, unsigned int order);
#define __get_free_page(f) __get_free_pages(f, 0)
#define get_zeroed_page(f) __get_free_pages((f) | __GFP_ZERO, 0)
void free_pages(unsigned long addr, unsigned int order);
static inline void *page_address(const struct page *p) { return p->kpi_address; }
#define lowmem_page_address page_address
struct page *virt_to_page(const void *);
#define virt_addr_valid(a) (virt_to_page((void *)(a)) != NULL)
static inline u64 page_to_phys(const struct page *p) { return p->kpi_phys; }
static inline unsigned long page_to_pfn(const struct page *p) { return p->kpi_phys >> PAGE_SHIFT; }
struct page *pfn_to_page(unsigned long pfn);
static inline u64 virt_to_phys(const void *a) { struct page *p = virt_to_page(a); return p ? page_to_phys(p) + offset_in_page(a) : 0; }
void *phys_to_virt(u64);
#define __pa(a) virt_to_phys((void *)(a))
#define pfn_valid(pfn) (pfn_to_page(pfn) != NULL)
#define phys_to_page(a) pfn_to_page((a) >> PAGE_SHIFT)
#define nth_page(p, n) ((p) + (n))
static inline struct page *compound_head(const struct page *p) { return p->kpi_head ? p->kpi_head : (struct page *)p; }
static inline unsigned int compound_order(const struct page *p) { return compound_head(p)->kpi_order; }
static inline bool PageCompound(const struct page *p) { return compound_head(p)->kpi_order != 0; }
static inline bool PageHead(const struct page *p) { return p->kpi_head == NULL && p->kpi_order != 0; }
static inline bool PageTail(const struct page *p) { return p->kpi_head != NULL; }
static inline bool PageHighMem(const struct page *p) { (void)p; return false; }
static inline int page_count(const struct page *p) { return atomic_read(&compound_head(p)->_refcount); }
static inline int page_ref_count(const struct page *p) { return atomic_read(&p->_refcount); }
static inline void get_page(struct page *p) { atomic_inc(&compound_head(p)->_refcount); }
void put_page(struct page *);
static inline void page_ref_inc(struct page *p) { atomic_inc(&p->_refcount); }
static inline void page_ref_dec(struct page *p) { atomic_dec(&p->_refcount); }
static inline int page_mapcount(struct page *p) { return atomic_read(&p->_mapcount) + 1; }
static inline int page_to_nid(const struct page *p) { (void)p; return 0; }
static inline void *kmap(struct page *p) { return page_address(p); }
static inline void kunmap(struct page *p) { (void)p; }
static inline void *kmap_atomic(struct page *p) { return page_address(p); }
static inline void kunmap_atomic(void *a) { (void)a; }
static inline void *kmap_local_page(struct page *p) { return page_address(p); }
static inline void kunmap_local(void *a) { (void)a; }
static inline void clear_highpage(struct page *p) { memset(page_address(p), 0, PAGE_SIZE); }
static inline void copy_highpage(struct page *t, struct page *f) { memcpy(page_address(t), page_address(f), PAGE_SIZE); }
static inline void lock_page(struct page *p) { wait_on_bit_lock(&compound_head(p)->flags, PG_locked, 0); }
static inline int trylock_page(struct page *p) { return !test_and_set_bit(PG_locked, &compound_head(p)->flags); }
static inline void unlock_page(struct page *p) { clear_bit_unlock(PG_locked, &compound_head(p)->flags); wake_up_bit(&p->flags, PG_locked); }
static inline bool PageLocked(const struct page *p) { return test_bit(PG_locked, &compound_head(p)->flags); }
static inline bool set_page_dirty(struct page *p) { return !test_and_set_bit(PG_dirty, &p->flags); }
#define set_page_dirty_lock set_page_dirty
static inline void SetPageDirty(struct page *p) { set_bit(PG_dirty, &p->flags); }
static inline bool PageDirty(const struct page *p) { return test_bit(PG_dirty, &p->flags); }
static inline bool TestClearPageDirty(struct page *p) { return test_and_clear_bit(PG_dirty, &p->flags); }
static inline void ClearPageDirty(struct page *p) { clear_bit(PG_dirty, &p->flags); }
static inline bool PageReserved(const struct page *p) { (void)p; return false; }
static inline bool PageAnon(const struct page *p) { (void)p; return false; }
static inline bool PageSwapCache(const struct page *p) { (void)p; return false; }
static inline bool is_zone_device_page(const struct page *p) { (void)p; return false; }
#define page_private(p) ((p)->private)
#define set_page_private(p, v) ((p)->private = (v))
void vunmap(const void *);
struct page *vmalloc_to_page(const void *);
#define VM_MAP 0x4
typedef struct { unsigned long pgprot; } pgprot_t;
#define _PAGE_PRESENT 0x001UL
#define _PAGE_RW 0x002UL
#define _PAGE_USER 0x004UL
#define _PAGE_PWT 0x008UL
#define _PAGE_PCD 0x010UL
#define _PAGE_PSE 0x080UL
#define _PAGE_NX (1UL << 63)
struct cpuinfo_x86 { u8 x86; u8 x86_vendor; u8 x86_model; };
extern struct cpuinfo_x86 boot_cpu_data;
void *vmap(struct page **, unsigned int count, unsigned long flags, pgprot_t prot);

#define __pgprot(x) ((pgprot_t){ (x) })
#define pgprot_val(x) ((x).pgprot)
#define PAGE_KERNEL __pgprot(3)
#define PAGE_KERNEL_RO __pgprot(1)
#define PAGE_KERNEL_NOCACHE __pgprot(0x13)
#define PAGE_SHARED __pgprot(7)
#define PAGE_READONLY __pgprot(5)
#define pgprot_writecombine(p) __pgprot(pgprot_val(p) | _PAGE_PWT)
#define pgprot_modify(o, n) (n)
#define pgprot_decrypted(p) (p)
#define pgprot_encrypted(p) (p)
#define set_memory_uc(a, n) 0
#define set_memory_wb(a, n) 0
#define set_memory_wc(a, n) 0
#define set_memory_encrypted(a, n) 0
#define set_memory_decrypted(a, n) 0
#define ZERO_PAGE(a) kpi_zero_page()
struct page *kpi_zero_page(void);
#define totalram_pages() kpi_totalram_pages()
unsigned long kpi_totalram_pages(void);
struct sysinfo { unsigned long totalram, freeram, sharedram, bufferram, totalswap, freeswap; unsigned int mem_unit; };
void si_meminfo(struct sysinfo *);

/* ---- A client's address space, as nvrm sees it. ---- */
typedef unsigned long vm_flags_t;
#define VM_NONE 0x0UL
#define VM_READ 0x1UL
#define VM_WRITE 0x2UL
#define VM_EXEC 0x4UL
#define VM_SHARED 0x8UL
#define VM_MAYREAD 0x10UL
#define VM_MAYWRITE 0x20UL
#define VM_MAYEXEC 0x40UL
#define VM_MAYSHARE 0x80UL
#define VM_IO 0x4000UL
#define VM_DONTCOPY 0x20000UL
#define VM_DONTEXPAND 0x40000UL
#define VM_ACCOUNT 0x100000UL
#define VM_NORESERVE 0x200000UL
#define VM_HUGETLB 0x400000UL
#define VM_PFNMAP 0x400UL
#define VM_MIXEDMAP 0x10000000UL
#define VM_DONTDUMP 0x4000000UL
#define VM_WIPEONFORK 0x2000000UL
#define VM_LOCKED 0x2000UL
#define VM_MIGRATABLE 0x0UL
struct vm_area_struct;
struct vm_fault;
struct vm_operations_struct {
    void (*open)(struct vm_area_struct *);
    void (*close)(struct vm_area_struct *);
    vm_fault_t (*fault)(struct vm_fault *);
    int (*access)(struct vm_area_struct *, unsigned long, void *, int, int);
    int (*mremap)(struct vm_area_struct *);
    int (*may_split)(struct vm_area_struct *, unsigned long);
    vm_fault_t (*page_mkwrite)(struct vm_fault *);
};
struct rw_semaphore;
struct mm_struct {
    struct rw_semaphore mmap_lock;
    atomic_t mm_users;
    atomic_t mm_count;
    unsigned long task_size;
    void *kpi_client;  /* the client this stands for */
};
struct file;
struct vm_area_struct {
    unsigned long vm_start;
    unsigned long vm_end;
    struct mm_struct *vm_mm;
    pgprot_t vm_page_prot;
    vm_flags_t vm_flags;
    unsigned long vm_pgoff;
    struct file *vm_file;
    void *vm_private_data;
    const struct vm_operations_struct *vm_ops;
    void *kpi_window;  /* the fault window backing it (K1) */
};
static inline void vm_flags_set(struct vm_area_struct *v, vm_flags_t f) { v->vm_flags |= f; }
static inline void vm_flags_clear(struct vm_area_struct *v, vm_flags_t f) { v->vm_flags &= ~f; }
static inline void vm_flags_init(struct vm_area_struct *v, vm_flags_t f) { v->vm_flags = f; }
#define VM_FAULT_OOM 0x0001
#define VM_FAULT_SIGBUS 0x0002
#define VM_FAULT_MAJOR 0x0004
#define VM_FAULT_HWPOISON 0x0010
#define VM_FAULT_SIGSEGV 0x0040
#define VM_FAULT_NOPAGE 0x0100
#define VM_FAULT_LOCKED 0x0200
#define VM_FAULT_RETRY 0x0400
#define VM_FAULT_FALLBACK 0x0800
#define VM_FAULT_DONE_COW 0x1000
#define VM_FAULT_ERROR (VM_FAULT_OOM | VM_FAULT_SIGBUS | VM_FAULT_SIGSEGV | VM_FAULT_HWPOISON | VM_FAULT_FALLBACK)
#define FAULT_FLAG_WRITE 0x01
#define FAULT_FLAG_MKWRITE 0x02
#define FAULT_FLAG_ALLOW_RETRY 0x04
#define FAULT_FLAG_RETRY_NOWAIT 0x08
#define FAULT_FLAG_KILLABLE 0x10
#define FAULT_FLAG_TRIED 0x20
#define FAULT_FLAG_USER 0x40
#define FAULT_FLAG_REMOTE 0x80
#define FAULT_FLAG_INSTRUCTION 0x100
struct vm_fault {
    struct vm_area_struct *vma;
    unsigned int flags;
    pgoff_t pgoff;
    unsigned long address;
    unsigned long real_address;
    struct page *page;
};
static inline int vm_fault_to_errno(vm_fault_t f, int foll) { (void)foll; return (f & VM_FAULT_OOM) ? -ENOMEM : (f & (VM_FAULT_SIGBUS | VM_FAULT_SIGSEGV)) ? -EFAULT : 0; }
pgprot_t vm_get_page_prot(unsigned long vm_flags);
/* K1's window_insert, one page. */
int vm_insert_page(struct vm_area_struct *, unsigned long addr, struct page *);
int vm_insert_pages(struct vm_area_struct *, unsigned long addr, struct page **, unsigned long *num);
vm_fault_t vmf_insert_pfn(struct vm_area_struct *, unsigned long addr, unsigned long pfn);
vm_fault_t vmf_insert_page(struct vm_area_struct *, unsigned long addr, struct page *);
int remap_pfn_range(struct vm_area_struct *, unsigned long addr, unsigned long pfn, unsigned long size, pgprot_t);
void zap_vma_ptes(struct vm_area_struct *, unsigned long addr, unsigned long size);
struct vm_area_struct *find_vma(struct mm_struct *, unsigned long addr);
struct vm_area_struct *find_vma_intersection(struct mm_struct *, unsigned long start, unsigned long end);
static inline struct vm_area_struct *vma_lookup(struct mm_struct *m, unsigned long a)
{ struct vm_area_struct *v = find_vma(m, a); return (v && a >= v->vm_start) ? v : NULL; }
static inline bool vma_is_anonymous(struct vm_area_struct *v) { return !v->vm_ops; }
static inline unsigned long vma_pages(struct vm_area_struct *v) { return (v->vm_end - v->vm_start) >> PAGE_SHIFT; }
static inline void vma_start_write(struct vm_area_struct *v) { (void)v; }
#define mmap_read_lock(m) down_read(&(m)->mmap_lock)
#define mmap_read_trylock(m) down_read_trylock(&(m)->mmap_lock)
#define mmap_read_unlock(m) up_read(&(m)->mmap_lock)
#define mmap_write_lock(m) down_write(&(m)->mmap_lock)
#define mmap_write_unlock(m) up_write(&(m)->mmap_lock)
#define mmap_write_downgrade(m) downgrade_write(&(m)->mmap_lock)
#define mmap_assert_locked(m) do { } while (0)
#define mmap_assert_write_locked(m) do { } while (0)
#define mmap_lock_is_contended(m) false
void mmput(struct mm_struct *);
#define mmput_async mmput
bool mmget_not_zero(struct mm_struct *);
void mmgrab(struct mm_struct *);
void mmdrop(struct mm_struct *);
static inline void mmget(struct mm_struct *m) { atomic_inc(&m->mm_users); }
struct mm_struct *get_task_mm(struct task_struct *);
#define use_mm(m) do { (void)(m); } while (0)
#define unuse_mm(m) do { (void)(m); } while (0)
#define kthread_use_mm(m) do { (void)(m); } while (0)
#define kthread_unuse_mm(m) do { (void)(m); } while (0)
#define FOLL_WRITE 0x01
#define FOLL_FORCE 0x10
#define FOLL_LONGTERM 0x100
#define FOLL_NOWAIT 0x20
#define FOLL_REMOTE 0x2000
long get_user_pages(unsigned long start, unsigned long n, unsigned int gup, struct page **, ...);
long get_user_pages_remote(struct mm_struct *, unsigned long start, unsigned long n, unsigned int gup, struct page **, ...);
long pin_user_pages(unsigned long start, unsigned long n, unsigned int gup, struct page **, ...);
long pin_user_pages_remote(struct mm_struct *, unsigned long start, unsigned long n, unsigned int gup, struct page **, ...);
void unpin_user_page(struct page *);
void unpin_user_pages(struct page **, unsigned long);
vm_fault_t handle_mm_fault(struct vm_area_struct *, unsigned long addr, unsigned int flags, void *regs);
struct mmu_notifier;
int __mmu_notifier_register(struct mmu_notifier *, struct mm_struct *);
struct mem_cgroup;
static inline struct mem_cgroup *get_mem_cgroup_from_mm(struct mm_struct *m) { (void)m; return NULL; }
static inline void mem_cgroup_put(struct mem_cgroup *c) { (void)c; }
static inline struct mem_cgroup *set_active_memcg(struct mem_cgroup *c) { (void)c; return NULL; }

/* ---- Files. ---- */
struct inode { dev_t i_rdev; void *i_private; unsigned long i_ino; struct address_space *i_mapping; };
struct address_space_operations;
struct address_space { struct inode *host; const struct address_space_operations *a_ops; void *kpi_window_set; };
struct file_operations;
struct file {
    struct inode *f_inode;
    struct address_space *f_mapping;
    void *private_data;
    const struct file_operations *f_op;
    fmode_t f_mode;
    unsigned int f_flags;
    atomic_long_t f_count;
    void *kpi_identity;  /* the forwarding core's identity for it */
};
struct poll_table_struct;
typedef struct poll_table_struct poll_table;
typedef unsigned int __poll_t;
struct file_operations {
    struct module *owner;
    int (*open)(struct inode *, struct file *);
    int (*release)(struct inode *, struct file *);
    long (*unlocked_ioctl)(struct file *, unsigned int, unsigned long);
    long (*compat_ioctl)(struct file *, unsigned int, unsigned long);
    int (*mmap)(struct file *, struct vm_area_struct *);
    __poll_t (*poll)(struct file *, poll_table *);
    ssize_t (*read)(struct file *, char __user *, size_t, loff_t *);
    ssize_t (*write)(struct file *, const char __user *, size_t, loff_t *);
    loff_t (*llseek)(struct file *, loff_t, int);
    unsigned long (*get_unmapped_area)(struct file *, unsigned long, unsigned long, unsigned long, unsigned long);
};
static inline struct inode *file_inode(const struct file *f) { return f->f_inode; }
struct file *fget(unsigned int fd);
void fput(struct file *);
void address_space_init_once(struct address_space *);
/* K1's window_revoke, over every mapping of the file's windows. */
void unmap_mapping_range(struct address_space *, long long start, long long len, int even_cows);
#define POLLIN 0x0001
#define POLLPRI 0x0002
#define POLLOUT 0x0004
#define POLLERR 0x0008
#define POLLHUP 0x0010
#define POLLRDNORM 0x0040
#define EPOLLIN POLLIN
#define EPOLLRDNORM POLLRDNORM
void poll_wait(struct file *, wait_queue_head_t *, poll_table *);
#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_NONBLOCK 04000
#define O_CLOEXEC 02000000
#define FMODE_READ 0x1
#define FMODE_WRITE 0x2
#define S_IRUSR 0400
#define S_IWUSR 0200
#define S_IXUSR 0100
#define S_IRGRP 0040
#define S_IWGRP 0020
#define S_IROTH 0004
#define S_IXGRP 0010
#define S_IXOTH 0001
#define S_IXUGO (S_IXUSR | S_IXGRP | S_IXOTH)
#define S_IWOTH 0002
#define S_IRUGO (S_IRUSR | S_IRGRP | S_IROTH)
#define S_IWUGO (S_IWUSR | S_IWGRP | S_IWOTH)
#define S_IFDIR 0040000
#define S_IFREG 0100000
#define S_IFCHR 0020000

/* ---- User memory: request_copy_in/_out of the waiting client. ---- */
unsigned long copy_from_user(void *to, const void __user *from, unsigned long n);
unsigned long copy_to_user(void __user *to, const void *from, unsigned long n);
#define __copy_from_user copy_from_user
#define __copy_to_user copy_to_user
#define get_user(x, p) ({ __typeof__(*(p)) _v; int _r = copy_from_user(&_v, (p), sizeof(_v)) ? -EFAULT : 0; (x) = _v; _r; })
#define put_user(x, p) ({ __typeof__(*(p)) _v = (x); copy_to_user((p), &_v, sizeof(_v)) ? -EFAULT : 0; })
#define access_ok(p, n) true
static inline unsigned long clear_user(void __user *to, unsigned long n)
{ static const char z[256]; unsigned long left = n; while (left) { unsigned long c = min(left, sizeof(z)); if (copy_to_user((char *)to + (n - left), z, c)) return left; left -= c; } return 0; }

/* ---- DMA: pool pages are pinned into the device's domain at allocation,
 *      and that domain is identity-mapped (src/kernel/src/iommu.rs), so a
 *      page's DMA address is its physical address. ---- */
struct device { void *kpi_device; struct device *parent; };
enum dma_data_direction { DMA_BIDIRECTIONAL = 0, DMA_TO_DEVICE = 1, DMA_FROM_DEVICE = 2, DMA_NONE = 3 };
#define DMA_ATTR_SKIP_CPU_SYNC (1UL << 5)
#define DMA_MAPPING_ERROR (~(dma_addr_t)0)
#define DMA_BIT_MASK(n) (((n) == 64) ? ~0ULL : ((1ULL << (n)) - 1))
dma_addr_t dma_map_page_attrs(struct device *, struct page *, size_t offset, size_t size, enum dma_data_direction, unsigned long attrs);
#define dma_map_page(d, p, o, s, dir) dma_map_page_attrs(d, p, o, s, dir, 0)
void dma_unmap_page_attrs(struct device *, dma_addr_t, size_t, enum dma_data_direction, unsigned long attrs);
#define dma_unmap_page(d, a, s, dir) dma_unmap_page_attrs(d, a, s, dir, 0)
static inline int dma_mapping_error(struct device *d, dma_addr_t a) { (void)d; return a == DMA_MAPPING_ERROR; }
void *dma_alloc_coherent(struct device *, size_t, dma_addr_t *, gfp_t);
void dma_free_coherent(struct device *, size_t, void *, dma_addr_t);
#define dma_alloc_attrs(d, s, h, f, a) dma_alloc_coherent(d, s, h, f)
#define dma_free_attrs(d, s, c, h, a) dma_free_coherent(d, s, c, h)
static inline u64 dma_get_mask(struct device *d) { (void)d; return DMA_BIT_MASK(64); }
static inline u64 dma_to_phys(struct device *d, dma_addr_t a) { (void)d; return a; }
static inline int dma_set_mask(struct device *d, u64 m) { (void)d; (void)m; return 0; }
struct scatterlist { unsigned long page_link; unsigned int offset; unsigned int length; dma_addr_t dma_address; unsigned int dma_length; };
struct sg_table { struct scatterlist *sgl; unsigned int nents; unsigned int orig_nents; };
#define sg_dma_address(sg) ((sg)->dma_address)
#define sg_dma_len(sg) ((sg)->dma_length)
static inline struct page *sg_page(struct scatterlist *s) { return (struct page *)(s->page_link & ~3UL); }
static inline struct scatterlist *sg_next(struct scatterlist *s) { return s + 1; }
#define for_each_sg(sgl, sg, n, i) for ((i) = 0, (sg) = (sgl); (i) < (n); (i)++, (sg) = sg_next(sg))
int sg_alloc_table(struct sg_table *, unsigned int nents, gfp_t);
int sg_alloc_table_from_pages(struct sg_table *, struct page **, unsigned int n, unsigned int offset, unsigned long size, gfp_t);
void sg_free_table(struct sg_table *);
static inline void sg_set_page(struct scatterlist *s, struct page *p, unsigned int len, unsigned int off) { s->page_link = (unsigned long)p; s->length = len; s->offset = off; }
int dma_map_sg_attrs(struct device *, struct scatterlist *, int nents, enum dma_data_direction, unsigned long attrs);
#define dma_map_sg(d, s, n, dir) dma_map_sg_attrs(d, s, n, dir, 0)
void dma_unmap_sg_attrs(struct device *, struct scatterlist *, int nents, enum dma_data_direction, unsigned long attrs);
#define dma_unmap_sg(d, s, n, dir) dma_unmap_sg_attrs(d, s, n, dir, 0)
static inline int dma_map_sgtable(struct device *d, struct sg_table *t, enum dma_data_direction dir, unsigned long a)
{ int n = dma_map_sg_attrs(d, t->sgl, t->orig_nents, dir, a); if (n <= 0) return -EINVAL; t->nents = n; return 0; }
static inline void dma_unmap_sgtable(struct device *d, struct sg_table *t, enum dma_data_direction dir, unsigned long a)
{ dma_unmap_sg_attrs(d, t->sgl, t->orig_nents, dir, a); }
struct sg_dma_page_iter { struct scatterlist *sg; unsigned int nents; unsigned long page_off; };
#define sg_page_iter_dma_address(it) (sg_dma_address((it)->sg) + ((it)->page_off << PAGE_SHIFT))
bool kpi_sg_dma_page_next(struct sg_dma_page_iter *);
#define for_each_sgtable_dma_page(t, it, pg)                                     \
    for ((it)->sg = (t)->sgl, (it)->nents = (t)->nents, (it)->page_off = (unsigned long)-1; \
         kpi_sg_dma_page_next(it);)
#define dma_sync_single_for_cpu(d, a, s, dir) do { } while (0)
#define dma_sync_single_for_device(d, a, s, dir) do { } while (0)

/* ---- Device memory windows (BARs), mapped by nvrm. ---- */
void __iomem *ioremap(phys_addr_t, size_t);
#define ioremap_cache ioremap
#define ioremap_wc ioremap
#define ioremap_nocache ioremap
void iounmap(volatile void __iomem *);
static inline u8 readb(const volatile void __iomem *a) { return *(const volatile u8 *)a; }
static inline u16 readw(const volatile void __iomem *a) { return *(const volatile u16 *)a; }
static inline u32 readl(const volatile void __iomem *a) { return *(const volatile u32 *)a; }
static inline u64 readq(const volatile void __iomem *a) { return *(const volatile u64 *)a; }
static inline void writeb(u8 v, volatile void __iomem *a) { *(volatile u8 *)a = v; }
static inline void writew(u16 v, volatile void __iomem *a) { *(volatile u16 *)a = v; }
static inline void writel(u32 v, volatile void __iomem *a) { *(volatile u32 *)a = v; }
static inline void writeq(u64 v, volatile void __iomem *a) { *(volatile u64 *)a = v; }
#define ioread32 readl
#define iowrite32 writel
#define readl_relaxed readl
#define writel_relaxed writel
struct resource { resource_size_t start, end; const char *name; unsigned long flags; };
extern struct resource iomem_resource;

#endif

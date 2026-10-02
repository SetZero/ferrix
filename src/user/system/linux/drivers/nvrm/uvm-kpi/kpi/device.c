/*
 * uvm-kpi: UVM's device files, procfs, files and the client's memory.
 *
 * In nvrm these go to the forwarding core (docs/NVIDIA.md §4.4): a
 * registered character device becomes a node the core serves, a copy
 * from or to user memory is request_copy_in/_out of the client waiting in
 * the request, and fget is request_file. In the self-test build the
 * "client" is this process, so a user pointer is an ordinary pointer and
 * the device files are opened by the test host directly (kpi_open).
 *
 * What only K1 can do -- putting pages into a client and taking them out
 * again (vm_insert_page, unmap_mapping_range) -- answers -ENOSYS until
 * C3 (§11.5). Pageable-memory access (get_user_pages and friends, HMM)
 * is off in the Ferrix profile and answers as a kernel without it would.
 *
 * SPDX-License-Identifier: MIT
 */
#include "libc.h"

/* ---- Character devices. ---- */
#define KPI_MAX_CDEVS 8
static struct cdev *cdevs[KPI_MAX_CDEVS];
static int cdev_lock;
static unsigned int next_major = 240; /* Linux's first "local use" major */

void cdev_init(struct cdev *c, const struct file_operations *ops)
{
    memset(c, 0, sizeof(*c));
    c->ops = ops;
}

int cdev_add(struct cdev *c, dev_t dev, unsigned int count)
{
    int i, r = -ENOSPC;

    c->dev = dev;
    c->count = count;
    kpi_spin_lock(&cdev_lock);
    for (i = 0; i < KPI_MAX_CDEVS; i++) {
        if (!cdevs[i]) {
            cdevs[i] = c;
            r = 0;
            break;
        }
    }
    kpi_spin_unlock(&cdev_lock);
    return r;
}

void cdev_del(struct cdev *c)
{
    int i;

    kpi_spin_lock(&cdev_lock);
    for (i = 0; i < KPI_MAX_CDEVS; i++)
        if (cdevs[i] == c)
            cdevs[i] = NULL;
    kpi_spin_unlock(&cdev_lock);
}

int alloc_chrdev_region(dev_t *dev, unsigned int baseminor, unsigned int count, const char *name)
{
    (void)count;
    (void)name;
    *dev = MKDEV(__atomic_fetch_add(&next_major, 1, __ATOMIC_RELAXED), baseminor);
    return 0;
}

int register_chrdev_region(dev_t dev, unsigned int count, const char *name)
{
    (void)dev;
    (void)count;
    (void)name;
    return 0;
}

void unregister_chrdev_region(dev_t dev, unsigned int count)
{
    (void)dev;
    (void)count;
}

/* Open the device with this number as a client would: a new file and
 * inode, then the driver's open. Returns the file, or an ERR_PTR. */
struct file *kpi_open(dev_t dev)
{
    const struct file_operations *ops = NULL;
    struct file *f;
    struct inode *ino;
    struct address_space *mapping;
    int i, r;

    kpi_spin_lock(&cdev_lock);
    for (i = 0; i < KPI_MAX_CDEVS; i++) {
        struct cdev *c = cdevs[i];
        if (c && dev >= c->dev && dev < c->dev + c->count)
            ops = c->ops;
    }
    kpi_spin_unlock(&cdev_lock);
    if (!ops)
        return ERR_PTR(-ENODEV);
    f = kzalloc(sizeof(*f), GFP_KERNEL);
    ino = kzalloc(sizeof(*ino) + sizeof(*mapping), GFP_KERNEL);
    if (!f || !ino) {
        kfree(f);
        kfree(ino);
        return ERR_PTR(-ENOMEM);
    }
    /* The inode's own mapping lives in the same block. */
    mapping = (struct address_space *)(ino + 1);
    mapping->host = ino;
    ino->i_mapping = mapping;
    ino->i_rdev = dev;
    f->f_inode = ino;
    f->f_mapping = mapping;
    f->f_op = ops;
    f->f_mode = FMODE_READ | FMODE_WRITE;
    atomic_long_set(&f->f_count, 1);
    r = ops->open ? ops->open(ino, f) : 0;
    if (r) {
        kfree(f);
        kfree(ino);
        return ERR_PTR(r);
    }
    return f;
}

long kpi_ioctl(struct file *f, unsigned int cmd, unsigned long arg)
{
    return f->f_op->unlocked_ioctl(f, cmd, arg);
}

void kpi_close(struct file *f)
{
    fput(f);
}

/* ---- Files by descriptor: the self-test host has no descriptors to
 *      resolve; nvrm answers through request_file. ---- */
struct file *fget(unsigned int fd)
{
    (void)fd;
    return NULL;
}

void fput(struct file *f)
{
    if (!f)
        return;
    if (atomic_long_dec_and_test(&f->f_count)) {
        if (f->f_op->release)
            f->f_op->release(f->f_inode, f);
        kfree(f->f_inode);
        kfree(f);
    }
}

void address_space_init_once(struct address_space *m)
{
    memset(m, 0, sizeof(*m));
}

void poll_wait(struct file *f, wait_queue_head_t *q, poll_table *p)
{
    (void)f;
    (void)q;
    (void)p;
}

/* ---- procfs: entries are recorded, and nvrm's procfs hook (not in the
 *      self-test build) reads them. ---- */
struct proc_dir_entry {
    const char *name;
    const struct proc_ops *ops;
    void *data;
};

static struct proc_dir_entry *proc_entry(const char *name, const struct proc_ops *ops, void *data)
{
    struct proc_dir_entry *e = kzalloc(sizeof(*e), GFP_KERNEL);
    if (e) {
        e->name = name;
        e->ops = ops;
        e->data = data;
    }
    return e;
}

struct proc_dir_entry *proc_mkdir(const char *name, struct proc_dir_entry *parent)
{
    (void)parent;
    return proc_entry(name, NULL, NULL);
}

struct proc_dir_entry *proc_mkdir_mode(const char *name, umode_t mode, struct proc_dir_entry *parent)
{
    (void)mode;
    return proc_mkdir(name, parent);
}

struct proc_dir_entry *proc_create_data(const char *name, umode_t mode, struct proc_dir_entry *parent,
                                        const struct proc_ops *ops, void *data)
{
    (void)mode;
    (void)parent;
    return proc_entry(name, ops, data);
}

struct proc_dir_entry *proc_symlink(const char *name, struct proc_dir_entry *parent, const char *dest)
{
    (void)parent;
    (void)dest;
    return proc_entry(name, NULL, NULL);
}

void proc_remove(struct proc_dir_entry *e)
{
    kfree(e);
}

void remove_proc_entry(const char *name, struct proc_dir_entry *parent)
{
    (void)name;
    (void)parent;
}

void *pde_data(const struct inode *ino)
{
    return ino->i_private;
}

int single_open(struct file *f, int (*show)(struct seq_file *, void *), void *data)
{
    struct seq_file *m = kzalloc(sizeof(*m), GFP_KERNEL);
    if (!m)
        return -ENOMEM;
    m->private = data;
    f->private_data = m;
    (void)show;
    return 0;
}

int single_release(struct inode *ino, struct file *f)
{
    struct seq_file *m = f->private_data;
    (void)ino;
    if (m)
        kfree(m->kpi_buf);
    kfree(m);
    return 0;
}

ssize_t seq_read(struct file *f, char __user *buf, size_t n, loff_t *pos)
{
    (void)f;
    (void)buf;
    (void)n;
    (void)pos;
    return 0;
}

loff_t seq_lseek(struct file *f, loff_t off, int whence)
{
    (void)f;
    (void)whence;
    return off;
}

void seq_printf(struct seq_file *m, const char *fmt, ...)
{
    char line[512];
    va_list ap;
    int n;

    va_start(ap, fmt);
    n = vsnprintf(line, sizeof(line), fmt, ap);
    va_end(ap);
    if (n <= 0)
        return;
    if ((size_t)n >= sizeof(line))
        n = sizeof(line) - 1;
    if (m->kpi_len + (size_t)n + 1 > m->kpi_size) {
        size_t size = max_t(size_t, 4096, 2 * (m->kpi_len + (size_t)n + 1));
        char *b = krealloc(m->kpi_buf, size, GFP_KERNEL);
        if (!b)
            return;
        m->kpi_buf = b;
        m->kpi_size = size;
    }
    memcpy(m->kpi_buf + m->kpi_len, line, (size_t)n);
    m->kpi_len += (size_t)n;
    m->kpi_buf[m->kpi_len] = '\0';
}

void seq_puts(struct seq_file *m, const char *s)
{
    seq_printf(m, "%s", s);
}

/* ---- The client's memory. ---- */
unsigned long copy_from_user(void *to, const void __user *from, unsigned long n)
{
    memcpy(to, from, n);
    return 0;
}

unsigned long copy_to_user(void __user *to, const void *from, unsigned long n)
{
    memcpy(to, from, n);
    return 0;
}

struct vm_area_struct *find_vma(struct mm_struct *mm, unsigned long addr)
{
    (void)mm;
    (void)addr;
    return NULL;
}

struct vm_area_struct *find_vma_intersection(struct mm_struct *mm, unsigned long start, unsigned long end)
{
    (void)mm;
    (void)start;
    (void)end;
    return NULL;
}

pgprot_t vm_get_page_prot(unsigned long vm_flags)
{
    unsigned long p = _PAGE_PRESENT | _PAGE_USER;
    if (vm_flags & VM_WRITE)
        p |= _PAGE_RW;
    if (!(vm_flags & VM_EXEC))
        p |= _PAGE_NX;
    return __pgprot(p);
}

int vm_insert_page(struct vm_area_struct *vma, unsigned long addr, struct page *page)
{
    (void)vma;
    (void)addr;
    (void)page;
    return -ENOSYS; /* K1's window_insert, C3 */
}

void unmap_mapping_range(struct address_space *m, long long start, long long len, int even_cows)
{
    (void)m;
    (void)start;
    (void)len;
    (void)even_cows;
    /* K1's window_revoke, C3. With no window there is nothing mapped. */
}

int remap_pfn_range(struct vm_area_struct *vma, unsigned long addr, unsigned long pfn,
                    unsigned long size, pgprot_t prot)
{
    (void)vma;
    (void)addr;
    (void)pfn;
    (void)size;
    (void)prot;
    return -ENOSYS;
}

long get_user_pages_remote(struct mm_struct *mm, unsigned long start, unsigned long n,
                           unsigned int gup, struct page **pages, ...)
{
    (void)mm;
    (void)start;
    (void)n;
    (void)gup;
    (void)pages;
    return -EFAULT;
}

long pin_user_pages(unsigned long start, unsigned long n, unsigned int gup, struct page **pages, ...)
{
    (void)start;
    (void)n;
    (void)gup;
    (void)pages;
    return -EFAULT;
}

long pin_user_pages_remote(struct mm_struct *mm, unsigned long start, unsigned long n,
                           unsigned int gup, struct page **pages, ...)
{
    (void)mm;
    return pin_user_pages(start, n, gup, pages);
}

void unpin_user_page(struct page *p)
{
    (void)p;
}

vm_fault_t handle_mm_fault(struct vm_area_struct *vma, unsigned long addr, unsigned int flags, void *regs)
{
    (void)vma;
    (void)addr;
    (void)flags;
    (void)regs;
    return VM_FAULT_SIGBUS;
}

int __mmu_notifier_register(struct mmu_notifier *n, struct mm_struct *mm)
{
    (void)n;
    (void)mm;
    return -ENOSYS;
}

void mmput(struct mm_struct *mm)
{
    if (mm)
        atomic_dec(&mm->mm_users);
}

bool mmget_not_zero(struct mm_struct *mm)
{
    return mm && atomic_inc_not_zero(&mm->mm_users);
}

void mmgrab(struct mm_struct *mm)
{
    if (mm)
        atomic_inc(&mm->mm_count);
}

void mmdrop(struct mm_struct *mm)
{
    if (mm)
        atomic_dec(&mm->mm_count);
}

/* ---- PCI: the card's BARs are RM's; UVM only asks where BAR1 is. ---- */
resource_size_t pci_resource_start(struct pci_dev *dev, int bar)
{
    (void)dev;
    (void)bar;
    return 0;
}

resource_size_t pci_resource_len(struct pci_dev *dev, int bar)
{
    (void)dev;
    (void)bar;
    return 0;
}

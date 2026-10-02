/*
 * uvm-kpi: device files, procfs, randomness, PCI identity and the rest.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef FERRIX_KPI_MISC_H
#define FERRIX_KPI_MISC_H

/* ---- Character devices: registered with the forwarding core. ---- */
#define MINORBITS 20
#define MINORMASK ((1U << MINORBITS) - 1)
#define MAJOR(d) ((unsigned int)((d) >> MINORBITS))
#define MINOR(d) ((unsigned int)((d) & MINORMASK))
#define MKDEV(ma, mi) (((ma) << MINORBITS) | (mi))
struct cdev { const struct file_operations *ops; dev_t dev; unsigned int count; struct module *owner; };
void cdev_init(struct cdev *, const struct file_operations *);
int cdev_add(struct cdev *, dev_t, unsigned int);
void cdev_del(struct cdev *);
int alloc_chrdev_region(dev_t *, unsigned int baseminor, unsigned int count, const char *name);
int register_chrdev_region(dev_t, unsigned int, const char *);
void unregister_chrdev_region(dev_t, unsigned int);
#define iminor(i) MINOR((i)->i_rdev)
#define imajor(i) MAJOR((i)->i_rdev)
static inline loff_t noop_llseek(struct file *f, loff_t o, int w) { (void)f; (void)o; (void)w; return 0; }
static inline int nonseekable_open(struct inode *i, struct file *f) { (void)i; (void)f; return 0; }

/* ---- procfs: answered through nvrm's procfs hook. ---- */
struct seq_file { void *private; char *kpi_buf; size_t kpi_size, kpi_len; };
struct proc_dir_entry;
struct proc_ops {
    int (*proc_open)(struct inode *, struct file *);
    ssize_t (*proc_read)(struct file *, char __user *, size_t, loff_t *);
    ssize_t (*proc_write)(struct file *, const char __user *, size_t, loff_t *);
    loff_t (*proc_lseek)(struct file *, loff_t, int);
    int (*proc_release)(struct inode *, struct file *);
    unsigned int proc_flags;
};
#define PROC_ENTRY_PERMANENT 1U
struct proc_dir_entry *proc_mkdir(const char *, struct proc_dir_entry *);
struct proc_dir_entry *proc_mkdir_mode(const char *, umode_t, struct proc_dir_entry *);
struct proc_dir_entry *proc_create_data(const char *, umode_t, struct proc_dir_entry *, const struct proc_ops *, void *);
struct proc_dir_entry *proc_symlink(const char *, struct proc_dir_entry *, const char *);
void proc_remove(struct proc_dir_entry *);
void remove_proc_entry(const char *, struct proc_dir_entry *);
void *pde_data(const struct inode *);
#define PDE_DATA pde_data
int single_open(struct file *, int (*)(struct seq_file *, void *), void *);
int single_release(struct inode *, struct file *);
ssize_t seq_read(struct file *, char __user *, size_t, loff_t *);
loff_t seq_lseek(struct file *, loff_t, int);
void seq_printf(struct seq_file *, const char *, ...) __printf(2, 3);
void seq_puts(struct seq_file *, const char *);
#define seq_putc(m, c) seq_printf(m, "%c", c)

/* ---- Randomness and identity. ---- */
void get_random_bytes(void *, size_t);
u32 get_random_u32(void);
u64 get_random_u64(void);
#define prandom_u32() get_random_u32()
static inline kuid_t current_euid(void) { kuid_t k = { 0 }; return k; }
#define current_uid current_euid
#define uid_eq(a, b) ((a).val == (b).val)
#define GLOBAL_ROOT_UID ((kuid_t){ 0 })
static inline bool capable(int cap) { (void)cap; return true; }
#define CAP_SYS_ADMIN 21
#define CAP_SYS_NICE 23
#define CAP_IPC_LOCK 14

/* ---- ioctl numbers. ---- */
#define _IOC_NRBITS 8
#define _IOC_TYPEBITS 8
#define _IOC_SIZEBITS 14
#define _IOC_NRSHIFT 0
#define _IOC_TYPESHIFT (_IOC_NRSHIFT + _IOC_NRBITS)
#define _IOC_SIZESHIFT (_IOC_TYPESHIFT + _IOC_TYPEBITS)
#define _IOC_DIRSHIFT (_IOC_SIZESHIFT + _IOC_SIZEBITS)
#define _IOC_NONE 0U
#define _IOC_WRITE 1U
#define _IOC_READ 2U
#define _IOC(d, t, n, s) (((d) << _IOC_DIRSHIFT) | ((t) << _IOC_TYPESHIFT) | ((n) << _IOC_NRSHIFT) | ((s) << _IOC_SIZESHIFT))
#define _IO(t, n) _IOC(_IOC_NONE, (t), (n), 0)
#define _IOR(t, n, s) _IOC(_IOC_READ, (t), (n), sizeof(s))
#define _IOW(t, n, s) _IOC(_IOC_WRITE, (t), (n), sizeof(s))
#define _IOWR(t, n, s) _IOC(_IOC_READ | _IOC_WRITE, (t), (n), sizeof(s))
#define _IOC_NR(n) (((n) >> _IOC_NRSHIFT) & ((1 << _IOC_NRBITS) - 1))
#define _IOC_TYPE(n) (((n) >> _IOC_TYPESHIFT) & ((1 << _IOC_TYPEBITS) - 1))
#define _IOC_SIZE(n) (((n) >> _IOC_SIZESHIFT) & ((1 << _IOC_SIZEBITS) - 1))
#define _IOC_DIR(n) (((n) >> _IOC_DIRSHIFT) & 3)

/* ---- PCI: only identity. The device itself is RM's. ---- */
struct pci_dev { struct device dev; unsigned int devfn; unsigned short vendor, device; void *bus; };
#define to_pci_dev(d) container_of(d, struct pci_dev, dev)
resource_size_t pci_resource_start(struct pci_dev *, int bar);
resource_size_t pci_resource_len(struct pci_dev *, int bar);
#define PCI_SLOT(d) (((d) >> 3) & 0x1f)
#define PCI_FUNC(d) ((d) & 0x07)

/* ---- Reference counts. ---- */
struct kref { atomic_t refcount; };
static inline void kref_init(struct kref *k) { atomic_set(&k->refcount, 1); }
static inline void kref_get(struct kref *k) { atomic_inc(&k->refcount); }
static inline int kref_put(struct kref *k, void (*release)(struct kref *))
{ if (atomic_dec_and_test(&k->refcount)) { release(k); return 1; } return 0; }
static inline unsigned int kref_read(const struct kref *k) { return atomic_read(&k->refcount); }

/* ---- Notifiers and the rest that UVM names but never relies on. ---- */
struct notifier_block { int (*notifier_call)(struct notifier_block *, unsigned long, void *); struct notifier_block *next; int priority; };
#define NOTIFY_DONE 0
#define NOTIFY_OK 1
#define IRQ_NONE 0
#define IRQ_HANDLED 1
#define IRQ_WAKE_THREAD 2

#endif

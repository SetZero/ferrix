/* uvm-kpi: what Ferrix's Linux-compatible layer provides (see ferrix_kpi.h).
 * Each NV_*_PRESENT below answers one of NVIDIA's conftests for uvm-kpi,
 * as conftest.sh would for a kernel that has the interface. */
#define NV_KTIME_GET_RAW_TS64_PRESENT
#define NV_LIST_IS_FIRST_PRESENT
#define NV_VMF_INSERT_PFN_PROT_PRESENT
#define NV_VM_FLAGS_SET_PRESENT
#define NV_PIN_USER_PAGES_PRESENT
#define NV_PIN_USER_PAGES_REMOTE_PRESENT
#define NV_IOREMAP_CACHE_PRESENT
#define NV_IOREMAP_WC_PRESENT
#define NV_SET_MEMORY_UC_PRESENT
#define NV_FOR_EACH_SGTABLE_DMA_PAGE_PRESENT
#define NV_PAGE_PGMAP_PRESENT
#define NV_PROC_OPS_PRESENT
#define NV_PDE_DATA_LOWER_CASE_PRESENT
#define NV_HANDLE_MM_FAULT_HAS_PT_REGS_ARG

# Builds NVIDIA's nvidia-uvm from a fetched open-gpu-kernel-modules tree
# against uvm-kpi, Ferrix's Linux-compatible header set, as user-space
# objects for nvrm (docs/NVIDIA.md §11.3).
#
#   make NVSRC=<tree> OUT=<dir> [CC=gcc]
#
# NVSRC is the open-gpu-kernel-modules checkout at 580.173.02; OUT is a
# build directory outside the checkout.

NVSRC ?= $(HOME)/.local/share/ferrix/nvidia-ref/ogkm-580.173.02
OUT   ?= $(HOME)/.local/share/ferrix/target-nvidia-cuda/uvm-kpi
CC    ?= gcc
KPI   := $(CURDIR)
UVM   := $(NVSRC)/kernel-open/nvidia-uvm

# The UVM sources of the Ferrix profile: NVIDIA's list less uvm_common.c
# (GPL-2.0-or-later, replaced by kpi/uvm_common.c). The tests stay in: the
# self-test runner calls them.
UVM_SOURCES := $(filter-out uvm_common.c,$(notdir $(shell sed -n 's|^NVIDIA_UVM_SOURCES += nvidia-uvm/||p' $(UVM)/nvidia-uvm-sources.Kbuild)))
UVM_OBJECTS := $(patsubst %.c,$(OUT)/uvm/%.o,$(UVM_SOURCES))

KPI_SOURCES := $(wildcard $(KPI)/kpi/*.c)
KPI_OBJECTS := $(patsubst $(KPI)/kpi/%.c,$(OUT)/kpi/%.o,$(KPI_SOURCES))

# UVM's translation units see only uvm-kpi and the compiler's freestanding
# headers, never the C library's: a libc header would pull in the
# Linux UAPI headers that uvm-kpi's names shadow.
UVM_CFLAGS := -std=gnu11 -O2 -g -ffreestanding -nostdinc \
	-isystem $(shell $(CC) -print-file-name=include) \
	-I$(KPI)/include -I$(UVM) -I$(NVSRC)/kernel-open/common/inc -I$(NVSRC)/kernel-open \
	-include $(KPI)/include/ferrix_kpi.h \
	-D__KERNEL__ -DMODULE -DNVRM -DNV_VERSION_STRING=\"580.173.02\" \
	-DNVIDIA_UVM_ENABLED -DNVIDIA_UNDEF_LEGACY_BIT_MACROS -DLinux -D__linux__ \
	-DNV_UVM_ENABLE -DNV_KERNEL_INTERFACE_LAYER -DNV_SPECTRE_V2=0 \
	-Wall -Wno-unused-function -Wno-cast-qual -Wno-format-extra-args \
	-fno-strict-aliasing -fno-common -pthread
# The kpi implementation is compiled the same way, so that it sees the
# same kernel types; the few C library calls it makes are declared in
# kpi/libc.h.
KPI_CFLAGS := $(UVM_CFLAGS) -I$(KPI)/kpi

all: $(OUT)/libuvm.a $(OUT)/libuvmkpi.a $(OUT)/uvm-selftest

$(OUT)/uvm/%.o: $(UVM)/%.c $(wildcard $(KPI)/include/*.h $(KPI)/include/*/*.h)
	@mkdir -p $(dir $@)
	$(CC) $(UVM_CFLAGS) -c $< -o $@

$(OUT)/uvm/uvm_common.o: $(KPI)/kpi/uvm/uvm_common.c
	@mkdir -p $(dir $@)
	$(CC) $(UVM_CFLAGS) -c $< -o $@

$(OUT)/kpi/%.o: $(KPI)/kpi/%.c $(wildcard $(KPI)/kpi/*.h)
	@mkdir -p $(dir $@)
	$(CC) $(KPI_CFLAGS) -c $< -o $@

$(OUT)/libuvm.a: $(UVM_OBJECTS) $(OUT)/uvm/uvm_common.o
	rm -f $@ && ar rcs $@ $^

$(OUT)/libuvmkpi.a: $(KPI_OBJECTS)
	rm -f $@ && ar rcs $@ $^

# uvm-selftest: UVM's built-in tests with no GPU (selftest/main.c).
SELFTEST_OBJECTS := $(OUT)/selftest/main.o $(OUT)/selftest/rm_stub.o

$(OUT)/selftest/%.o: $(KPI)/selftest/%.c $(wildcard $(KPI)/include/*.h $(KPI)/include/*/*.h)
	@mkdir -p $(dir $@)
	$(CC) $(UVM_CFLAGS) -c $< -o $@

$(OUT)/uvm-selftest: $(SELFTEST_OBJECTS) $(OUT)/libuvm.a $(OUT)/libuvmkpi.a
	$(CC) -pthread -o $@ $(SELFTEST_OBJECTS) $(OUT)/libuvm.a $(OUT)/libuvmkpi.a $(OUT)/libuvm.a

test: $(OUT)/uvm-selftest
	$(OUT)/uvm-selftest

clean:
	rm -rf $(OUT)

.PHONY: all clean test

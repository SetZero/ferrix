# Builds nvrm, the ring-3 host of NVIDIA's resource manager
# (docs/NVIDIA.md §4.1), statically against ferrousli, Ferrix's own C
# library, for x86-64.
#
#   make OUT=<dir> [CC=gcc] [FERROUSLI_TARGET=<dir>]
#
# N1b's skeleton: no NVIDIA object is linked yet. `nvrm` is started by
# devmgr as a driver (src/start.c is its native entry); `nvrm-hold` is the
# init `cargo xtask test-nvrm` boots beside it, to keep the machine up while
# nvrm runs. Both link against crt-less ferrousli and libgcc, and nothing
# from the host's C library.

OUT   ?= $(HOME)/.local/share/ferrix/target-nvidia/nvrm
CC    ?= gcc
NVRM  := $(CURDIR)

FERROUSLI        := $(abspath $(NVRM)/../../ferrousli)
FERROUSLI_TARGET ?= $(OUT)/ferrousli
FERROUSLI_LIB    := $(FERROUSLI_TARGET)/release/libferrousli.a

# Against ferrousli's headers alone, never the host's, with the stack
# protector on as a distribution's compiler leaves it.
CFLAGS := -std=gnu11 -O2 -g -Wall -Wextra -Werror -nostdinc \
	-isystem $(FERROUSLI)/include -fno-pie -fstack-protector-strong
# The entry runs before ferrousli has set the thread pointer, where the
# canary lives.
START_CFLAGS := $(filter-out -fstack-protector-strong,$(CFLAGS)) -fno-stack-protector

all: $(OUT)/nvrm $(OUT)/nvrm-hold

# cargo decides whether the library is stale, so this always runs; the
# programs below are linked again when it wrote a new one.
$(FERROUSLI_LIB): FORCE
	cd $(FERROUSLI) && cargo build --release --lib --target-dir $(FERROUSLI_TARGET)

$(OUT)/start.o: $(NVRM)/src/start.c
	@mkdir -p $(OUT)
	$(CC) $(START_CFLAGS) -c $< -o $@

$(OUT)/main.o: $(NVRM)/src/main.c $(NVRM)/src/native.h
	@mkdir -p $(OUT)
	$(CC) $(CFLAGS) -c $< -o $@

$(OUT)/hold.o: $(NVRM)/test/hold.c
	@mkdir -p $(OUT)
	$(CC) $(CFLAGS) -c $< -o $@

# No crt1.o: start.o is the entry.
$(OUT)/nvrm: $(OUT)/start.o $(OUT)/main.o $(FERROUSLI_LIB)
	$(CC) -static -no-pie -nostdlib -Wl,--strip-debug -o $@ $(OUT)/start.o $(OUT)/main.o \
		-Wl,--start-group $(FERROUSLI_LIB) -lgcc -Wl,--end-group

$(OUT)/nvrm-hold: $(OUT)/hold.o $(FERROUSLI_LIB)
	crt1=$$(ls -t $(FERROUSLI_TARGET)/release/build/ferrousli-*/out/crt1.o | head -1) && \
	$(CC) -static -no-pie -nostdlib -Wl,--strip-debug -o $@ $$crt1 $(OUT)/hold.o \
		-Wl,--start-group $(FERROUSLI_LIB) -lgcc -Wl,--end-group

clean:
	rm -rf $(OUT)

.PHONY: all clean FORCE
FORCE:

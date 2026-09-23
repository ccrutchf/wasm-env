# Builds every benchmark natively, as wasm32 and wasm64 modules, and as
# wasm2c + clang binaries from those modules.
#   make            build everything into build/
#   make clean
#
# Run inside `nix develop` (provides CC and WASM_CC).

BENCHES := list tree stream sort hash interp sha256

NATIVE_CC ?= clang
WASM_CC ?= clang
OUT     := build

COMMON_CFLAGS := -O2 -ffreestanding -fno-builtin -Wall -Wextra
WASM_CFLAGS   := $(COMMON_CFLAGS) -nostdlib -msimd128 -Wl,--no-entry

NATIVE := $(BENCHES:%=$(OUT)/native/%)
WASM32 := $(BENCHES:%=$(OUT)/wasm32/%.wasm)
WASM64 := $(BENCHES:%=$(OUT)/wasm64/%.wasm)

all: $(NATIVE) $(WASM32) $(WASM64)

$(OUT)/native/%: c/%.c c/alloc.c c/native_driver.c c/bench.h
	@mkdir -p $(@D)
	$(NATIVE_CC) $(COMMON_CFLAGS) -c c/$*.c -o $@.o
	$(NATIVE_CC) $(COMMON_CFLAGS) -c c/alloc.c -o $@.alloc.o
	$(NATIVE_CC) -O2 -Wall c/native_driver.c $@.o $@.alloc.o -o $@
	@rm -f $@.o $@.alloc.o

$(OUT)/wasm32/%.wasm: c/%.c c/alloc.c c/bench.h
	@mkdir -p $(@D)
	$(WASM_CC) --target=wasm32-unknown-unknown $(WASM_CFLAGS) c/$*.c c/alloc.c -o $@

$(OUT)/wasm64/%.wasm: c/%.c c/alloc.c c/bench.h
	@mkdir -p $(@D)
	$(WASM_CC) --target=wasm64-unknown-unknown $(WASM_CFLAGS) c/$*.c c/alloc.c -o $@

# ---------------------------------------------------------------------------
# wasm2c + clang: the same .wasm files compiled ahead of time by LLVM instead
# of Cranelift. Output binaries follow the native driver protocol.
#   w2c-wasm32         wasm32, guard pages (wasm2c default)
#   w2c-wasm64         memory64, explicit bounds checks (wasm2c default)
#   w2c-wasm64-masked  memory64, 38-bit masking (w2c/mask.h, w2c/mem_masked.c)

WASM2C         ?= wasm2c
WASM_ENV_MASK_BITS := 38
W2C_CFLAGS     := -O2 -I$(WASM2C_INCLUDE) -w
W2C_VARIANTS   := w2c-wasm32 w2c-wasm64 w2c-wasm64-masked
W2C            := $(foreach v,$(W2C_VARIANTS),$(BENCHES:%=$(OUT)/$(v)/%))

all: $(W2C)

$(OUT)/w2c-src/wasm32/%.c: $(OUT)/wasm32/%.wasm
	@mkdir -p $(@D)
	$(WASM2C) -n $* $< -o $@

$(OUT)/w2c-src/wasm64/%.c: $(OUT)/wasm64/%.wasm
	@mkdir -p $(@D)
	$(WASM2C) --enable-memory64 -n $* $< -o $@

# Insert w2c/mask.h right after the MEMCHECK_GENERAL definition.
$(OUT)/w2c-src/wasm64-masked/%.c: $(OUT)/w2c-src/wasm64/%.c w2c/mask.h
	@mkdir -p $(@D)
	cp $(OUT)/w2c-src/wasm64/$*.h $(@D)/
	test "$$(grep -c '^  RANGE_CHECK(mem, a, sizeof(t));$$' $<)" = 1
	sed '/^  RANGE_CHECK(mem, a, sizeof(t));$$/r w2c/mask.h' $< > $@

$(OUT)/w2c-wasm32/%: $(OUT)/w2c-src/wasm32/%.c w2c/driver.c
	@mkdir -p $(@D)
	$(NATIVE_CC) $(W2C_CFLAGS) -I$(<D) -DMOD=$* -include $(<D)/$*.h w2c/driver.c $< \
	  $(WASM2C_RT)/wasm-rt-impl.c $(WASM2C_RT)/wasm-rt-mem-impl.c -lm -o $@

$(OUT)/w2c-wasm64/%: $(OUT)/w2c-src/wasm64/%.c w2c/driver.c
	@mkdir -p $(@D)
	$(NATIVE_CC) $(W2C_CFLAGS) -I$(<D) -DMOD=$* -include $(<D)/$*.h w2c/driver.c $< \
	  $(WASM2C_RT)/wasm-rt-impl.c $(WASM2C_RT)/wasm-rt-mem-impl.c -lm -o $@

$(OUT)/w2c-wasm64-masked/%: $(OUT)/w2c-src/wasm64-masked/%.c w2c/driver.c w2c/mem_masked.c
	@mkdir -p $(@D)
	$(NATIVE_CC) $(W2C_CFLAGS) -DWASM_ENV_MASK_BITS=$(WASM_ENV_MASK_BITS) -I$(<D) -DMOD=$* \
	  -include $(<D)/$*.h w2c/driver.c $< \
	  $(WASM2C_RT)/wasm-rt-impl.c w2c/mem_masked.c -lm -o $@

.SECONDARY:

clean:
	rm -rf $(OUT)

.PHONY: all clean

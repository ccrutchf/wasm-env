# lavapipe: wrong results from large compute shaders

Two problems found while running OpenCL kernels on lavapipe through
wasm-env's GPU service (OpenCL C → LLVM IR → WGSL → naga → SPIR-V → lavapipe).
Both involve large compute shaders: OpenCL `double` arithmetic emulated on
32-bit integers, with libclc's math functions inlined, which gives shaders
of 80–100 KB of WGSL (130–180 KB of SPIR-V).

| | Bug 1: 256-bit miscompile | Bug 2: equivalent shaders differ |
|---|---|---|
| Status | Reproduced standalone | Seen only inside wasm-env so far |
| Trigger | `LP_NATIVE_VECTOR_WIDTH=256` (the default on AVX2 machines) | One of two semantically identical shaders |
| Workaround | `LP_NATIVE_VECTOR_WIDTH=128` | None in place |

## Environment

- Mesa 26.2.3, llvmpipe with LLVM 21.1.8: `llvmpipe (LLVM 21.1.8, 256 bits)`,
  Vulkan 1.4.354, conformance version 1.3.1.1.
- CPU: 11th Gen Intel Core i7-11800H (AVX2, AVX-512F).
- Linux 7.2.8, NixOS.
- wgpu 30.0.1 and naga 30.0.1 (WGSL → SPIR-V).

## Files here

- `bug1.cl`, `bug2.cl`: the OpenCL C kernels the shaders came from (for
  context; the shaders are the reproducers).
- `bug1.wgsl`, `bug2-lets.wgsl`, `bug2-vars.wgsl`: the shaders as wasm-env
  generates them.
- `*.spv`: the same shaders as SPIR-V, from naga 30 with default options.
  These are not byte for byte what wgpu submits, which uses its own naga
  options.
- `repro/`: a standalone wgpu program that runs a shader with the exact
  inputs, prints every output (value and bits), and for bug 1 checks each
  output against Rust's `f64` arithmetic.

The shaders use no `f64` (doubles are `vec2<u32>` bit patterns), so they need
no optional Vulkan features.

Buffer layout, as the reproducer sets it up:

- **Binding 0:** a read-only `array<u32>` of 16 header words: global offset
  x/y/z, global size x/y/z, dimensions, workgroup offset x/y/z, six unused.
  Then one word per kernel argument: a buffer argument's byte offset within
  its buffer, 0 here.
- **Bindings 1 and up:** the kernel's buffers, in argument order.

## Bug 1: wrong results at 256-bit vector width

`bug1.wgsl` computes double arithmetic, conversions and libclc's `sin`,
`exp` and `pow` for 64 inputs, in one workgroup of 64.

```sh
cd repro
VK_ICD_FILENAMES=/path/to/lvp_icd.x86_64.json \
    LP_NATIVE_VECTOR_WIDTH=128 cargo run --release -- bug1 ../bug1.wgsl > out-128.txt
VK_ICD_FILENAMES=/path/to/lvp_icd.x86_64.json \
    LP_NATIVE_VECTOR_WIDTH=256 cargo run --release -- bug1 ../bug1.wgsl > out-256.txt
```

**Expected:** every output matches Rust's `f64` results, within 4 ulps for
`sin` and `exp` and 16 for `pow` (as libclc's accuracy allows). That's what
128 bits gives: "0 of 1280 outputs wrong".

**Actual at 256 bits:** "1050 of 1280 outputs wrong". The program prints each
on stderr; the first few:

```
wrong: o[0][9] for NaN, -958.4062955714286: got 1.5043946084157472e1, want 2.4060157389285715e2
wrong: o[0][11] for NaN, -958.4062955714286: got -3.0199999999976717e2, want -3e2
wrong: o[0][15] for NaN, -958.4062955714286: got -1.9999999997671694e0, want -9.59e2
wrong: l[0][0] for NaN, -958.4062955714286: got -1, want 0
wrong: l[0][1] for NaN, -958.4062955714286: got -9223372032559808513, want -9584062955714
```

Even plain integer-to-double conversions come out wrong. `o[i][11]` is
`(double)(i * 7 - 300)`, which gives -301.99999999976717 instead of -300.

Also observed:

- **Build time:** the pipeline takes about four times as long to create at 256
  bits (about 14 minutes against 3 for an earlier, similar shader).
- **Shader size:** in an earlier version of the shader, removing any one of its
  roughly twenty statements made the results correct again. That points to a
  size-dependent problem rather than one wrong operation.
- **Not divergence:** the earlier version also gave wrong results when every
  invocation had the same input, and correct results with a workgroup of one.
- **Not the LLVM optimizer:** setting `GALLIVM_PERF=no_opt` did not change the
  wrong results (tried on bug 2's shader; not separately on this one).

**Ruled out on our side:**

- **Our arithmetic:** the soft-float routines it uses agree exactly with the
  CPU on 60,000 random cases each, run as small shaders on the same lavapipe.
- **Uninitialized locals:** naga zero-initializes function-local variables
  (checked in naga's SPIR-V writer), so they can't be reading uninitialized
  `var`s.
- **Undefined shifts:** shift amounts are masked or bounded throughout.

## Bug 2: two equivalent shaders, different results

**Status:** observed deterministically in wasm-env's own harness, but
**not yet reproduced standalone**. Read the evidence with that in mind.

`bug2.cl` is pyopencl's complex power, `z^b` through log, hypot, atan2, exp
and sincos, with `b = -1`. wasm-env generates `bug2-lets.wgsl`. That version
binds values used only within one block with `let`, rather than as
function-scope `var`s, which keeps large shaders smaller.

`bug2-vars.wgsl` is the same shader with every value a `var`. The two differ
only in how values are bound:
- no value is both a `var` and a `let`;
- every value assigned after its declaration is a `var`;
- the soft-float functions are called the same number of times in each.

What we saw, at 128 bits, through wasm-env's own runner:

- **`let` version:** deterministically slightly wrong for some elements. For
  `1/z` at element 1 it gave 0.2965130815484636 instead of 0.3008849557522124.
- **`var` version:** correct (one element differs only in the last digits of
  a value near zero, about 1e-17).
- **Same IR on the CPU:** the LLVM IR the shader comes from, compiled natively
  with clang, gives the right answer.
- **Instrumented IR:** recording all 680 intermediate values of that same IR
  through the GPU path also made the result right. All 9,360 recorded values
  matched the CPU exactly.

With the standalone program here (`cargo run --release -- bug2 ../bug2-lets.wgsl`
and `../bug2-vars.wgsl`), **both** versions give NaN for most elements.
Element 1 is the exception and shows the same split: the `var` version
right, the `let` version wrong. We haven't found what differs between that
program and wasm-env's runner: same bindings, same inputs, same limits, same
vector width. So this needs more reduction before it's a useful report.

## Workaround in wasm-env

wasm-env sets `LP_NATIVE_VECTOR_WIDTH=128` before creating its Vulkan
instance, unless it's already set. That fixes bug 1 and also speeds up
lavapipe's compiles of these shaders. Bug 2 has no workaround; it affects
pyopencl's complex `pow` (2 of its 318 tests).

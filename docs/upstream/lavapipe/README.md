# lavapipe: wrong results from large compute shaders

Four problems found while running OpenCL kernels on lavapipe through
wasm-env's GPU service (OpenCL C → LLVM IR → WGSL → naga → SPIR-V → lavapipe).
All four involve large compute shaders: OpenCL `double` arithmetic
emulated on 32-bit integers, with libclc's math functions inlined, which
gives shaders of 25–100 KB of WGSL (40–180 KB of SPIR-V).

| | Bug 1: 256-bit miscompile | Bug 2: wrong with larger workgroups | Bug 3: wrong at both widths | Bug 4: small, all wrong |
|---|---|---|---|---|
| Status | Reproduced standalone | Reproduced standalone | Reproduced standalone | Reproduced standalone |
| Trigger | `LP_NATIVE_VECTOR_WIDTH=256` (the default on AVX2 machines) | Workgroups of more than one invocation; any width | A branch around three inlined `sin`s; any width | Workgroups of 4 (of 1: right) |
| Workaround | `LP_NATIVE_VECTOR_WIDTH=128` | None in place | None in place | None in place |

## Environment

- Mesa 26.2.3, llvmpipe with LLVM 21.1.8: `llvmpipe (LLVM 21.1.8, 256 bits)`,
  Vulkan 1.4.354, conformance version 1.3.1.1.
- CPU: 11th Gen Intel Core i7-11800H (AVX2, AVX-512F).
- Linux 7.2.8, NixOS.
- wgpu 30.0.1 and naga 30.0.1 (WGSL → SPIR-V).

## Files here

- `bug1.cl` to `bug4.cl`: the OpenCL C kernels the shaders came from (for
  context; the shaders are the reproducers).
- `bug1.wgsl`, `bug2-lets.wgsl`, `bug2-lets-group1.wgsl`, `bug2-vars.wgsl`,
  `bug3.wgsl`, `bug4.wgsl`, `bug4-group1.wgsl`: the shaders as wasm-env
  generates them.
- `*.spv`: the same shaders as SPIR-V, from naga 30 with default options.
  These are not byte for byte what wgpu submits, which uses its own naga
  options.
- `repro/`: a standalone wgpu program that runs a shader with the exact
  inputs, prints every output (value and bits), and checks each against
  Rust's `f64` arithmetic (bugs 1, 3 and 4) or the exact answer (bug 2).

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

## Bug 2: the same shader, wrong with larger workgroups

`bug2.cl` is pyopencl's complex power, `z^b` through log, hypot, atan2, exp
and sincos, with `b = -1`, for 16 complex numbers. wasm-env generates
`bug2-lets.wgsl` (workgroups of 4). `bug2-lets-group1.wgsl` is the same
shader generated for workgroups of 1: the two differ only in
`@workgroup_size` and the matching constant in the global id computation.

```sh
cd repro
VK_ICD_FILENAMES=/path/to/lvp_icd.x86_64.json \
    LP_NATIVE_VECTOR_WIDTH=128 cargo run --release -- bug2 ../bug2-lets.wgsl > /dev/null
VK_ICD_FILENAMES=/path/to/lvp_icd.x86_64.json \
    LP_NATIVE_VECTOR_WIDTH=128 cargo run --release -- bug2 ../bug2-lets-group1.wgsl > /dev/null
```

The program dispatches 16 invocations in all (4 workgroups of 4, or 16 of
1) and checks each result against `1/z`.

**Expected:** every element right, as with workgroups of 1:
"0 of 16 elements wrong".

**Actual with workgroups of 4:** "15 of 16 elements wrong", most of them NaN:

```
wrong: element 0: got NaN NaN, want -3.293297171638899e-1 5.927934908950019e-2
wrong: element 1: got 2.965130815484636e-1 2.632742800899157e-1, want 3.008849557522124e-1 2.632743362831858e-1
```

Also observed:

- **Width:** at `LP_NATIVE_VECTOR_WIDTH=256`, workgroups of 4 give finite
  but slightly wrong values everywhere (`-3.293297240097818e-1` for
  element 0's real part).
- **Workgroups of 16:** wrong too, like 4.
- **How values are bound matters, a little:** `bug2-vars.wgsl` is the
  workgroups-of-4 shader with every value a function-scope `var` (no
  `let`s). It's wrong too ("13 of 16 elements wrong"), but on different
  elements: element 1, for one, is right. (In an earlier test with
  pyopencl's own inputs, the `var` version looked entirely right, which is
  why the two versions are both here.)
- **The same IR on the CPU** (compiled natively with clang) gives the right
  answers, and recording every intermediate value through the GPU path
  matched the CPU exactly, value for value (which changes the shader's
  shape too).

This looks like the same problem as bug 3: results that depend on how many
invocations run together.

## Bug 3: three `sin`s in a branch, wrong at any vector width

`bug3.wgsl` computes libclc's double `sin` of three inputs per invocation,
inside a branch (the last invocation does nothing):

```c
if (i + 1 < get_global_size(0)) {
    out[3 * i] = sin(in[3 * i]);
    out[3 * i + 1] = sin(in[3 * i + 1]);
    out[3 * i + 2] = sin(in[3 * i + 2]);
}
```

The inputs are huge (`-0x1p+1009` and the like), which takes `sin` through
its large-argument reduction (Payne–Hanek, a table lookup and a long chain
of integer arithmetic), plus `1.0`. One workgroup of 4.

```sh
cd repro
VK_ICD_FILENAMES=/path/to/lvp_icd.x86_64.json \
    LP_NATIVE_VECTOR_WIDTH=128 cargo run --release -- bug3 ../bug3.wgsl > /dev/null
VK_ICD_FILENAMES=/path/to/lvp_icd.x86_64.json \
    LP_NATIVE_VECTOR_WIDTH=256 cargo run --release -- bug3 ../bug3.wgsl > /dev/null
```

**Expected:** each output within 4 ulps of Rust's `f64::sin` (libclc's
accuracy), the last invocation's three untouched (zero).

**Actual:** 5 of 12 outputs wrong at either width, differently:

```
width 128:
wrong: out[0] = sin(-5.486124068793689e303): got NaN, want 1.7722886839407961e-1
wrong: out[2] = sin(-1.404447761611344e306): got -9.864817135300161e-1, want -9.864816126884146e-1
wrong: out[3] = sin(-8.371160993642713e298): got NaN, want 2.4419163478511433e-1
width 256:
wrong: out[0] = sin(-5.486124068793689e303): got 1.7798366486886463e-1, want 1.7722886839407961e-1
wrong: out[2] = sin(-1.404447761611344e306): got 5.033976941658121e253, want -9.864816126884146e-1
wrong: out[3] = sin(-8.371160993642713e298): got 2.4456350246454409e-1, want 2.4419163478511433e-1
```

Also observed, through wasm-env's own runner:

- **Size:** the same kernel with one or two `sin`s in the branch is right;
  three is wrong. Without the branch, three are right too.
- **Workgroup size:** the results change with it. With workgroups of one
  invocation, a kernel computing `out[i] = sin(in[i])` (no branch), right
  with larger workgroups, gives NaN for these inputs, and at width 256 some
  outputs aren't written at all.
- **Not the optimizer:** `GALLIVM_PERF=no_opt` gives the same wrong results.
  `LP_NUM_THREADS=0` too.
- **`var`s don't help:** making every value a function-scope `var` (no
  `let`s) doesn't change the results.
- **Where it came from:** Khronos's conformance test `test_bruteforce sin`
  (double), whose 3-wide kernel (two `sin(double3)`s, one in a branch)
  fails on lavapipe; its scalar, 2-, 4-, 8- and 16-wide kernels pass.

## Bug 4: a small shader, every result wrong with workgroups of 4

This is the smallest of the four, and the quickest to run (25 KB of WGSL,
built in under a second). `bug4.wgsl` is libclc's double `acospi` on a
`double16`. wasm-env compiles double math on vectors as a loop over the
lanes through private arrays (one copy of `acospi` rather than sixteen), so
each invocation loops over 16 inputs. `bug4-group1.wgsl` is the same shader
for workgroups of 1: again only `@workgroup_size` and the matching constant
differ.

```sh
cd repro
VK_ICD_FILENAMES=/path/to/lvp_icd.x86_64.json \
    LP_NATIVE_VECTOR_WIDTH=128 cargo run --release -- bug4 ../bug4.wgsl > /dev/null
VK_ICD_FILENAMES=/path/to/lvp_icd.x86_64.json \
    LP_NATIVE_VECTOR_WIDTH=128 cargo run --release -- bug4 ../bug4-group1.wgsl > /dev/null
```

Four invocations in all, 64 inputs, each checked against `acos(x) / pi`.

**Expected:** "0 of 64 outputs wrong", as with workgroups of 1.

**Actual with workgroups of 4:** "64 of 64 outputs wrong", all NaN:

```
wrong: out[0] = acospi(-7.8125e-3): got NaN, want 5.024868212835026e-1
wrong: out[1] = acospi(-7.75e-1): got NaN, want 7.822501809667615e-1
```

Also observed:

- **Where it goes wrong:** storing intermediate values instead of the
  result, every value up to the last two selects is right, and so is
  `v58 = select(v56, v52, v57)`. The final `v60 = select(vec2<u32>(0u,
  1071644672u), v58, v59)` gives NaN: `v56`, the value `v58` chose
  against. (`v57` and `v59` are `v13 < 1023u` and `v13 > 966u`.)
- **Size:** making almost anything else in the shader simpler (replacing
  `v52` by an earlier value, for one) makes it right, and the two selects
  alone, in a small shader, are right.
- **Not the optimizer:** the same with `GALLIVM_PERF=no_opt`.
- **Not private memory:** declaring the two arrays in the function instead
  (`var` rather than `var<private>`) doesn't change it, nor does dropping
  the `loop` wrapping the lane loop.
- **Only width 16:** the same kernel on double2, double4 and double8 is
  right, as are `acos`, `asinpi` and `atanpi` on double16.

## Workaround in wasm-env

wasm-env sets `LP_NATIVE_VECTOR_WIDTH=128` before creating its Vulkan
instance, unless it's already set. That fixes bug 1 and also speeds up
lavapipe's compiles of these shaders. Bugs 2 to 4 have none in place.
Workgroups of one invocation make these four shaders right, but they aren't
a workaround: with them, other shaders go wrong (libclc's double `sinh`,
for one, right with workgroups of 4 and wrong with workgroups of 1, even
as a scalar). They look like one problem:
results that depend on how many invocations run together. They affect
pyopencl's complex `pow` (2 of its 318 tests) and some of the conformance
tests' double precision kernels (`acospi` on double16, for one). wasm-env
computes double-precision vector math as a loop over the lanes, which
happens to avoid bug 3 in the conformance test it came from (and is how
bug 4's shader looks).

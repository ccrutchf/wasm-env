# wasm-env on macOS: what to check

For whoever picks this up on a Mac with Apple silicon, you or a Claude Code
session there. It says how to build wasm-env there, what to run, what Linux
got for the same thing, and what to send back.
[docs/hosts.md](docs/hosts.md) has the background on macOS.

## Why

**Nothing has run on a Mac yet.** The runtime and translator compile for
`aarch64-apple-darwin`, but that's only checked by cross-compiling on Linux.
None of the GPU work has run on **Metal**, macOS's only GPU backend:
- the GPU service's OpenCL;
- the Vulkan driver, with Vulkan 1.1 and the conformance-test fixes (commits
  up to `1ec44718`).

So, in order of importance:

1. Do the runtime and translator work at all? Memory is the first suspect:
   Apple silicon's pages are 16 KiB, the guest's 4 KiB.
2. Does the GPU service open and work on Metal?
3. Do the Vulkan driver's own test, vkcube and vkQuake give Linux's results,
   without a window and in a Mac window?
4. Do OpenCL's conformance tests and some Vulkan groups give Linux's results?

## Ground rules

- Don't push, and don't post anything anywhere (issues, upstream projects).
  The user does all outside communication.
- Fixes go on a branch. Nothing lands on main until the full suite
  (`tests/all.sh`) passes on Linux.
- Run one heavy job at a time (a conformance run, a build, vkQuake).
- Don't run `cargo fmt`: the repository isn't rustfmt-formatted, and it
  rewrites about 60 files.

## 1. Build

Install Nix (the Determinate Systems installer works). The flake supports
`aarch64-darwin`. From the repository's root:

```
nix develop
cargo build --release
```

This gives `target/release/wasm-env` and `target/release/wasm-env-translate`.
The dev shell has the clang and wasm-ld that translation needs. It has no
qemu-user and no aarch64 cross gcc, so the tests that compare with qemu
(M2, M3, most of `tests/ubuntu.sh`) can't run on a Mac.

The unit tests, which come before anything bigger:

```
cargo test --release -p wasm-env -p bench-harness
cargo test --release -p wasm-env-translate --features lift
WASM_ENV_DEBUG_GPU=1 cargo test --release -p wasm-env-gpu -- --nocapture
```

The last one compiles OpenCL kernels to WGSL and runs them on the Mac's GPU
through Metal, checking them against the CPU. It prints the adapter it used.
On Linux all 57 pass, and on Windows under Wine. Then `tests/m0.sh` and
`tests/m1.sh` (BusyBox, processes, a shell, networking) should run as they
do on Linux.

## 2. The Ubuntu environment

The conformance tests, vkQuake and the drivers' tests are built on the Linux
machine into its `build/ubuntu`. Bring that over as an export, made on Linux:

```
nix develop -c tests/export.sh
```

The script first rebuilds the GPU drivers from the tree into the
environment. It then writes `export/ubuntu-oci.tar` and its `.sha256`
(ignored by git), and reports anything the checks below need that's
missing. The export includes everything below, without the translation
cache (which is per host):

- the Vulkan driver, at `/usr/lib/wasm-env/libwasm-env-vulkan.so` with
  `/usr/share/vulkan/icd.d/wasm-env_icd.aarch64.json`;
- the OpenCL driver (`/etc/OpenCL/vendors/wasm-env.icd`);
- the driver's test (`/opt/tests/vulkan`);
- `vkcube-wayland` and `vulkaninfo`;
- vkQuake with Quake's shareware data (`/opt/vkquake`);
- OpenCL's conformance tests (`/opt/cts`) and Vulkan's (`/opt/vk-cts`).

On the Mac:

```
target/release/wasm-env convert --lazy --no-precompile ubuntu-oci.tar build/ubuntu
target/release/wasm-env run --root build/ubuntu /usr/bin/python3.12 -c 'print(1)'
```

`--lazy` translates each program the first time it runs. So the first run of
anything big (Python, vkQuake, the conformance tests) takes minutes.

If you change the Vulkan driver (`packages/vulkan`), rebuild it on the Mac
with `packages/vulkan/build.sh` inside `nix develop`, then copy it in:

```
cp build/packages/vulkan/libwasm-env-vulkan.so build/ubuntu/usr/lib/wasm-env/
```

The build cross-compiles for arm64 Linux against `build/ubuntu`'s headers.

## 3. The GPU

On a Mac the GPU service uses Metal. Leave `WGPU_BACKEND` unset: the Linux
scripts set it to `vulkan`, which finds no adapter on a Mac. Useful
variables:

| Variable | Effect |
|---|---|
| `WASM_ENV_DEBUG_GPU=1` | prints the adapter and its quirks, plus wgpu's warnings and validation errors |
| `WASM_ENV_DEBUG_WGSL=1` | prints each shader as WGSL |
| `WASM_ENV_DEBUG_WAYLAND=1` | reports frames (`frame 60: …, N colors`), which the checks below look for |

Places where Metal differs from Linux's GPUs, and so the first places to look
when something fails:

- **No 64-bit floats:** OpenCL's doubles use the software ones (slower).
- **Depth:** 24-bit depth is 32-bit float on Apple GPUs. Depth copies and
  readbacks are special-cased in `gpu/src/vulkan.rs` (`depth24_to_buffer`,
  `depth_from_buffer`).
- **Timestamps:** these may be taken only between passes, not inside them.
  The driver's test checks timestamp queries.
- **Feature-dependent paths:** binding arrays, read-write storage textures
  and border colors depend on the GPU family. The Vulkan driver requests
  what the device has, so a missing feature should show as a feature the
  driver doesn't report, not as a crash.

## 4. The Vulkan driver

From the repository's root, with these flags (they point the environment's
Vulkan loader at wasm-env's driver only):

```
vk=(--root build/ubuntu --env VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/wasm-env_icd.aarch64.json --env HOME=/root)
```

1. **The device:**
   `target/release/wasm-env run "${vk[@]}" /usr/bin/vulkaninfo --summary`.
   Its `deviceName` should end in `(wasm-env)`.
2. **The driver's own test:**
   `target/release/wasm-env run "${vk[@]}" /opt/tests/vulkan`.
   On Linux, on both lavapipe and the Intel GPU, it prints exactly this:
   ```
   device: wasm-env's
   compute: 0 of 1000 wrong
   fill and update: right
   the program's own write kept: yes
   images: texels fetched 0 wrong, image 0 wrong
   blit: 0 of 64 wrong
   clears and resolves: 0 of 192 wrong
   draw: 5 pixels looked at, 0 wrong
   queries: occlusion right, timestamps in order
   subpasses: 0 of 256 wrong
   texel buffer: 0 of 256 wrong
   descriptor array: 0 of 12 wrong
   render area 64 of 64 right, fans 128 of 128, stencil masked 64 of 64, query right, texels written 256 of 256, BC right, features right
   layouts: 0 of 29 wrong
   Vulkan 1.1 right, subgroups right, views 128 of 128 right, 1D image 8 of 8 right
   done
   ```
   Any line that differs is worth reporting, in full.
3. **vkcube, without a window:**
   ```
   WASM_ENV_DEBUG_WAYLAND=1 target/release/wasm-env run --wayland=headless "${vk[@]}" /usr/bin/vkcube-wayland --c 70 2>&1 | grep -m1 'frame 60:'
   ```
   On Linux its color count is over 1000 (a textured cube).
4. **vkQuake, without a window:**
   ```
   time (WASM_ENV_DEBUG_WAYLAND=1 target/release/wasm-env run --wayland=headless --audio=null "${vk[@]}" /opt/vkquake/vkquake -basedir /opt/vkquake 2>&1 | grep -m1 'frame 300:')
   ```
   On Linux its 300th frame (the attract-mode demo) has over 1000 colors.
   Report the time too, and run it twice: the first run includes
   translating vkQuake.
5. **In a Mac window:** the same with `--wayland` instead of
   `--wayland=headless`, and without the `grep`. See that it looks right and
   that it's smooth. Try GTK's widget factory as well (docs/hosts.md, step 5).

## 5. OpenCL's conformance tests

```
cl=(--root build/ubuntu --env OCL_ICD_VENDORS=/etc/OpenCL/vendors/wasm-env.icd --env HOME=/root)
target/release/wasm-env run "${cl[@]}" /opt/cts/test_basic
```

Each test ends with a line like `PASSED 98 of 98 tests.` Linux's results
(lavapipe, at `1ec44718`):

| Test | Linux | Note |
|---|---|---|
| test_computeinfo | exit 0 | prints no count |
| test_basic | 98 / 98 | |
| test_compiler | 72 / 72 | |
| test_atomics | 13 / 13 | |
| test_profiling | 32 / 32 | |
| test_commonfns | 19 / 19 | |
| test_geometrics | 8 / 8 | |
| test_relationals | 17 / 17 | |
| test_select | 22 / 22 | |
| test_vectors | 9 / 9 | |
| test_mem_host_flags | 9 / 9 | |
| test_allocations | 6 / 6 | 1 GB buffers |
| test_buffers | 93 / 93 | |
| test_api | 111 / 112 | `min_max_read_image_args` wants 128 image arguments; lavapipe has 47. Metal may pass it |
| test_events | 30 / 30 | |
| test_integer_ops | 63 / 63 | about 80 minutes on lavapipe |
| test_half | 13 / 13 | |
| test_cl_get_info | 6 / 6 | |
| test_cl_read_write_images | 6 / 6 | |
| test_cl_copy_images | 14 / 14 | |
| test_cl_fill_images | 6 / 6 | |
| test_kernel_image_methods | 6 / 6 | |
| test_samplerless_reads | 6 / 6 | |
| test_image_streams | 5 / 5 | about 5 hours on lavapipe |
| test_conversions | 1045 / 1045 sub-tests | |
| test_printf | 20 / 20 | |

Start with the quick ones. The math tests (`test_bruteforce -w`) pass on
Linux and are worth one run too, because Metal's arithmetic isn't
lavapipe's.

## 6. Some of Vulkan's conformance tests

[tests/vulkan-cts.sh](tests/vulkan-cts.sh) fixes Linux's GPUs (it sets
`WGPU_BACKEND=vulkan`), so on a Mac run deqp-vk directly, a group at a time:

```
target/release/wasm-env run "${vk[@]}" /bin/sh -c "cd /opt/vk-cts && ./deqp-vk --deqp-archive-dir=/opt/vk-cts --deqp-case='dEQP-VK.api.smoke.*' --deqp-log-filename=/tmp/run.qpa --deqp-log-images=disable --deqp-log-shader-sources=disable"
```

It ends with totals: `Passed`, `Failed`, `Not supported`. A lost device ends
the run early (`Test run was ABORTED!`); report the last case named before
that. Groups worth running, smallest first, with the Intel GPU on Linux's
results for comparison:

| Group | Linux (pass / fail / device lost) |
|---|---|
| `dEQP-VK.api.smoke.*` | all pass |
| `dEQP-VK.query_pool.*` | 219 / 0 / 0 |
| `dEQP-VK.memory.*` | 2602 / 4 / 0 (before the vertex-stride fix) |
| `dEQP-VK.ubo.*` | 2241 / 51 / 0 |
| `dEQP-VK.renderpass.*` | 8544 / 58 / 97 |
| `dEQP-VK.texture.*` | 3413 / 563 / 288 (several fixes since) |

Most of what fails on Linux is listed under "Not yet" in
[docs/abi.md](docs/abi.md) (Vulkan). Differences from Linux matter more than
the totals.

## What to send back

- The adapter line from `WASM_ENV_DEBUG_GPU=1`.
- The unit tests' results: any failure's output in full.
- For each check: the command, and its output where it differs from Linux's,
  in full. Include any `wasm-env:` lines and any `panicked at` lines.
- vkQuake's time to frame 300, both runs, and how it looks in a window.
- The OpenCL table's numbers, and the Vulkan groups' totals.
- Anything that hung, and what it was doing. A hang is often a panic in the
  GPU service that left the program waiting.
- If memory misbehaves (crashes in translated programs, wrong data after
  `mmap`), `--trace`'s last lines. Then look at `host_page` in
  `runtime/src/memory.rs`: the 16 KiB pages.

# wasm-env on Windows: what to check

For whoever picks this up on a Windows x86-64 machine, you or a Claude Code
session there. It says what to bring from the Linux machine, what to run,
what Linux got for the same thing, and what to send back.
[docs/hosts.md](docs/hosts.md) has the background on Windows: how symlinks and
shared mappings work there, and where the toolchain goes.

## Why

The runtime and translator build for `x86_64-pc-windows-gnu` (cross-compiled
on Linux) and pass their tests **under Wine**. That includes the Vulkan driver
(vkcube, vkQuake) and the GPU service's unit tests, but only through Wine's
Vulkan on Linux's GPUs. The GPU work since then hasn't run on Windows at all:
Vulkan 1.1 and the conformance-test fixes (commits `9e1da1e5` to `1ec44718`).
Nor has the GPU service ever run on **DirectX 12**, Windows' own backend: Wine
can't compile the shaders wgpu makes for it. And OpenCL on Windows was one of
the goals of this round of work.

So, in order of importance:

1. Does the GPU service open and work on DirectX 12?
2. Do the Vulkan driver's own test, vkcube and vkQuake give Linux's results,
   on DirectX 12 and on Windows' native Vulkan driver?
3. Do OpenCL's conformance tests pass as they do on Linux?
4. What do some Vulkan conformance groups give, compared with Linux?

## Ground rules

- Don't push, and don't post anything anywhere (issues, upstream projects).
  The user does all outside communication.
- Fixes go on a branch. Nothing lands on main until the full suite
  (`tests/all.sh`) passes on Linux.
- Run one heavy job at a time (a conformance run, a build, vkQuake). Two at
  once have run a machine out of memory.
- Don't run `cargo fmt`: the repository isn't rustfmt-formatted, and it
  rewrites about 60 files.

## 1. On the Linux machine: build what Windows needs

From the repository's root, inside `nix develop`:

```
tests/cross.sh --build
target/release/wasm-env export build/ubuntu build/ubuntu-oci.tar
```

That gives you:

- `build/target-cross/x86_64-pc-windows-gnu/release/wasm-env.exe` and
  `wasm-env-translate.exe`. The second must sit beside the first.
- `build/win-toolchain/`, the translator's toolchain for Windows:
  `bin\clang.exe`, `bin\wasm-ld.exe`, `lib\clang\21\include\` and
  `lib\libclang_rt.builtins.a`.
- `build/ubuntu-oci.tar`, the Ubuntu 24.04 arm64 environment, without its
  translation cache. It already has everything below:
  - the Vulkan driver, at `/usr/lib/wasm-env/libwasm-env-vulkan.so` with
    `/usr/share/vulkan/icd.d/wasm-env_icd.aarch64.json`;
  - the OpenCL driver (`/etc/OpenCL/vendors/wasm-env.icd`);
  - the driver's test (`/opt/tests/vulkan`);
  - `vkcube-wayland` and `vulkaninfo`;
  - vkQuake with Quake's shareware data (`/opt/vkquake`);
  - OpenCL's conformance tests (`/opt/cts`) and Vulkan's (`/opt/vk-cts`).

  If you change the drivers or the tests on Linux, export it again.

Copy those three to the Windows machine.

## 2. On Windows: set up

Lay them out so the runtime finds its toolchain without settings: by default
it looks in `..\lib\wasm-env\toolchain` beside `wasm-env.exe`.

```
C:\wasm-env\bin\wasm-env.exe
C:\wasm-env\bin\wasm-env-translate.exe
C:\wasm-env\lib\wasm-env\toolchain\...   (the contents of build/win-toolchain)
C:\wasm-env\ubuntu-oci.tar
```

(Elsewhere, set `WASM_ENV_TOOLCHAIN` to the toolchain's directory.)

Convert the environment **on Windows**. Copying an unpacked root from Linux
won't work: an environment's symlinks are link files on Windows, which only
`convert` makes.

```powershell
cd C:\wasm-env
.\bin\wasm-env.exe convert --lazy --no-precompile ubuntu-oci.tar ubuntu
```

`--lazy` translates each program the first time it runs, not all of them now.
So the first run of anything big (Python, vkQuake, the conformance tests)
takes minutes.

A quick check that the runtime works:

```powershell
.\bin\wasm-env.exe run --root ubuntu /usr/bin/python3.12 -c 'print(1)'
```

## 3. Choosing the GPU

The GPU service (wgpu) picks a backend and an adapter from these variables,
set in PowerShell:

| Variable | Values | Effect |
|---|---|---|
| `WGPU_BACKEND` | `dx12`, `vulkan` | the backend; unset, wgpu tries all |
| `WASM_ENV_GPU` | part of an adapter's name (`intel`, `amd`, …), or `software` | prefers that adapter; unset, the most capable |
| `WASM_ENV_DEBUG_GPU` | `1` | prints the adapter and its quirks, plus wgpu's warnings and validation errors |

For example `$env:WGPU_BACKEND = "dx12"; $env:WASM_ENV_DEBUG_GPU = "1"`.
Run each check below on **DirectX 12 first**, then on `vulkan` if the machine
has a Vulkan driver, and note which adapter each run used.

If shaders fail on DirectX 12, try the other shader compiler:
`$env:WGPU_DX12_COMPILER = "dxc"`. This needs `dxcompiler.dll` and
`dxil.dll` from the DirectX Shader Compiler on the `PATH`; wgpu's default is
FXC, which is older and more limited.

## 4. The Vulkan driver

Every command here runs from `C:\wasm-env` and uses these flags. They point
the environment's Vulkan loader at wasm-env's driver only:

```powershell
$vk = @('--root', 'ubuntu', '--env', 'VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/wasm-env_icd.aarch64.json', '--env', 'HOME=/root')
```

1. **The device:**
   `.\bin\wasm-env.exe run @vk /usr/bin/vulkaninfo --summary`.
   Its `deviceName` should end in `(wasm-env)`.
2. **The driver's own test:**
   `.\bin\wasm-env.exe run @vk /opt/tests/vulkan`.
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
   ```powershell
   $env:WASM_ENV_DEBUG_WAYLAND = "1"
   .\bin\wasm-env.exe run --wayland=headless @vk /usr/bin/vkcube-wayland --c 70
   ```
   Look for the line with `frame 60:`. On Linux its color count is over
   1000 (a textured cube).
4. **vkQuake, without a window:** with `WASM_ENV_DEBUG_WAYLAND=1` still set,
   ```powershell
   Measure-Command { .\bin\wasm-env.exe run --wayland=headless --audio=null @vk /opt/vkquake/vkquake -basedir /opt/vkquake | Select-String 'frame 300:' | Select-Object -First 1 }
   ```
   On Linux its 300th frame (the attract-mode demo) has over 1000 colors.
   Report the time too, and run it twice: the first run includes
   translating vkQuake.
5. **In a window:** the same with `--wayland` instead of
   `--wayland=headless`. It shows in a window of Windows' own; see that it
   looks right.

## 5. OpenCL's conformance tests

```powershell
$cl = @('--root', 'ubuntu', '--env', 'OCL_ICD_VENDORS=/etc/OpenCL/vendors/wasm-env.icd', '--env', 'HOME=/root')
.\bin\wasm-env.exe run @cl /opt/cts/test_basic
```

Each test ends with a line like `PASSED 98 of 98 tests.` Linux's results
(lavapipe, at `1ec44718`), in a sensible order to run them:

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
| test_api | 111 / 112 | `min_max_read_image_args` wants 128 image arguments; lavapipe has 47. A GPU may pass it |
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

Start with the quick ones. A real GPU should be much faster than lavapipe.
DirectX 12 has no 64-bit floats in FXC's shaders, so doubles fall back to the
software ones: slow, but they should pass.

## 6. Some of Vulkan's conformance tests

[tests/vulkan-cts.sh](tests/vulkan-cts.sh) is a Linux script: it fixes
Linux's GPUs. On Windows, run deqp-vk directly, a group at a time:

```powershell
.\bin\wasm-env.exe run @vk /bin/sh -c "cd /opt/vk-cts && ./deqp-vk --deqp-archive-dir=/opt/vk-cts --deqp-case='dEQP-VK.api.smoke.*' --deqp-log-filename=/tmp/run.qpa --deqp-log-images=disable --deqp-log-shader-sources=disable"
```

It ends with totals: `Passed`, `Failed`, `Not supported`. A lost device ends
the run early (`Test run was ABORTED!`); report the last case named before
that.

Groups worth running, smallest first, with the Intel GPU on Linux's results
for comparison:

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

## 7. Optional: a native build

All of the above uses the Linux-built executables. A build on Windows itself
hasn't been tried:

- `rustup target add x86_64-pc-windows-gnu`, with MinGW-w64's gcc, should
  work like the cross build.
- MSVC (`x86_64-pc-windows-msvc`) hasn't been tried at all.

`cargo test --release -p wasm-env-gpu` would then run the GPU service's unit
tests on DirectX 12. Under Wine (on Vulkan) all 57 pass.

## What to send back

- The adapter line from `WASM_ENV_DEBUG_GPU=1`, for each backend tried.
- For each check: the command, and its output where it differs from Linux's,
  in full. Include any `wasm-env:` lines and any `panicked at` lines.
- vkQuake's time to frame 300, both runs.
- The OpenCL table's numbers, and the Vulkan groups' totals.
- Anything that hung. Note what it was doing; a hang is often a panic in the
  GPU service that left the program waiting.

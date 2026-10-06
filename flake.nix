{
  description = "wasm-env: a Linux environment whose native architecture is WebAssembly";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      # (nixpkgs no longer supports x86_64-darwin; macOS means arm64.)
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # `nix build` / `nix profile install`: wasm-env and its translator, with
      # the toolchain translation compiles with (clang, wasm-ld and compiler-rt
      # builtins for wasm64) in lib/wasm-env/toolchain, where the translator
      # looks for it (translator/src/link.rs, Toolchain::from_env). So
      # programs translate on first use without the dev shell.
      packages = forAll (pkgs:
        let
          llvm = pkgs.llvmPackages_21;
          lib = pkgs.lib;
          # compiler-rt's generic builtins (128-bit integers, IEEE quad) for
          # wasm64, as toolchain/build.sh builds them but with clang's own
          # freestanding headers (translated code needs no libc headers).
          builtins = pkgs.runCommand "wasm-env-builtins" { nativeBuildInputs = [ llvm.llvm ]; } ''
            src=$TMPDIR/builtins
            cp -r ${llvm.compiler-rt.src}/lib/builtins $src 2>/dev/null || cp -r ${llvm.compiler-rt.src}/compiler-rt/lib/builtins $src
            chmod -R u+w $src
            sed -i 's/^#if defined(__aarch64__) || defined(__arm64ec__)$/& || defined(__wasm__)/' $src/fp_compare_impl.inc
            skip='^(atomic|clear_cache|crtbegin|crtend|emutls|enable_execute_stack|eprintf|gcc_personality_v0|os_version_check|trampoline_setup|cpu_model|aarch64|x86|i386|arm|riscv|ppc|mips|loongarch|hexagon|ve|.*xf.*|.*xc.*|.*bf.*|udivmodti4_fast)\b'
            mkdir -p $TMPDIR/obj $out/lib
            for f in $src/*.c; do
              name=$(basename $f .c)
              [[ $name =~ $skip ]] && continue
              ${llvm.clang-unwrapped}/bin/clang --target=wasm64-unknown-unknown -O2 -ffreestanding \
                -mbulk-memory -mmutable-globals -msign-ext -mnontrapping-fptoint -mmultivalue \
                -I$src -c $f -o $TMPDIR/obj/$name.o 2>/dev/null || true
            done
            llvm-ar rcs $out/lib/libclang_rt.builtins.a $TMPDIR/obj/*.o
          '';
          # OpenCL's builtins (LLVM's libclc, its clspv target: math in
          # software, for the GPU service), as packages/libclc/build.sh
          # builds them: marked to be inlined into kernels.
          libclc = pkgs.stdenv.mkDerivation {
            pname = "wasm-env-libclc";
            version = llvm.llvm.version;
            srcs = [
              (pkgs.fetchurl {
                url = "https://github.com/llvm/llvm-project/releases/download/llvmorg-21.1.8/libclc-21.1.8.src.tar.xz";
                hash = "sha256-bCZ3NipTUxw17fSCvckXHqBHHKCh6ROKybWheCklYW8=";
              })
              (pkgs.fetchurl {
                url = "https://github.com/llvm/llvm-project/releases/download/llvmorg-21.1.8/cmake-21.1.8.src.tar.xz";
                hash = "sha256-hXNfIP2MgeywoJq7DCZwGEdUIOk7ZQUMxbdjTqt0Tek=";
              })
            ];
            sourceRoot = ".";
            nativeBuildInputs = [ pkgs.cmake pkgs.ninja pkgs.python3 llvm.llvm llvm.clang-unwrapped ];
            dontConfigure = true;
            buildPhase = ''
              ln -s cmake-21.1.8.src cmake
              patch -d libclc-21.1.8.src -p1 < ${./packages/libclc/conformance.patch}
              cmake -S libclc-21.1.8.src -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DLIBCLC_TARGETS_TO_BUILD=clspv-- \
                -DLLVM_DIR=${llvm.llvm.dev}/lib/cmake/llvm -DLLVM_TOOLS_BINARY_DIR=${llvm.llvm}/bin \
                -DLLVM_TOOL_clang=${llvm.clang-unwrapped}/bin/clang
              ninja -C build
            '';
            installPhase = ''
              mkdir -p $out/lib/wasm-env/clc
              llvm-dis build/clspv--.bc -o - |
                sed -E '/^attributes #/{s/ noinline//g; s/ "no-builtins"//g; /"frame-pointer"/s/^(attributes #[0-9]+ = \{ )/\1alwaysinline /}' |
                llvm-as -o $out/lib/wasm-env/clc/clspv--.bc
            '';
          };
          # Libraries the compositor's host windows (winit) load at run time
          # on Linux: the host's window system, Wayland or X11, and the Vulkan
          # loader the GPU service (wgpu) opens.
          windowLibs = with pkgs; [ wayland libxkbcommon libx11 libxcursor libxrandr libxi vulkan-loader ];
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./gpu ./packages/opencl ./runtime ./translator ./vendor ./bench/harness ];
          };
        in
        rec {
          inherit builtins libclc;
          wasm-env = pkgs.rustPlatform.buildRustPackage {
            pname = "wasm-env";
            version = "0.1.0";
            inherit src;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "wasm-env" "-p" "wasm-env-translate" "--features" "wasm-env-translate/lift" ];
            doCheck = false;
            nativeBuildInputs = [ llvm.clang ];
            LIBCLANG_PATH = "${llvm.libclang.lib}/lib";
            postFixup = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              patchelf --add-rpath ${lib.makeLibraryPath windowLibs} $out/bin/wasm-env
            '';
            postInstall = ''
              tc=$out/lib/wasm-env/toolchain
              mkdir -p $tc/bin $tc/lib
              ln -s ${llvm.clang-unwrapped}/bin/clang $tc/bin/clang
              ln -s ${llvm.lld}/bin/wasm-ld $tc/bin/wasm-ld
              ln -s ${builtins}/lib/libclang_rt.builtins.a $tc/lib/libclang_rt.builtins.a
              # OpenCL's builtins (runtime/src/gpu.rs looks here).
              mkdir -p $out/lib/wasm-env/clc
              ln -s ${libclc}/lib/wasm-env/clc/clspv--.bc $out/lib/wasm-env/clc/clspv--.bc
              # The guest's OpenCL driver (packages/opencl), for environments.
              mkdir -p $out/lib/wasm-env/guest
              ${llvm.clang-unwrapped}/bin/clang --target=aarch64-linux-gnu -ffreestanding -nostdlib -shared -fPIC \
                -fuse-ld=${llvm.lld}/bin/ld.lld -O2 -fvisibility=hidden -fno-stack-protector -mno-outline-atomics \
                -Wno-incompatible-function-pointer-types -I${pkgs.opencl-headers}/include \
                -Wl,-soname,libwasm-env-opencl.so -o $out/lib/wasm-env/guest/libwasm-env-opencl.so packages/opencl/icd.c
              # And its Vulkan driver (packages/vulkan).
              ${llvm.clang-unwrapped}/bin/clang --target=aarch64-linux-gnu -ffreestanding -nostdlib -shared -fPIC \
                -fuse-ld=${llvm.lld}/bin/ld.lld -O2 -fvisibility=hidden -fno-stack-protector -mno-outline-atomics \
                -I${pkgs.vulkan-headers}/include \
                -Wl,-soname,libwasm-env-vulkan.so -o $out/lib/wasm-env/guest/libwasm-env-vulkan.so packages/vulkan/icd.c
            '';
          };
          default = wasm-env;
        });

      devShells = forAll (pkgs:
        let llvm = pkgs.llvmPackages_21;
        in {
          default = pkgs.mkShell ({
            packages = [
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
              llvm.clang # wrapped clang for native builds
              llvm.lld # provides wasm-ld
              llvm.llvm # llvm-objdump etc.
              pkgs.wabt # wasm2c, wasm-objdump, wasm2wat
              pkgs.simde # SIMD headers used by wasm2c output
              pkgs.wasmtime
              pkgs.gnumake
              pkgs.zstd # Ubuntu .deb packages (tests/ubuntu-fetch.sh)
              pkgs.jq # PyPI metadata (tests/wheel-fetch.sh)
              pkgs.unzip # wheels
              pkgs.opencl-headers # packages/opencl (the guest's OpenCL driver)
              pkgs.vulkan-headers # packages/vulkan (the guest's Vulkan driver)
              pkgs.glslang # its tests' compute shaders, as SPIR-V
            ]
            # Test references that exist only on Linux hosts; elsewhere the
            # tests that need them skip.
            ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              # M2: arm64 reference toolchain and emulator for translator tests.
              pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc
              pkgs.qemu-user
              # M8: a headless compositor for Wayland clients, and the host's own
              # wayland-info to compare against (tests/ubuntu.sh).
              pkgs.weston
              pkgs.wayland-utils
            ];
            # The wrapped clang injects host flags that break wasm targets,
            # so wasm builds use the unwrapped compiler.
            WASM_CC = "${llvm.clang-unwrapped}/bin/clang";
            # wasm2c's runtime sources and headers.
            WASM2C_RT = "${pkgs.wabt}/share/wabt/wasm2c";
            WASM2C_INCLUDE = "${pkgs.wabt}/include";
            # Sources for the wasm64-linux-musl toolchain (toolchain/build.sh).
            MUSL_SRC = "${pkgs.musl.src}";
            COMPILER_RT_SRC = "${llvm.compiler-rt.src}";
            # Packages built with the toolchain (packages/*/build.sh).
            BUSYBOX_SRC = "${pkgs.busybox.src}";
            OPENCL_HEADERS = "${pkgs.opencl-headers}";
            VULKAN_HEADERS = "${pkgs.vulkan-headers}";
            # bindgen (used by the bad64 arm64 decoder crate) needs libclang.
            LIBCLANG_PATH = "${llvm.libclang.lib}/lib";
          } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            # The compositor's host windows (winit) and the GPU service (wgpu)
            # load these at run time.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ wayland libxkbcommon libx11 libxcursor libxrandr libxi vulkan-loader ]);
            # arm64 musl runtime (libc.so, ld-musl) for dynamic-linking tests.
            AARCH64_MUSL = "${pkgs.pkgsCross.aarch64-multiplatform-musl.musl}";
          });
        });
    };
}

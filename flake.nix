{
  description = "wasm-env: a Linux environment whose native architecture is WebAssembly";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
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
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./runtime ./translator ./vendor ./bench/harness ];
          };
        in
        rec {
          inherit builtins;
          wasm-env = pkgs.rustPlatform.buildRustPackage {
            pname = "wasm-env";
            version = "0.1.0";
            inherit src;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "wasm-env" "-p" "wasm-env-translate" "--features" "wasm-env-translate/lift" ];
            doCheck = false;
            nativeBuildInputs = [ llvm.clang ];
            LIBCLANG_PATH = "${llvm.libclang.lib}/lib";
            postInstall = ''
              tc=$out/lib/wasm-env/toolchain
              mkdir -p $tc/bin $tc/lib
              ln -s ${llvm.clang-unwrapped}/bin/clang $tc/bin/clang
              ln -s ${llvm.lld}/bin/wasm-ld $tc/bin/wasm-ld
              ln -s ${builtins}/lib/libclang_rt.builtins.a $tc/lib/libclang_rt.builtins.a
            '';
          };
          default = wasm-env;
        });

      devShells = forAll (pkgs:
        let llvm = pkgs.llvmPackages_21;
        in {
          default = pkgs.mkShell {
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
              # M2: arm64 reference toolchain and emulator for translator tests.
              pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc
              pkgs.qemu-user
              pkgs.gnumake
              pkgs.zstd # Ubuntu .deb packages (tests/ubuntu-fetch.sh)
              pkgs.jq # PyPI metadata (tests/wheel-fetch.sh)
              pkgs.unzip # wheels
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
            # arm64 musl runtime (libc.so, ld-musl) for dynamic-linking tests.
            AARCH64_MUSL = "${pkgs.pkgsCross.aarch64-multiplatform-musl.musl}";
            # bindgen (used by the bad64 arm64 decoder crate) needs libclang.
            LIBCLANG_PATH = "${llvm.libclang.lib}/lib";
          };
        });
    };
}

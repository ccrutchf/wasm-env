{
  description = "wasm-env: a Linux environment whose native architecture is WebAssembly";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
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
              pkgs.gnumake
            ];
            # The wrapped clang injects host flags that break wasm targets,
            # so wasm builds use the unwrapped compiler.
            WASM_CC = "${llvm.clang-unwrapped}/bin/clang";
            # wasm2c's runtime sources and headers.
            WASM2C_RT = "${pkgs.wabt}/share/wabt/wasm2c";
            WASM2C_INCLUDE = "${pkgs.wabt}/include";
          };
        });
    };
}

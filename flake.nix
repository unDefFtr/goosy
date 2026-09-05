{
  description = "GoosyRenderer development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            config.allowUnfree = true;
          };
          linuxLibraries = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
            pkgs.fontconfig
            pkgs.freetype
            pkgs.libGL
            pkgs.libglvnd
            pkgs.libxkbcommon
            pkgs.vulkan-loader
            pkgs.vulkan-validation-layers
            pkgs.wayland
            pkgs.libx11
            pkgs.libxcursor
            pkgs.libxi
            pkgs.libxrandr
            pkgs.libdrm
          ];
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clang
              clippy
              cmake
              ffmpeg
              glslang
              shaderc
              vulkan-tools
              vulkan-headers
              maturin
              ninja
              nodejs_22
              pkg-config
              python313
              rust-analyzer
              rustfmt
            ] ++ lib.optionals stdenv.hostPlatform.isLinux [
              cudaPackages.cuda_cudart
              cudaPackages.cuda_nvcc
            ];

            buildInputs = linuxLibraries;

            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux (
              pkgs.lib.makeLibraryPath (
                linuxLibraries
                ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                  pkgs.cudaPackages.cuda_cudart
                ]
              )
              + ":/run/opengl-driver/lib:/run/opengl-driver-32/lib"
            );
            shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              export CC=clang
              export CUDA_PATH="${pkgs.cudaPackages.cuda_nvcc}"
              export PATH="$CUDA_PATH/bin:$PATH"
              export GOOSY_REQUIRE_GLSLC=1
            '';
          };
        }
      );
    };
}

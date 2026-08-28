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
          pkgs = import nixpkgs { inherit system; };
          linuxLibraries = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
            pkgs.fontconfig
            pkgs.freetype
            pkgs.libGL
            pkgs.libxkbcommon
            pkgs.vulkan-loader
            pkgs.wayland
            pkgs.libx11
            pkgs.libxcursor
            pkgs.libxi
            pkgs.libxrandr
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
              maturin
              ninja
              nodejs_22
              pkg-config
              python313
              rust-analyzer
              rustc
              rustfmt
            ];

            buildInputs = linuxLibraries;

            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux (
              pkgs.lib.makeLibraryPath linuxLibraries
            );
          };
        }
      );
    };
}

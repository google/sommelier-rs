{
  inputs = {
    nixpkgs.url = "https://flakehub.com/f/DeterminateSystems/nixpkgs-weekly/0.1";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config.allowUnfree = true;
        };
        rust-toolchain = pkgs.rust-bin.nightly.latest.default.override {
          targets = [ "x86_64-unknown-linux-musl" ];
          extensions = [ "rust-src" ];
        };
      in {
        devShell = pkgs.mkShell {
          buildInputs = [
            rust-toolchain
            pkgs.wayland
            pkgs.libxkbcommon
            pkgs.libglvnd
            pkgs.mesa
          ];

          shellHook = ''
            export LD_LIBRARY_PATH="${pkgs.wayland}/lib:${pkgs.libxkbcommon}/lib:${pkgs.libglvnd}/lib:${pkgs.mesa}/lib:$LD_LIBRARY_PATH"
          '';
        };
      }
    );
}

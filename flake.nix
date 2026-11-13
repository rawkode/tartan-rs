{
  description = "Tartan development shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            # rustup installs the toolchain pinned in rust-toolchain.toml on first use.
            rustup
            git
            nodejs
            openspec
            cue
            rclone
            rustfs
            jq
            curl
            wasm-tools
            # Builds walgit (WALGIT_BIN), which needs protoc.
            protobuf
            # The CNCF distribution `registry` binary (REGISTRY_BIN).
            distribution
          ];

          shellHook = ''
            export REGISTRY_BIN="$(command -v registry)"
            export CUE_BIN="$(command -v cue)"
          '';
        };
      });
    };
}

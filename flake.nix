# flake.nix — Nix dev shell for atmospheric-horizon.
#
# The Rust toolchain is pinned by rust-toolchain.toml and expected to come
# from the user's environment (rustup) rather than nixpkgs; python3 and
# minisign are provided for the horizon-mixins staging scripts and release
# signing.

{
  description = "atmospheric-horizon — AT Protocol integration for Horizon (Minecraft in Rust)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              python3
              minisign
            ];

            shellHook = ''
              echo "atmospheric-horizon dev shell ready (python3 + minisign; bring your own rustup)"
            '';
          };
        }
      );

      # Code formatting via nixfmt
      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}

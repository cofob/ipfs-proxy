{
  description = "Rust project";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-22.11";
    import-cargo.url = github:edolstra/import-cargo;
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, import-cargo, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };
      in
      {
        packages.default = pkgs.callPackage ./package.nix { inherit (import-cargo.builders) importCargo; };
      }
    );
}

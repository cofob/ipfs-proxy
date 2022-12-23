{ pkgs, stdenv, importCargo, rustc, cargo, openssl, pkg-config, ... }:

stdenv.mkDerivation {
  name = "ipfs-proxy";
  src = ./.;

  buildInputs = [
    (importCargo { lockFile = ./Cargo.lock; inherit pkgs; }).cargoHome

    # Build-time dependencies
    rustc
    cargo
    openssl.dev
    pkg-config
  ];

  nativeBuildInputs = [
    openssl
  ];

  buildPhase = ''
    cargo build --release
  '';

  installPhase = ''
    install -Dm775 ./target/release/ipfs-proxy $out/bin/ipfs-proxy
  '';
}

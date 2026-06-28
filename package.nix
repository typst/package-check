{
  lib,
  rustPlatform,
  pkg-config,
  openssl,
}:
let
  cargoMeta = lib.importTOML ./Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = cargoMeta.package.name;
  inherit (cargoMeta.package) version;
  src = ./.;
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ openssl ];
  cargoHash = "sha256-7lpmevoAZ0pRTdpSx17IhhwdqhcVVsvoqHUci+Makf8=";
  # Don't run `cargo test`, as there are no tests to run.
  doCheck = false;
}

{
  lib,
  rustPlatform,
  pkg-config,
  openssl,
  notices ? null,
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
  cargoHash = "sha256-ux/5X6Ew39/6vX6bNPwtbhb2cQ3u9Hqq3xszKNMD5fc=";
  # Don't run `cargo test`, as there are no tests to run.
  doCheck = false;

  # Ship a NOTICE file covering the licenses of all dependencies, created by
  # the prebuilt `notices` binary. Cargo uses the vendored dependencies here, so
  # no network access is needed.
  postInstall = lib.optionalString (notices != null) ''
    mkdir -p $out/share/licenses/${cargoMeta.package.name}
    ${lib.getExe notices} release --frozen \
      -o $out/share/licenses/${cargoMeta.package.name}/NOTICE
  '';
}

# The prebuilt `notices` binary, used to create the NOTICE file of a release.
{
  lib,
  stdenv,
  fetchurl,
  autoPatchelfHook,
}:
let
  version = "0.1.2";
  assets = {
    x86_64-linux = {
      name = "notices-linux-x86_64";
      hash = "sha256-PbnpE3HlzDsgT3YbAw94bAgfwxagfNLd50W72ihvPXo=";
    };
    aarch64-linux = {
      name = "notices-linux-aarch64";
      hash = "sha256-my4a4W4in6jeNxNVyb2toQNlmIvjaSdh+nmf2emlnXU=";
    };
    aarch64-darwin = {
      name = "notices-macos-aarch64";
      hash = "sha256-3uOK9PYhPNoMzGusBP/FPIeSkap9sjfxZJQDgCLB6/Q=";
    };
  };
  asset = assets.${stdenv.hostPlatform.system} or null;
in
if asset == null then
  null
else
  stdenv.mkDerivation {
    pname = "notices";
    inherit version;
    src = fetchurl {
      url = "https://github.com/typst/notices/releases/download/v${version}/${asset.name}";
      inherit (asset) hash;
    };
    dontUnpack = true;
    nativeBuildInputs = lib.optionals stdenv.hostPlatform.isLinux [ autoPatchelfHook ];
    buildInputs = lib.optionals stdenv.hostPlatform.isLinux [ stdenv.cc.cc.lib ];
    installPhase = ''
      runHook preInstall
      install -Dm755 $src $out/bin/notices
      runHook postInstall
    '';
    meta = {
      description = "Tool for Typst NOTICE files";
      homepage = "https://github.com/typst/notices";
      license = with lib.licenses; [ mit asl20 ];
      mainProgram = "notices";
      platforms = builtins.attrNames assets;
    };
  }

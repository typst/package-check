{
  description = "Tool to check a Typst package.";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils }:
    let cargoMeta = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in utils.lib.eachDefaultSystem (system:
      let pkgs = nixpkgs.legacyPackages.${system};
      in {
        packages = rec {
          default = typst-package-check;
          typst-package-check = pkgs.rustPlatform.buildRustPackage {
            pname = cargoMeta.package.name;
            version = cargoMeta.package.version;
            src = ./.;
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.openssl.dev pkgs.git ];
            useFetchCargoVendor = true;
            cargoHash = "sha256-0t6bQzAbok/xBi53tr7KCU78Ew/i99B+AyfR36r7AA4=";
            # Don't run `cargo test`, as there are no tests to run.
            doCheck = false;
          };
          docker-image = pkgs.dockerTools.buildImage {
            name = "ghcr.io/typst/package-check";
            tag = typst-package-check.version;
            copyToRoot = with pkgs.dockerTools; [
              caCertificates
              pkgs.git
              typst-package-check
            ];
            config = {
              Entrypoint = [ "/bin/typst-package-check" ];
              WorkingDir = "/data";
            };
          };
        };
      });
}

{
  description = "Tool to check a Typst package.";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      inherit (nixpkgs) lib;
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = lib.genAttrs supportedSystems;
    in
    {
      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-tree);

      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        rec {
          default = typst-package-check;

          typst-package-check = pkgs.callPackage ./package.nix { };

          docker-image = pkgs.dockerTools.buildImage {
            name = "ghcr.io/typst/package-check";
            tag = typst-package-check.version;
            copyToRoot = [
              pkgs.dockerTools.caCertificates
              pkgs.gitMinimal
              pkgs.bashNonInteractive
              pkgs.busybox
              typst-package-check
            ];
            config = {
              Entrypoint = [ "/bin/typst-package-check" ];
              WorkingDir = "/data";
            };
          };
        }
      );
    };
}

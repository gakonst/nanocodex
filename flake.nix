{
  description = "Nanocodex CLI packages and system modules";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          nanocodex = pkgs.callPackage ./nix/package.nix { };
        in
        {
          inherit nanocodex;
          default = nanocodex;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.nanocodex}/bin/nanocodex";
          meta = self.packages.${system}.nanocodex.meta;
        };
      });

      checks = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          package = self.packages.${system}.nanocodex;
        in
        {
          package = package;
          cli-journey = pkgs.runCommand "nanocodex-cli-journey" { nativeBuildInputs = [ package ]; } ''
            nanocodex --version | tee nanocodex-version.txt
            nanocodex2 --version | tee nanocodex2-version.txt
            grep -F 'nanocodex-bin Version: ${package.version}' nanocodex-version.txt
            grep -F 'nanocodex2 Version: ${package.version}' nanocodex2-version.txt
            ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              /usr/bin/codesign --verify --strict ${package}/bin/nanocodex
              /usr/bin/codesign --verify --strict ${package}/bin/nanocodex2
            ''}
            mkdir -p "$out"
            cp *-version.txt "$out/"
          '';
        }
      );

      nixosModules.nanocodex = import ./nix/module.nix self;
      nixosModules.default = self.nixosModules.nanocodex;

      darwinModules.nanocodex = import ./nix/darwin-module.nix self;
      darwinModules.default = self.darwinModules.nanocodex;
    };
}

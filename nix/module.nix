self:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.nanocodex;
  system = pkgs.stdenv.hostPlatform.system;
in
{
  options.programs.nanocodex = {
    enable = lib.mkEnableOption "Nanocodex";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${system}.nanocodex;
      defaultText = lib.literalExpression "inputs.nanocodex.packages.\${pkgs.system}.nanocodex";
      description = "The Nanocodex package to install.";
    };

    enableNixLd = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Enable the NixOS dynamic loader compatibility layer for verified
        runtime components downloaded by Nanocodex itself.
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        environment.systemPackages = [ cfg.package ];
      }
      (lib.mkIf cfg.enableNixLd {
        programs.nix-ld.enable = true;
        programs.nix-ld.libraries = [ pkgs.stdenv.cc.cc.lib ];
      })
    ]
  );
}

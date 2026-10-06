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
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
  };
}

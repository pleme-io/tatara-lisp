# tatara-lisp — nix-darwin module.
#
# Mirror of the NixOS module — puts `tatara-script` on the system PATH
# via `environment.systemPackages`, which on nix-darwin lands in
# `/run/current-system/sw/bin` (inherited by every login shell + launchd
# agent). Pairs with the home-manager module at `module/default.nix`.
#
# Enable via `blackmatter.components.tatara-script.enable = true;`.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.blackmatter.components.tatara-script;

  resolvedPackage =
    if pkgs ? tatara-script
    then pkgs.tatara-script
    else if pkgs ? tatara-lisp-script
    then pkgs.tatara-lisp-script
    else throw ''
      blackmatter.components.tatara-script.enable = true (darwin) requires
      either:
        (a) inputs.tatara-lisp.overlays.default applied to nixpkgs, or
        (b) blackmatter.components.tatara-script.package set explicitly.
    '';
in {
  options.blackmatter.components.tatara-script = {
    enable = lib.mkEnableOption "tatara-script on the nix-darwin system PATH";

    package = lib.mkOption {
      type = lib.types.package;
      default = resolvedPackage;
      defaultText = lib.literalExpression "pkgs.tatara-script (via overlays.default)";
      description = ''
        Derivation providing `bin/tatara-script`. Lands in
        `/run/current-system/sw/bin` on darwin; every user + launchd
        agent + Terminal.app login shell picks it up.
      '';
    };

    logLevel = lib.mkOption {
      type = lib.types.enum ["debug" "info" "warn" "error" "silent"];
      default = "info";
      description = ''
        System-wide TATARA_LOG default. Set via `environment.variables`
        so launchd + zsh interactive + non-login `sh` all inherit.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    environment.variables.TATARA_LOG = cfg.logLevel;
  };
}

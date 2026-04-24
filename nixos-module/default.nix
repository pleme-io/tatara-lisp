# tatara-lisp — NixOS module.
#
# Puts the `tatara-script` binary on the system PATH so systemd services,
# k3s pods that bind-mount /run/current-system/sw, and the root user all
# resolve `tatara-script`. Pairs with the home-manager module at
# `module/default.nix` — HM handles per-user PATH for interactive shells,
# this handles the system level.
#
# Enable via `blackmatter.components.tatara-script.enable = true;`. The
# module resolves `pkgs.tatara-script` via the flake's `overlays.default`
# (applied at the consumer's `nixpkgs.overlays = […]` site) or via an
# explicit `package` override.
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
      blackmatter.components.tatara-script.enable = true (NixOS) requires
      either:
        (a) inputs.tatara-lisp.overlays.default applied to nixpkgs, or
        (b) blackmatter.components.tatara-script.package set explicitly.
    '';
in {
  options.blackmatter.components.tatara-script = {
    enable = lib.mkEnableOption "tatara-script on the system PATH";

    package = lib.mkOption {
      type = lib.types.package;
      default = resolvedPackage;
      defaultText = lib.literalExpression "pkgs.tatara-script (via overlays.default)";
      description = ''
        Derivation providing `bin/tatara-script`. Lands in
        `environment.systemPackages` — available to root, systemd units,
        and any subshell on the host.
      '';
    };

    logLevel = lib.mkOption {
      type = lib.types.enum ["debug" "info" "warn" "error" "silent"];
      default = "info";
      description = ''
        System-wide TATARA_LOG default exported via `environment.variables`.
        HM module's per-user value wins inside a user shell; this matters
        for systemd units and non-login shells.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    environment.variables.TATARA_LOG = cfg.logLevel;
  };
}

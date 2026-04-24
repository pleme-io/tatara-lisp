# tatara-lisp — home-manager module.
#
# Exposes the `tatara-script` binary on the user's PATH so `.tlisp` files
# invoke as `tatara-script foo.tlisp` from any shell, and so `nix-run`
# apps in sibling flakes can shebang against the user's installed copy.
#
# The home-manager module intentionally mirrors the blackmatter-cli
# shape: enable flag + package option. Config hooks (script path,
# editor bindings, repl history location) land here when the stdlib
# grows a feature that warrants per-user state.
#
# Consumers opt in via `blackmatter.components.tatara-script.enable`
# from their home configuration. The `package` default resolves
# against `pkgs.tatara-script` (requires `overlays.tatara-script`
# applied to the nixpkgs instance) or may be passed explicitly from a
# flake's `packages.<system>.tatara-script`.
#
# Namespace alias: `services.tatara-script` forwards to
# `blackmatter.components.tatara-script` for users who prefer the
# nix-darwin / HM service-style path.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.blackmatter.components.tatara-script;

  # Resolve a sensible default without forcing the overlay. If the
  # caller has applied `overlays.tatara-script` we get `pkgs.tatara-script`
  # (or its alias `pkgs.tatara-lisp-script`). Otherwise the caller is
  # expected to set `package` explicitly.
  resolvedPackage =
    if pkgs ? tatara-script
    then pkgs.tatara-script
    else if pkgs ? tatara-lisp-script
    then pkgs.tatara-lisp-script
    else throw ''
      blackmatter.components.tatara-script.enable = true requires either:
        (a) inputs.tatara-lisp.overlays.tatara-script applied to pkgs, or
        (b) blackmatter.components.tatara-script.package set explicitly
            to a derivation that provides `bin/tatara-script`.
    '';
in {
  options.blackmatter.components.tatara-script = {
    enable = lib.mkEnableOption "tatara-script (pleme-io Lisp scripting language)";

    package = lib.mkOption {
      type = lib.types.package;
      default = resolvedPackage;
      defaultText = lib.literalExpression "pkgs.tatara-script (via overlays.tatara-script)";
      description = ''
        Derivation providing `bin/tatara-script`. Defaults to the
        overlay-provided `pkgs.tatara-script`; override with a specific
        flake output when pinning or testing a branch.
      '';
    };

    scriptDir = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "\${config.home.homeDirectory}/scripts/tlisp";
      description = ''
        Optional directory where personal `.tlisp` scripts live. When
        set, the directory is appended to the user session's PATH via
        `home.sessionPath` and exported as `TATARA_LISP_PATH` so
        `(require "foo.tlisp")` can resolve helpers from it (when
        invoked with relative paths starting with `./`).
      '';
    };

    logLevel = lib.mkOption {
      type = lib.types.enum ["debug" "info" "warn" "error" "silent"];
      default = "info";
      description = ''
        Default `TATARA_LOG` level. The stdlib's `log-*` primitives
        honor this — scripts that expect to log at debug must not
        rely on a silent default.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    home.sessionVariables = {
      TATARA_LOG = cfg.logLevel;
    } // lib.optionalAttrs (cfg.scriptDir != null) {
      TATARA_LISP_PATH = cfg.scriptDir;
    };

    home.sessionPath = lib.optional (cfg.scriptDir != null) cfg.scriptDir;
  };
}

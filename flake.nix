{
  description = "tatara-lisp — homoiconic S-expression reader + macroexpander + #[derive(TataraDomain)] proc macro. Ships the `tatara-script` binary as the official pleme-io scripting surface (.tlisp replaces bash in nix-run apps).";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    crate2nix.url = "github:nix-community/crate2nix";
    flake-utils.url = "github:numtide/flake-utils";
    substrate = {
      url = "github:pleme-io/substrate";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    crate2nix,
    flake-utils,
    substrate,
  }: let
    # Substrate's baseline workspace-release outputs (packages.tatara-lisp,
    # apps.{bump,release,check-all,regenerate-cargo-nix}, devShells, etc).
    baseline = (import "${substrate}/lib/rust-workspace-release-flake.nix" {
      inherit nixpkgs crate2nix flake-utils;
    }) {
      toolName = "tatara-lisp";
      packageName = "tatara-lisp";
      src = self;
      repo = "pleme-io/tatara-lisp";
    };

    # Per-system augmentation: expose `tatara-lisp-script` as a first-class
    # package + an `apps.tatara-script` that dispatches whatever .tlisp path
    # the caller supplies (so downstream flakes can just depend on this
    # flake's `apps.<system>.tatara-script` and pass their own path).
    scriptAugment = flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs { inherit system; };
      cargoNix = import ./Cargo.nix {
        inherit pkgs;
      };
      tatara-lisp-script = cargoNix.workspaceMembers."tatara-lisp-script".build;
    in {
      packages.tatara-lisp-script = tatara-lisp-script;
      packages.tatara-script = tatara-lisp-script;

      apps.tatara-script = {
        type = "app";
        program = "${tatara-lisp-script}/bin/tatara-script";
      };

      # Direct symlink alias so downstream `nix run pleme-io/tatara-lisp#script`
      # works as a shorthand.
      apps.script = {
        type = "app";
        program = "${tatara-lisp-script}/bin/tatara-script";
      };

      overlays.tatara-script = _final: _prev: {
        inherit tatara-lisp-script;
      };
    });
  in
    nixpkgs.lib.recursiveUpdate baseline scriptAugment;
}

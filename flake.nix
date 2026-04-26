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

      # Distroless OCI image — used by the wasm-engine pods to evaluate
      # tatara-lisp programs at runtime. Same content-addressed pattern
      # as substrate's tool-image-flake.nix; we build it inline here so
      # the consumer flake stays a single import.
      image = if pkgs.stdenv.isLinux then
        pkgs.dockerTools.buildLayeredImage {
          name = "ghcr.io/pleme-io/tatara-lisp-script";
          tag = "0.2.0";
          contents = [
            tatara-lisp-script
            pkgs.cacert
            pkgs.dockerTools.fakeNss
            pkgs.bashInteractive    # for diagnostic shell access; remove when settled
          ];
          config = {
            Entrypoint = [ "${tatara-lisp-script}/bin/tatara-script" ];
            User = "65532:65532";
            Env = [
              "PATH=${tatara-lisp-script}/bin:/usr/bin:/bin"
              "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
              "RUST_LOG=info,tatara_lisp_script=debug"
            ];
            Labels = {
              "org.opencontainers.image.source" = "https://github.com/pleme-io/tatara-lisp";
              "org.opencontainers.image.description" =
                "tatara-lisp-script — pleme-io Lisp scripting + WASM/WASI program evaluator";
              "org.opencontainers.image.licenses" = "MIT";
              "org.opencontainers.image.version" = "0.2.0";
            };
          };
        }
      else
        # Cross-system stub on Darwin — the image is Linux-only.
        pkgs.runCommand "tatara-lisp-script-image-stub" {} ''
          mkdir -p $out
          echo "Build the OCI image on Linux:" > $out/README
          echo "  nix build .#image --system aarch64-linux" >> $out/README
        '';
    in {
      packages.tatara-lisp-script = tatara-lisp-script;
      packages.tatara-script = tatara-lisp-script;
      packages.image = image;

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
    });

    # System-agnostic outputs: overlays (consumer supplies final pkgs) and
    # home-manager modules (pure Nix, no pkgs dependency at the module
    # top level). Both are kept outside `eachDefaultSystem` so consumers
    # reach them as `flake.overlays.tatara-script` / `flake.homeManagerModules.default`
    # rather than the per-system wrapped forms.
    crossSystemAugment = {
      overlays.tatara-script = final: _prev: let
        cargoNix = import ./Cargo.nix { pkgs = final; };
        pkg = cargoNix.workspaceMembers."tatara-lisp-script".build;
      in {
        tatara-lisp-script = pkg;
        tatara-script = pkg;
      };

      # `overlays.default` is the well-known entry point for consumers that
      # want the overlay without caring about its name.
      overlays.default = final: _prev: let
        cargoNix = import ./Cargo.nix { pkgs = final; };
        pkg = cargoNix.workspaceMembers."tatara-lisp-script".build;
      in {
        tatara-lisp-script = pkg;
        tatara-script = pkg;
      };

      homeManagerModules.default = import ./module;
      homeManagerModules.tatara-script = import ./module;

      nixosModules.default = import ./nixos-module;
      nixosModules.tatara-script = import ./nixos-module;

      darwinModules.default = import ./darwin-module;
      darwinModules.tatara-script = import ./darwin-module;
    };
  in
    nixpkgs.lib.recursiveUpdate
      (nixpkgs.lib.recursiveUpdate baseline scriptAugment)
      crossSystemAugment;
}

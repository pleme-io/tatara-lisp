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
    # Follows substrate's pin rather than carrying its own, per the
    # no-rev-pin doctrine — one fenix in the closure, no skew.
    fenix.follows = "substrate/fenix";
  };

  outputs = {
    self,
    nixpkgs,
    crate2nix,
    flake-utils,
    substrate,
    fenix,
  }: let
    # Substrate's baseline workspace-release outputs (packages.tatara-lisp,
    # apps.{bump,release,check-all,regenerate-cargo-nix}, devShells, etc).
    # fenix is passed explicitly: the flake takes it as `fenix ? null`, and
    # omitting it silently falls back to nixpkgs' rustc. That fallback is what
    # broke the v0.3.36 release — Cargo.toml declares rust-version 1.97.0
    # (measured against the fenix toolchain, which is 1.97.1) while the devShell
    # the release gate runs in was serving nixpkgs' 1.95.0, so the gate died on
    # `rustc 1.95.0 is not supported by the following packages`. The MSRV was
    # right; the shell was the one that never got told.
    baseline = (import "${substrate}/lib/rust-workspace-release-flake.nix" {
      inherit nixpkgs crate2nix flake-utils fenix;
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
      lockfileBuilder = import "${substrate}/lib/build/rust/lockfile-builder.nix" { inherit pkgs; };
      plemeCrateOverrides = import "${substrate}/lib/build/rust/pleme-crate-overrides.nix";
      cargoNix = lockfileBuilder.mkProject {
        src = self;
        defaultCrateOverrides = pkgs.defaultCrateOverrides // plemeCrateOverrides;
      };
      tatara-lisp-script = cargoNix.workspaceMembers."tatara-lisp-script".build;

      # Distroless OCI image — used by the wasm-engine pods to evaluate
      # tatara-lisp programs at runtime. Same content-addressed pattern
      # as substrate's tool-image-flake.nix; we build it inline here so
      # the consumer flake stays a single import.
      image = if pkgs.stdenv.isLinux then
        pkgs.dockerTools.buildLayeredImage {
          name = "ghcr.io/pleme-io/tatara-lisp-script";
          tag = "0.3.0";
          # Universal action base — bakes in every CLI a pleme-io/actions
          # tlisp script might shell out to via exec-check, so per-action
          # Dockerfiles stay pure (FROM + COPY + ENTRYPOINT, no RUN).
          # New tool needed? Add it here, cut a tatara-lisp release.
          contents = [
            tatara-lisp-script

            # Cert + identity bootstrap
            pkgs.cacert
            pkgs.dockerTools.fakeNss

            # Generic core utils — many tlisp scripts call sh/sed/awk via exec-check
            pkgs.coreutils
            pkgs.gnused
            pkgs.gawk
            pkgs.findutils
            pkgs.gnugrep
            pkgs.gzip
            pkgs.gnutar
            pkgs.bashInteractive

            # Per-action tooling
            pkgs.git              # git-push-with-token + checkouts
            pkgs.curl             # generic HTTP
            pkgs.openssh          # ssh remotes
            pkgs.ruby_3_3         # gem-publish (gem build/push)
            pkgs.kubernetes-helm  # helm-oci-publish (lint/package/push)
            pkgs.skopeo           # oci-image-push (fallback when forge absent)
            pkgs.openssl          # often pulled by gem deps
          ];
          # buildLayeredImage doesn't create /tmp /var/tmp /run by
          # default — skopeo (and most "real" tools) need them at
          # runtime for staging tarballs. Create with sticky bit.
          extraCommands = ''
            mkdir -p tmp var/tmp run
            chmod 1777 tmp var/tmp
          '';
          config = {
            Entrypoint = [ "${tatara-lisp-script}/bin/tatara-script" ];
            # Run as root. Originally non-root for K8s pod hygiene, but
            # the GHA Docker-action use case mounts $GITHUB_OUTPUT (and
            # related file_commands) owned by root in the container —
            # a non-root user can't write to those, breaking output
            # forwarding. Defaulting to root keeps the action use case
            # working; security-sensitive Kubernetes consumers can
            # override `securityContext.runAsUser` at deploy time.
            Env = [
              "PATH=${tatara-lisp-script}/bin:/usr/bin:/bin"
              "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
              "RUST_LOG=info,tatara_lisp_script=debug"
            ];
            Labels = {
              "org.opencontainers.image.source" = "https://github.com/pleme-io/tatara-lisp";
              "org.opencontainers.image.description" =
                "tatara-script + common tooling — universal base image for pleme-io/actions Docker actions";
              "org.opencontainers.image.licenses" = "MIT";
              "org.opencontainers.image.version" = "0.3.0";
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
    crossSystemAugment = let
      mkPkg = pkgs:
        let
          lfb = import "${substrate}/lib/build/rust/lockfile-builder.nix" { inherit pkgs; };
          pco = import "${substrate}/lib/build/rust/pleme-crate-overrides.nix";
          project = lfb.mkProject {
            src = self;
            defaultCrateOverrides = pkgs.defaultCrateOverrides // pco;
          };
        in project.workspaceMembers."tatara-lisp-script".build;
    in {
      overlays.tatara-script = final: _prev: let
        pkg = mkPkg final;
      in {
        tatara-lisp-script = pkg;
        tatara-script = pkg;
      };

      # `overlays.default` is the well-known entry point for consumers that
      # want the overlay without caring about its name.
      overlays.default = final: _prev: let
        pkg = mkPkg final;
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

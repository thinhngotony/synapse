{
  description = "Synapse — single-command AI harness installer and auto-updater";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  # Pre-built packages are served from Cachix so a fresh install does not compile
  # anything. `synapse install` prompts the user to trust this; CI pushes to it.
  nixConfig = {
    extra-substituters = [ "https://synapse.cachix.org" ];
    extra-trusted-public-keys = [
      "synapse.cachix.org-1:2W4A4S39XeD4dJ1cUCVOgFRs9L2Xg1r0xdVfCEHUCzE="
    ];
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # Already packaged upstream in nixpkgs (pkgs/by-name/he/herdr) and in
        # homebrew-core, so we track upstream's maintenance instead of forking it.
        herdr = pkgs.herdr;
        skillshare = pkgs.callPackage ./nix/skillshare.nix { };

        # omp version-checks bun at runtime and rejects nixpkgs' current 1.3.13.
        bun = pkgs.callPackage ./nix/bun.nix { };
        omp = pkgs.callPackage ./nix/omp.nix { inherit bun; };

        # The Rust CLI itself (SYN-1 scaffold).
        synapse = pkgs.rustPlatform.buildRustPackage {
          pname = "synapse";
          version = "1.0.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # Nix build sandbox has no system packages; tests that probe `which sh`
          # need `sh` in the build environment.
          nativeBuildInputs = [ pkgs.bash ];
          meta.mainProgram = "synapse";
        };
      in
      {
        packages = {
          inherit
            herdr
            skillshare
            omp
            synapse
            ;

          # Every managed package in one closure, for Cachix warming and for
          # `nix-fast-build` to fan out in a single invocation.
          harness = pkgs.symlinkJoin {
            name = "synapse-harness";
            paths = [
              herdr
              skillshare
              omp
            ];
          };

          default = synapse;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            # Parallel package builds; CI uses it to fan out the platform matrix.
            nix-fast-build
            cachix
            nixfmt-rfc-style
          ];
        };
      }
    );
}

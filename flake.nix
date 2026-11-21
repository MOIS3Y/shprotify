{
  description = "Self-hosted music server with a headless HTTP API";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      crane,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        craneLib = crane.mkLib pkgs;
        cargoManifest = fromTOML (builtins.readFile ./Cargo.toml);
        cargoPackage = cargoManifest.package;

        commonArgs = {
          pname = cargoPackage.name;
          version = cargoPackage.version;
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              craneLib.filterCargoSources path type
              || path == toString ./examples/shprotify.toml
              || pkgs.lib.hasPrefix "${toString ./migrations}/" path;
          };
          strictDeps = true;

          meta = {
            description = cargoPackage.description;
            license = pkgs.lib.meta.getLicenseFromSpdxId cargoPackage.license;
            mainProgram = cargoPackage.name;
          };
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        packageArgs = commonArgs // {
          inherit cargoArtifacts;
        };
        shprotify = craneLib.buildPackage packageArgs;

        app =
          flake-utils.lib.mkApp {
            drv = shprotify;
            name = cargoPackage.name;
          }
          // {
            meta.description = cargoPackage.description;
          };

        checks = {
          inherit shprotify;

          clippy = craneLib.cargoClippy (
            packageArgs
            // {
              cargoClippyExtraArgs = pkgs.lib.concatStringsSep " " [
                "--all-targets"
                "--all-features"
                "--"
                "--deny"
                "warnings"
              ];
            }
          );

          rustfmt = craneLib.cargoFmt { src = commonArgs.src; };
          tests = craneLib.cargoTest packageArgs;

          nixfmt =
            pkgs.runCommand "shprotify-nixfmt"
              {
                nativeBuildInputs = [ pkgs.nixfmt ];
              }
              ''
                nixfmt --check ${./flake.nix}
                touch "$out"
              '';

          tombi =
            pkgs.runCommand "shprotify-tombi"
              {
                nativeBuildInputs = [ pkgs.tombi ];
                src = pkgs.lib.cleanSource ./.;
              }
              ''
                export XDG_CACHE_HOME="$TMPDIR/cache"
                mkdir -p "$XDG_CACHE_HOME"
                cd "$src"
                tombi lint --offline --error-on-warnings
                tombi format --check
                touch "$out"
              '';
        };
      in
      {
        packages = {
          default = shprotify;
          "${cargoPackage.name}" = shprotify;
        };

        apps = {
          default = app;
          "${cargoPackage.name}" = app;
        };

        inherit checks;

        formatter = pkgs.nixfmt-tree;

        devShells.default = craneLib.devShell {
          inherit checks;

          packages = with pkgs; [
            nixfmt
            rust-analyzer
            tombi
          ];

          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      }
    );
}

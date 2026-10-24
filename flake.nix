{
  description = "Dual-pane terminal file manager and sync tool with vim keys and a review-before-you-overwrite confirmation window";

  # No rust-overlay: the toolchain comes from nixpkgs so a consumer can set
  # `inputs.lazysync.inputs.nixpkgs.follows` and have it actually work.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # Configure with .override rather than by copying files over the result;
      # the store is read-only and anything copied in is clobbered on update.
      # See the argument comments in package.nix.
      packages = forAllSystems (pkgs: rec {
        lazysync = pkgs.callPackage ./package.nix { };
        default = lazysync;
      });

      overlays.default = final: _prev: {
        lazysync = final.callPackage ./package.nix { };
      };

      apps = forAllSystems (pkgs: rec {
        lazysync = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.lazysync;
        };
        default = lazysync;
      });

      devShells = forAllSystems (
        pkgs:
        let
          # The integration fixtures in later phases need a real server on the
          # other end; a planner tested only against a mock will agree with the
          # mock and nothing else.
          fixtures = with pkgs; [
            openssh
            minio
            minio-client
          ];
        in
        {
          default = pkgs.mkShell {
            packages =
              (with pkgs; [
                rustc
                cargo
                clippy
                rustfmt
                rust-analyzer
                pkg-config
                openssl

                bacon
                cargo-watch
                cargo-nextest
                typos

                rsync
                rclone
              ])
              ++ fixtures;

            env = {
              RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
              RUST_BACKTRACE = "1";
            };
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}

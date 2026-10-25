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
          # The integration fixtures need a real server on the other end; a
          # planner tested only against a mock will agree with the mock and
          # nothing else.
          #
          # garage rather than minio for the S3 side: minio was removed from
          # nixpkgs after being abandoned upstream. rclone above doubles as the
          # S3 client, so there is no separate one here.
          fixtures = [
            pkgs.openssh
            # pyftpdlib for the FTP side, not pure-ftpd or vsftpd. Both of
            # those want root -- for the chroot jail and the setuid -- and a
            # test fixture does not have it; pure-ftpd just exits 252 with
            # nothing on stderr. pyftpdlib is a real server (real LIST, PASV,
            # MFMT, the lot) built to run as an ordinary user, which is why
            # it is what most FTP *clients* are tested against.
            # pyopenssl alongside it: pyftpdlib's `--tls` refuses to start
            # without it, and the FTPS tests then skip instead of running.
            (pkgs.python3.withPackages (ps: [
              ps.pyftpdlib
              ps.pyopenssl
            ]))
            pkgs.garage
          ];
        in
        {
          default = pkgs.mkShell {
            # openssl goes in buildInputs, not packages: `packages` lands in
            # nativeBuildInputs, and the pkg-config wrapper builds
            # PKG_CONFIG_PATH from buildInputs only. With it in the wrong list,
            # openssl-sys' build script cannot find openssl and the build fails
            # before compiling anything of ours.
            buildInputs = [
              pkgs.openssl
              # keyring's Secret Service backend links libdbus.
              pkgs.dbus
            ];

            nativeBuildInputs =
              (with pkgs; [
                rustc
                cargo
                clippy
                rustfmt
                rust-analyzer
                pkg-config

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

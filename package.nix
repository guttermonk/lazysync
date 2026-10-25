{
  lib,
  rustPlatform,
  makeWrapper,
  openssl,
  dbus,
  pkg-config,
  stdenv,
  darwin,

  # Optional external tools, put on lazysync's PATH.
  #
  # `rsync` is purely an executor: the native engine is always available, and
  # `executor = "rsync"` falls back to it with a note when rsync is absent.
  #
  # **`rclone` is more than an executor.** It is also how lazysync reaches
  # `rclone://` remotes at all -- and therefore how it reaches S3, WebDAV, B2,
  # Google Drive and the rest, since those have no native backend here. Drop
  # it from this list and `rclone://` bookmarks stop working, which is a larger
  # loss than losing an executor. Everything else keeps working.
  #
  # Passed as a list rather than booleans so a consumer can supply their own
  # build -- an rclone with extra backends compiled in, say.
  #
  #   lazysync.override { syncBackends = [ pkgs.rsync pkgs.rclone ]; }
  #   lazysync.override { syncBackends = [ pkgs.rclone ]; }  # no rsync executor
  #   lazysync.override { syncBackends = [ ]; }   # native engine, no rclone://
  rsync,
  rclone,
  syncBackends ? [ rsync rclone ],
}:

rustPlatform.buildRustPackage {
  pname = "lazysync";
  version = "0.1.0";

  src = lib.cleanSource ./.;

  cargoLock.lockFile = ./Cargo.lock;

  # No rustc/cargo here: buildRustPackage brings nixpkgs' toolchain itself, and
  # a second one would shadow it. rust-toolchain.toml is for rustup users.
  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];

  buildInputs =
    [ openssl ]
    # keyring stores bookmark passwords in the platform's own credential
    # store, which on Linux means Secret Service over libdbus and on Darwin
    # means the Keychain via the Security framework.
    ++ lib.optionals stdenv.hostPlatform.isLinux [ dbus ]
    ++ lib.optionals stdenv.hostPlatform.isDarwin [
      darwin.apple_sdk.frameworks.Security
    ];

  # --suffix, not --prefix: the point is to guarantee the executors are
  # reachable at all, while still letting a user's own rsync or rclone -- which
  # may be newer, or configured with remotes in their own home -- win.
  postInstall = lib.optionalString (syncBackends != [ ]) ''
    wrapProgram $out/bin/lazysync \
      --suffix PATH : ${lib.makeBinPath syncBackends}
  '';

  meta = {
    description = "Dual-pane terminal file manager and sync tool with a review-before-you-overwrite confirmation window";
    homepage = "https://github.com/guttermonk/lazysync";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "lazysync";
    platforms = lib.platforms.unix;
  };
}

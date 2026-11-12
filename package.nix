{
  lib,
  rustPlatform,
  makeWrapper,
  openssl,
  aws-lc,
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
    [
      openssl
      # aws-lc reaches us through remotefs-ftp's "rustls-aws-lc-rs" feature in
      # Cargo.toml -> aws-lc-rs -> aws-lc-sys, which builds C.
      #
      # It has to be a buildInput because nixpkgs decides for us. The cargo
      # setup hook's cargoUseSystemLibs() exports AWS_LC_SYS_USE_SYSTEM=1 for
      # every buildRustPackage, which tells aws-lc-sys to link a system AWS-LC
      # rather than vendor its own -- and then its build script panics outright
      # if it cannot find one. There is no in-crate fallback to vendoring.
      #
      # Worth knowing that this is a nixpkgs-side change rather than anything
      # we did: the hook had no AWS_LC_SYS_USE_SYSTEM line before 2026-11, so
      # this built fine until the pin crossed it. Without aws-lc here the
      # failure reads as an openssl problem -- aws-lc-sys probes OPENSSL_DIR,
      # finds openssl-dev, and rejects it for a missing openssl/base.h, which
      # is an AWS-LC header and was never going to be there.
      aws-lc
    ]
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

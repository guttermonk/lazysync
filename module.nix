# home-manager module for lazysync.
#
# What is and is not managed declaratively here follows from which files
# lazysync *writes*:
#
#   config.toml     lazysync only ever reads it, so it is safe to own.
#   bookmarks.toml  lazysync writes it from the UI -- `n` adds a connection,
#                   `e` edits one, `dd` deletes one. Owning it declaratively
#                   means those keys have nowhere to write.
#
# So `settings` is the normal way to use this module, and `bookmarks` is
# offered but off by default, with the trade stated at its option. Pretending
# both are equally safe would be the easy documentation and the wrong one.
#
# Passwords are never managed here. A bookmark stores everything except the
# password by design; the keyring or a prompt supplies that. An option that
# accepted one would write it into the Nix store, which is world-readable.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    mkEnableOption
    mkOption
    mkIf
    types
    literalExpression
    ;

  cfg = config.programs.lazysync;

  # `settings` and `bookmarks` are both TOML, so the same renderer does both.
  toml = pkgs.formats.toml { };
in
{
  options.programs.lazysync = {
    enable = mkEnableOption "lazysync, a dual-pane terminal file manager and sync tool";

    package = mkOption {
      type = types.package;
      default = pkgs.lazysync;
      defaultText = literalExpression "pkgs.lazysync";
      description = ''
        The lazysync package to install.

        Use `.override { syncBackends = [ ... ]; }` to choose which external
        tools are on its PATH. Note that `rclone` is not only an optional
        executor -- it is how `rclone://` bookmarks work at all, and so how
        S3, WebDAV, B2 and Google Drive are reached.
      '';
    };

    settings = mkOption {
      inherit (toml) type;
      default = { };
      example = literalExpression ''
        {
          show_hidden = false;
          sync_browse = {
            enabled = true;
            on_missing = "ghost";
          };
          compare.mtime_tolerance_secs = 2;
          safety.max_delete_fraction = 0.25;
          keys.browser.extra = [
            [ "ToggleSyncBrowse" "S" ]
            [ "SyncPush" [ "s" "p" ] ]
          ];
          profiles.deploy = {
            mode = "push";
            exclude = [ "/target/" "*.log" ];
          };
        }
      '';
      description = ''
        Contents of `config.toml`, written to
        `$XDG_CONFIG_HOME/lazysync/config.toml`.

        lazysync only reads this file, so managing it declaratively costs
        nothing. Every key is documented in `config.example.toml` in the
        repository; an unknown key is a hard error at startup rather than
        being ignored, so a typo here surfaces immediately.
      '';
    };

    bookmarks = mkOption {
      type = types.nullOr (types.listOf toml.type);
      default = null;
      example = literalExpression ''
        [
          {
            name = "www-prod";
            protocol = "sftp";
            host = "box.example.com";
            username = "deploy";
            auth = "key";
            identity_file = "~/.ssh/id_ed25519";
            remote_path = "/srv/www";
            local_path = "/home/greg/proj";
            sync_profile = "deploy";
          }
        ]
      '';
      description = ''
        Saved connections, written to
        `$XDG_CONFIG_HOME/lazysync/bookmarks.toml`.

        **This makes the bookmark editor read-only.** lazysync writes that
        file when you add (`n`), edit (`e`) or delete (`dd`) a connection, and
        home-manager points it at the Nix store, which is not writable. With
        this set, lazysync detects the situation and says so plainly rather
        than appearing to save — but the connections then have to be declared
        here, not in the UI.

        Leave it `null` (the default) to let lazysync own the file, which is
        what you want unless you are deliberately treating your connection
        list as configuration.

        Never put a password in here: it would be written to the Nix store,
        which is world-readable. Set `remember_password = true` on a bookmark
        and let the OS keyring hold it, or let lazysync prompt.
      '';
    };
  };

  config = mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile = lib.mkMerge [
      (lib.mkIf (cfg.settings != { }) {
        "lazysync/config.toml".source = toml.generate "lazysync-config.toml" cfg.settings;
      })

      # `bookmark` is the TOML table name lazysync expects: the store
      # deserializes `[[bookmark]]`, not `[[bookmarks]]`.
      (lib.mkIf (cfg.bookmarks != null) {
        "lazysync/bookmarks.toml".source = toml.generate "lazysync-bookmarks.toml" {
          bookmark = cfg.bookmarks;
        };
      })
    ];
  };
}

{
  pkgs,
  config,
  lib,
  ...
}:
let
  cfg = config.services.rlamus;
  staticUser = cfg.user != null && cfg.group != null;
in
{
  options.services.rlamus = {
    enable = lib.mkEnableOption "backend of sumalr, a mobile app for GLM assisted personal knowledge base";
    package = lib.mkPackageOption pkgs "rlamus-server" { };
    chromiumPackage = lib.mkPackageOption pkgs "chromium" { };

    user = lib.mkOption {
      type = with lib.types; nullOr str;
      default = null;
      example = "rlamus";
      description = ''
        User account under which to run rlamus. Defaults to [`DynamicUser`](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html#DynamicUser=)
        when set to `null`.

        The user will automatically be created, if this option is set to a non-null value.
      '';
    };
    group = lib.mkOption {
      type = with lib.types; nullOr str;
      default = cfg.user;
      defaultText = lib.literalExpression "config.services.rlamus.user";
      example = "rlamus";
      description = ''
        Group under which to run rlamus. Only used when `services.rlamus.user` is set.

        The group will automatically be created, if this option is set to a non-null value.
      '';
    };

    dataDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/rlamus";
      description = "The data directory where database and dialog histories are stored.";
    };
    bind = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      description = "IP address HTTP server binds on.";
    };
    modelName = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      description = "Generative model to use. Must support vision.";
    };
    ollamaEndpoint = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      description = "Ollama endpoint URL";
    };

    extraEnv = lib.mkOption {
      default = null;
      type = lib.types.nullOr lib.types.envVar;
      description = "Extra environment variables to use.";
    };
    extraOpts = lib.mkOption {
      default = null;
      type = lib.types.nullOr lib.types.str;
      description = "Extra command line options to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    users = lib.mkIf staticUser {
      users.${cfg.user} = {
        inherit (cfg) home;
        isSystemUser = true;
        group = cfg.group;
      };
      groups.${cfg.group} = { };
    };
    systemd.services.rlamus = {
      description = "rlamus, backend of sumalr, a mobile app for GLM assisted personal knowledge base";
      requires = [ "network-online.target" ];
      after = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];
      serviceConfig =
        lib.optionalAttrs staticUser {
          User = cfg.user;
          Group = cfg.group;
        }
        // {
          ExecStart = lib.concatStringsSep " " (
            [
              (lib.getExe cfg.package)
              "--data-dir ${cfg.dataDir}"
            ]
            ++ lib.optional (cfg.bind != null) "--bind ${cfg.bind}"
            ++ lib.optional (cfg.extraOpts != null) cfg.extraOpts
          );
          Environment = [
            "CHROMIUM_BIN=${lib.getExe cfg.chromiumPackage}"
          ]
          ++ (lib.optional (cfg.extraEnv != null) cfg.extraEnv)
          ++ lib.optional (cfg.ollamaEndpoint != null) "OLLAMA_ENDPOINT=${cfg.ollamaEndpoint}"
          ++ lib.optional (cfg.modelName != null) "RLAUMS_MODEL=${cfg.modelName}";
          DynamicUser = true;
          WorkingDirectory = cfg.dataDir;
          StateDirectory = [ (lib.removePrefix "/var/lib/" cfg.dataDir) ];
          ReadWritePaths = [ cfg.dataDir ];
        };
    };
  };
}

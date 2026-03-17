{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
with lib;
let
  cfg = config.services.pullix;

  urlSpecConfig = types.submodule {
    options = {
      ref = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Git reference (branch name)";
      };

      rev = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Git revision (commit SHA or tag)";
      };
    };
  };
  flakeRefType = types.submodule {
    options = {
      type = mkOption {
        type = types.enum [
          "Git"
          "GitHttp"
          "GitHttps"
          "GitSsh"
          "GitFile"
          "GitHub"
          "GitLab"
          "SourceHut"
          "Mercurial"
          "Tarball"
          "File"
          "Path"
          "Indirect"
        ];
        description = "Type of flake reference";
      };

      repo = mkOption {
        type = types.str;
        description = "Repository URL or identifier";
      };

      host = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Custom host for self-hosted git servers";
      };

      prodSpec = mkOption {
        type = types.nullOr urlSpecConfig;
        default = null;
        description = "Config for prod deployment (nixos-rebuild switch)";
      };

      testSpec = mkOption {
        type = types.nullOr urlSpecConfig;
        default = null;
        description = "Config for test deployment (nixos-rebuild test)";
      };
    };
  };

  privateKeyType = types.submodule {
    options = {
      path = mkOption {
        type = types.path;
        description = "Path to the private key file";
      };
      passphrase_path = mkOption {
        type = types.path;
        description = "Path to the passphrase file";
      };
    };
  };

  configFormat = pkgs.formats.toml { };

  urlSpecToToml =
    urlSpec:
    if urlSpec != null then
      (filterAttrs (n: v: v != null) {
        inherit (urlSpec) ref rev;
      })
    else
      null;

  flakeRepoToToml =
    flakeRepo:
    if flakeRepo != null then
      (filterAttrs (n: v: v != null) {
        inherit (flakeRepo) type repo host;
        prod_spec = urlSpecToToml flakeRepo.prodSpec;
        test_spec = urlSpecToToml flakeRepo.testSpec;
      })
    else
      null;

  configFile = configFormat.generate "pullix-config.toml" (
    filterAttrs (n: v: v != null) {
      flake_repo = flakeRepoToToml cfg.flakeRepo;
      poll_interval_secs = cfg.pollIntervalSecs;
      app_dir = cfg.appDir;
      hostname = cfg.hostname;
      otel_http_endpoint = cfg.otelHttpEndpoint;
      private_key = cfg.privateKey;
      keep_last = cfg.keepLast;
    }
  );
in
{
  options.services.pullix = {
    enable = mkEnableOption "Pullix deployment service";

    flakeRepo = mkOption {
      type = flakeRefType;
      description = "Flake reference for test deployments";
      example = literalExpression ''
        {
          type = "GitHub";
          repo = "owner/repo";
          prodSpec = {
            ref = "main";
          };
        }
        or
        {
          type = "GitHub";
          repo = "owner/repo";
          testSpec = {
            ref = "main";
            rev = "my-tag-or-sha";
          };
          prodSpec = {
            ref = "main";
          };
          host = "my.custom.host";
        }
      '';
    };

    pollIntervalSecs = mkOption {
      type = types.int;
      default = 60;
      description = "Polling interval in seconds";
    };

    appDir = mkOption {
      type = types.str;
      default = "/var/lib/pullix";
      description = "Directory for pullix state files";
    };

    hostname = mkOption {
      type = types.str;
      default = config.networking.hostName;
      defaultText = literalExpression "config.networking.hostName";
      description = "Hostname to use for nixosConfiguration lookup in flake";
    };

    environmentFile = mkOption {
      type = types.nullOr types.path;
      description = "Additional environment file to source for the pullix service";
      example = literalExpression ''
        NIX_CONFIG='access-tokens = github.com=ghp_xxx'
      '';
      default = null;
    };

    otelHttpEndpoint = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Endpoint for OpenTelemetry HTTP exporter.";
    };

    privateKey = mkOption {
      type = types.nullOr privateKeyType;
      default = null;
      description = "Private key to access git ssh repository";
    };

    verbose_logs = mkOption {
      type = types.bool;
      default = false;
      description = "Logs become very verbose";
    };

    keepLast = mkOption {
      type = types.int;
      default = 100;
      description = "Number of deployments to keep in history (internal state)";
    };
  };

  config = mkIf cfg.enable {
    # Ensure app directory exists
    systemd.tmpfiles.rules = [
      "d ${cfg.appDir} 0755 root root -"
    ];

    systemd.services.pullix = {
      description = "Pullix deployment service";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      # Prevent nixos-rebuild from restarting this service during switch
      reloadIfChanged = false;
      restartIfChanged = false;
      unitConfig.X-StopOnRemoval = false;
      stopIfChanged = false;
      path = with pkgs; [
        coreutils
        gnutar
        xz.bin
        gzip
        gitMinimal
        config.nix.package.out
        config.programs.ssh.package
        systemd
      ];
      environment = mkMerge [
        {
          PULLIX_CONFIG = "${toString configFile}";
          inherit (config.environment.sessionVariables) NIX_PATH;
          HOME = "/root";
        }
        config.nix.envVars
        config.networking.proxy.envVars
        (mkIf cfg.verbose_logs { RUST_LOG = "DEBUG"; })
      ];

      serviceConfig = {
        Type = "simple";
        Restart = "on-failure";
        RestartSec = "10s";
        EnvironmentFile = cfg.environmentFile;
        ExecStart = "${self.packages.${pkgs.system}.pullix}/bin/pullix";
      };
    };
  };
}

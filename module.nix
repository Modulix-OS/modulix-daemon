{ config, lib, pkgs, self, system, ... }:

let
  cfg = config.mx.services.modulix-daemon;

  coreUtils = self.inputs.modulix-core-utils.packages.${system};

  configDir = "/etc/modulix-os";
  cacheDir = "/var/cache/modulix-os";

  buildQueueDir = "/tmp/mx-build-queue";
  skipRebuildLock = "/tmp/mx-skip-rebuild.lock";

  vmResultDir = "${config.users.users.${cfg.testUser}.home}/modulix-vm";

  hasModuleIndex = cfg.moduleIndexFiles != { };

  installModuleIndex = pkgs.writeShellScript "modulix-install-module-index" ''
    set -euo pipefail
    ${lib.concatStringsSep "\n" (lib.mapAttrsToList (name: src: ''
      install -Dm644 ${src} "${cacheDir}/${name}"
    '') cfg.moduleIndexFiles)}
  '';

  mx-init-test = pkgs.writeShellScriptBin "mx-init-test" ''
    set -euo pipefail

    if [ "$(id -u)" -ne 0 ]; then
      echo "mx-init-test: must run as root (writes ${configDir})" >&2
      exit 1
    fi

    export PATH="${lib.makeBinPath [ pkgs.git config.nix.package ]}:/run/current-system/sw/bin:$PATH"

    mkdir -p "${vmResultDir}"
    cd "${vmResultDir}"

    ${cfg.initPackage}/bin/mx-init --debug \
      --config-dir "${configDir}" \
      --root / \
      --hostname modulix-test \
      --username ${cfg.testUser} --fullname "Modulix Test User" \
      --desktop gnome --locale fr_FR.UTF-8 --timezone Europe/Paris \
      --kb-layout fr --kb-variant "" --console-keymap fr "$@"

    chown -R ${cfg.testUser}:users "${vmResultDir}"
  '';
in
{
  options.mx.services.modulix-daemon = {
    enable = lib.mkOption {
      type        = lib.types.bool;
      default     = true;
      description = "Enable the Modulix OS daemon";
    };

    package = lib.mkOption {
      type        = lib.types.package;
      default     =
        let p = self.packages.${system};
        in if cfg.testMode then p.mx-daemon-test else p.default;
      defaultText = lib.literalMD "`mx-daemon` from this flake (`mx-daemon-test` when `testMode`)";
      description = "Modulix daemon package";
    };

    testMode = lib.mkEnableOption "test mode: debug build, nixos-rebuild build-vm, isolated config dir";

    testUser = lib.mkOption {
      type        = lib.types.nullOr lib.types.str;
      default     = null;
      description = ''
        User whose home receives the `result` symlink of the VMs built in test
        mode. Required when testMode is enabled.
      '';
    };

    moduleIndexFiles = lib.mkOption {
      type        = lib.types.attrsOf lib.types.path;
      default     = { };
      example      = lib.literalExpression ''{ "index.json" = ../modules/index.json; }'';
      description = ''
        Module index files to install into the daemon cache directory
        (${cacheDir}), keyed by destination file name. Empty disables the
        install step; the daemon then falls back to fetching the index.
      '';
    };

    applyUpdatePackage = lib.mkOption {
      type        = lib.types.package;
      default     = coreUtils.mx-apply-update;
      defaultText = lib.literalMD "`mx-apply-update` from `modulix-core-utils`";
      description = "Package providing the mx-apply-update binary run at shutdown";
    };

    initPackage = lib.mkOption {
      type        = lib.types.package;
      default     = coreUtils.mx-init;
      defaultText = lib.literalMD "`mx-init` from `modulix-core-utils`";
      description = "Package providing the mx-init binary wrapped by mx-init-test";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !cfg.testMode || cfg.testUser != null;
        message   = "mx.services.modulix-daemon.testUser must be set when testMode is enabled.";
      }
    ];

    systemd.tmpfiles.rules = [
      "d ${buildQueueDir} 0755 root root -"
      "f ${skipRebuildLock} 0644 root root -"
    ] ++ lib.optionals cfg.testMode [
      "d ${vmResultDir} 0755 ${cfg.testUser} users -"
    ];

    systemd.services.modulix-daemon = {
      description   = "Modulix OS daemon";
      documentation = [ "https://github.com/Modulix-OS/modulix-daemon" ];
      after         = [ "dbus.service" "polkit.service" "systemd-tmpfiles-setup.service" ];
      requires      = [ "dbus.service" ];
      wantedBy      = [ "multi-user.target" ];

      restartIfChanged = false;
      stopIfChanged    = false;

      startLimitIntervalSec = 0;

      path = [
        config.nix.package
        pkgs.git
        pkgs.pciutils
        pkgs.cpuid
        pkgs.usbutils
        "/run/current-system/sw"
      ];

      serviceConfig = {
        Type            = "dbus";
        BusName         = "org.modulix.Daemon";
        ExecStart       = "${cfg.package}/bin/mx-daemon";
        User            = "root";
        Restart         = "always";
        RestartSec      = 5;
        KillMode        = "process";
        TimeoutStopSec  = "30min";
        StandardOutput  = "journal";
        StandardError   = "journal";
        SyslogIdentifier = "modulix-daemon";
        PrivateTmp = true;
        BindPaths = [ buildQueueDir skipRebuildLock ];
        CacheDirectory = "modulix-os";
      } // lib.optionalAttrs hasModuleIndex {
        ExecStartPre = "${installModuleIndex}";
      } // lib.optionalAttrs cfg.testMode {
        WorkingDirectory = vmResultDir;
      };

      environment = {
        RUST_LOG = if cfg.testMode then "debug" else "info";
        MX_CACHE_DIR = cacheDir;
      } // lib.optionalAttrs cfg.testMode {
        MX_DAEMON_CONFIG_DIR = "${configDir}/";
        MX_DAEMON_DRY_RUN    = "0";
      };
    };

    systemd.services.mx-apply-update = {
      description = "Apply the staged Modulix system update";
      wantedBy    = [ "shutdown.target" ];
      before      = [ "shutdown.target" "umount.target" "final.target" ];
      after       = [ "local-fs.target" ];

      unitConfig.DefaultDependencies = false;

      path = [
        config.nix.package
        pkgs.git
        "/run/current-system/sw"
      ];

      serviceConfig = {
        Type             = "oneshot";
        ExecStart        = "${cfg.applyUpdatePackage}/bin/mx-apply-update";
        User             = "root";
        TimeoutStartSec  = "15min";
        StandardOutput   = "journal";
        StandardError    = "journal";
        SyslogIdentifier = "mx-apply-update";
        CacheDirectory   = "modulix-os";
      };

      environment = {
        MX_CACHE_DIR = cacheDir;
      } // lib.optionalAttrs cfg.testMode {
        MX_DAEMON_CONFIG_DIR = "${configDir}/";
      };
    };

    services.dbus.packages = [ cfg.package ];

    environment.pathsToLink = [ "/share/polkit-1" ];

    environment.systemPackages = [ cfg.package ]
      ++ lib.optional cfg.testMode mx-init-test;

    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id === "org.modulix.daemon.install" ||
            action.id === "org.modulix.daemon.remove") {

          if (subject.isInGroup("wheel")) {
            return polkit.Result.AUTH_SELF_KEEP;
          }

          return polkit.Result.AUTH_ADMIN_KEEP;
        }
      });
    '';
  };
}

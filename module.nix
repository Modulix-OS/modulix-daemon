# module.nix
{ config, lib, pkgs, self, system, ... }:

let
  cfg = config.services.mx.daemon;
in {
  options.services.mx.daemon = {
    enable = lib.mkEnableOption "mx-daemon — system DBus daemon for Modulix";
    package = lib.mkOption {
      type        = lib.types.package;
      default     = self.packages.${system}.mx-daemon;
      description = "mx-daemon package to use.";
    };
  };

  config = lib.mkIf cfg.enable {

    # Install binary
    environment.systemPackages = [ cfg.package ];

    # Install DBus policy
    services.dbus.packages = [ cfg.package ];

    # Install Polkit Policy
    environment.pathsToLink = [ "/share/polkit-1" ];

    # Systemd service for daemon
    systemd.services.mx-daemon = {
      description    = "System Daemon (DBus monitor + package installer)";
      documentation  = [ "https://example.com" ];
      after          = [ "dbus.service" ];
      requires       = [ "dbus.service" ];
      wantedBy       = [ "multi-user.target" ];

      serviceConfig = {
        Type            = "dbus";
        BusName         = "org.modulix.Daemon";
        ExecStart       = "${cfg.package}/bin/mx-daemon";
        User            = "root";
        Restart         = "on-failure";
        RestartSec      = "5s";
        SyslogIdentifier = "mx-daemon";

        NoNewPrivileges = true;
        ProtectSystem   = false;
        ProtectHome     = true;
      };
    };
  };
}

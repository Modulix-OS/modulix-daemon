{
  description = "mx-daemon — system DBus daemon for Modulix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    flake-utils.url = "github:numtide/flake-utils";

    modulix-core-utils = {
      url = "git+file:///home/quentin/Programmes/Modulix-OS/modulix-core-utils";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, modulix-core-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        nativeBuildInputs = [
          pkgs.pkg-config
          pkgs.cmake
          pkgs.perl
        ];
        buildInputs       = [ pkgs.dbus pkgs.openssl ];

        postUnpack = ''
          cp -r --no-preserve=mode,ownership \
            ${modulix-core-utils} "$NIX_BUILD_TOP/modulix-core-utils"
        '';

        postInstall = ''
            install -Dm644 org.modulix.Daemon.conf \
            $out/share/dbus-1/system.d/org.modulix.Daemon.conf

            install -Dm644 org.modulix.daemon.policy \
            $out/share/polkit-1/actions/org.modulix.daemon.policy
        '';

        mkMxDaemon = { release }: pkgs.rustPlatform.buildRustPackage {
          pname = "mx-daemon";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          inherit nativeBuildInputs buildInputs postUnpack postInstall;
          buildType = if release then "release" else "debug";
          doCheck = false;
        };

        mx-daemon = mkMxDaemon { release = true; };
        mx-daemon-debug = mkMxDaemon { release = false; };

      in {
        packages = {
          inherit mx-daemon mx-daemon-debug;
          default = mx-daemon;
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = nativeBuildInputs ++ [
            pkgs.rustc
            pkgs.cargo
            pkgs.clippy
            pkgs.rustfmt
            pkgs.dbus
            pkgs.d-spy
          ];
        };
      }
    ) // {
    nixosModules.mx-daemon = { config, lib, pkgs, ... }:
      import ./module.nix {
        inherit config lib pkgs self;
        system = pkgs.system;
      };

    nixosModules.default = self.nixosModules.mx-daemon;
  };
}

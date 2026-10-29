{
  description = "mx-daemon — system DBus daemon for Modulix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    flake-utils.url = "github:numtide/flake-utils";

    modulix-core-utils = {
      url = "github:Modulix-OS/modulix-core-utils/v1.1.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, modulix-core-utils }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        nativeBuildInputs = [
          pkgs.pkg-config
          pkgs.cmake
          pkgs.perl
        ];
        buildInputs       = [ pkgs.dbus pkgs.openssl ];

        coreUtils =
          let
            lock = fromTOML (builtins.readFile ./Cargo.lock);
            pkg = builtins.head
              (builtins.filter (p: p.name == "modulix-core-utils") lock.package);
            source = pkg.source or (throw
              "Cargo.lock has no source for modulix-core-utils: a local [patch] to a sibling path leaked into it. Restore the git source line.");
            rev = builtins.elemAt (builtins.split "#" source) 2;
          in
          if rev == modulix-core-utils.rev then {
            inherit (pkg) version;
            narHash = modulix-core-utils.narHash;
          } else throw ''
            modulix-core-utils pin mismatch:
              Cargo.lock: ${rev}
              flake.lock: ${modulix-core-utils.rev}
            Bump the tag in both Cargo.toml and flake.nix, then run
            `cargo update -p modulix-core-utils` and `nix flake update modulix-core-utils`.
          '';

        postInstall = ''
            install -Dm644 org.modulix.Daemon.conf \
            $out/share/dbus-1/system.d/org.modulix.Daemon.conf

            install -Dm644 org.modulix.daemon.policy \
            $out/share/polkit-1/actions/org.modulix.daemon.policy
        '';

        mkMxDaemon = { release, test ? false }: pkgs.rustPlatform.buildRustPackage {
          pname = if test then "mx-daemon-test" else "mx-daemon";
          version = "0.1.0";
          src = ./.;
          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes = {
              "modulix-core-utils-${coreUtils.version}" = coreUtils.narHash;
            };
          };
          inherit buildInputs;
          nativeBuildInputs = nativeBuildInputs
            ++ nixpkgs.lib.optional test pkgs.makeWrapper;
          buildType = if release then "release" else "debug";
          doCheck = false;

          postInstall = postInstall + nixpkgs.lib.optionalString test ''
            wrapProgram $out/bin/mx-daemon \
              --run 'export MX_DAEMON_CONFIG_DIR="''${MX_DAEMON_CONFIG_DIR:-$HOME/modulix-test-config}"' \
              --set-default MX_DAEMON_DRY_RUN 0
          '';
        };

        mx-daemon = mkMxDaemon { release = true; };
        mx-daemon-debug = mkMxDaemon { release = false; };
        mx-daemon-test = mkMxDaemon { release = false; test = true; };

        push-cache = import ./nix/attic-push.nix {
          inherit pkgs;
          attrs = [ "mx-daemon" ];
          flakeRef = "path:${self}";
          tokenFile =
            if builtins.pathExists ./secrets/attic-token.age
            then ./secrets/attic-token.age
            else null;
        };

      in {
        packages = {
          inherit mx-daemon mx-daemon-debug mx-daemon-test push-cache;
          default = mx-daemon;
        };

        apps.push-cache = {
          type = "app";
          program = "${push-cache}/bin/mx-push-cache";
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
        system = pkgs.stdenv.hostPlatform.system;
      };

    nixosModules.default = self.nixosModules.mx-daemon;
  };
}

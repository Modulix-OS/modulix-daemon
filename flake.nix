{
  description = "mx-daemon — system DBus daemon for Modulix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    naersk = {
      url = "github:nix-community/naersk";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, naersk, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        naersk' = pkgs.callPackage naersk {};

        nativeBuildInputs = [ pkgs.pkg-config ];
        # openssl: pulled in transitively by modulix-core-utils (reqwest / native-tls).
        buildInputs       = [ pkgs.dbus pkgs.openssl ];

        # NOTE: modulix-core-utils is a `path = "../modulix-core-utils"` cargo
        # dependency. `cargo build` inside a checkout that has the sibling crate
        # works as-is. For `nix build`, naersk only copies `src = ./.` into the
        # sandbox, so core-utils must additionally be vendored (add it as a flake
        # input and include its source), mirroring gnome-software-plugin/backend.

        postInstall = ''
            install -Dm644 org.modulix.Daemon.conf \
            $out/share/dbus-1/system.d/org.modulix.Daemon.conf

            install -Dm644 org.modulix.daemon.policy \
            $out/share/polkit-1/actions/org.modulix.daemon.policy
        '';

        mx-daemon = naersk'.buildPackage {
          pname = "mx-daemon";
          src   = ./.;
          inherit nativeBuildInputs buildInputs postInstall;
          release = true;


        };

        mx-daemon-debug = naersk'.buildPackage {
          pname = "mx-daemon";
          src   = ./.;
          inherit nativeBuildInputs buildInputs postInstall;
          release = false;
        };

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

{
  description = "linehash — JSONL line-hash file tool for AI agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ]
      (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          version = "0.1.0";
        in {
          packages.default = pkgs.rustPlatform.buildRustPackage {
            pname = "linehash";
            inherit version;
            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.openssl ];

            meta = {
              description = "JSONL line-hash file tool for AI agents";
              homepage = "https://github.com/RogerNavelsaker/linehash";
              license = pkgs.lib.licenses.mit;
              mainProgram = "linehash";
            };
          };

          devShells.default = pkgs.mkShell {
            buildInputs = [ pkgs.rustc pkgs.cargo pkgs.pkg-config pkgs.openssl pkgs.gcc ];
            # Keep compiler selection reproducible with the pinned Nixpkgs toolchain.
            CC = "${pkgs.gcc}/bin/gcc";
            CXX = "${pkgs.gcc}/bin/g++";
            RUSTFLAGS = "-C target-cpu=native";
            shellHook = ''
              echo "linehash dev shell"
              echo "  cargo build --release"
              echo "  cargo test"
            '';
          };
        });
}

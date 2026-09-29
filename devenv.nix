{ pkgs, ... }:
{
  name = "linehash";

  packages = with pkgs; [
    cargo
    gcc
    openssl
    pkg-config
    rustc
  ];

  env.CC = "${pkgs.gcc}/bin/gcc";
  env.CXX = "${pkgs.gcc}/bin/g++";
  env.RUSTFLAGS = "-C target-cpu=native";

  enterShell = ''
    echo "linehash devenv active"
    echo "  cargo build --release"
    echo "  cargo test"
  '';
}

{ pkgs ? import <nixpkgs> { } }:

with pkgs;

mkShell {
  buildInputs = [ pkgs.bash pkgs.pkg-config pkgs.openssl pkgs.clang pkgs.lldb ];
}

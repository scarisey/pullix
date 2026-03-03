{
  pkgs,
  rustToolchain,
  ...
}: let
  # Build dependencies for git2-rs
  buildInputs = with pkgs; [
    openssl
    pkg-config
    libgit2
    libssh2
    zlib
  ];

  nativeBuildInputs = with pkgs; [
    pkg-config
    rustToolchain
  ];
in
  pkgs.rustPlatform.buildRustPackage {
    pname = "pullix";
    version = "0.1.0";
    src = ../.;

    cargoLock = {
      lockFile = ../Cargo.lock;
    };

    inherit buildInputs nativeBuildInputs;

    # Set environment variables for native dependencies
    OPENSSL_NO_VENDOR = 1;
    PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";

    meta = with pkgs.lib; {
      description = "NixOS deployment automation tool";
      homepage = "https://github.com/scarisey/pullix";
      license = licenses.mit;
      maintainers = [];
    };
  }

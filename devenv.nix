{
  pkgs,
  lib,
  ...
}: {
  packages = with pkgs; [
    openssl
    openssh_gssapi
    pkg-config
    libgit2
    libssh2
    zlib
    docker-client
    direnv
  ];

  languages.rust = {
    enable = true;
    channel = "nightly";
    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      "rust-analyzer"
    ];
  };

  env.OPENSSL_NO_VENDOR = "1";
  env.LIBGIT2_NO_VENDOR = "1";
  env.LIBSSH2_SYS_USE_PKG_CONFIG = "1";
  env.PKG_CONFIG_PATH = lib.makeSearchPath "lib/pkgconfig" [
    pkgs.openssl.dev
    pkgs.libgit2.dev
    pkgs.libssh2
    pkgs.zlib.dev
  ];
  env.LIBRARY_PATH = lib.makeLibraryPath [
    pkgs.openssl
    pkgs.libgit2
    pkgs.libssh2
    pkgs.zlib
  ];
  env.RUST_LOG = "info";

  enterShell = ''
    echo "✓ Rust development environment ready"
  '';

  dotenv.disableHint = true;
}

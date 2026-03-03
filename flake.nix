{
  description = "Pullix - NixOS deployment automation tool";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.nightly.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        };

        pullix = pkgs.callPackage ./nix/package.nix {
          inherit rustToolchain;
        };
      in
      {
        packages = {
          inherit pullix;
          default = pullix;
        };

        formatter = pkgs.alejandra;
      }
    )
    // {
      # NixOS module for running pullix as a service
      nixosModules.default = import ./nix/nixos-module.nix { inherit self; };
    };

  nixConfig = {
    extra-substituters = [
      "https://nix-community.cachix.org?priority=31"
      "https://scarisey-public.cachix.org?priority=32"
    ];
    extra-trusted-public-keys = [
      "scarisey-public.cachix.org-1:kabqlCM0Wwd3iOh+C62WQg7vUO7vX3JbKKraSmxr2n8="
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };
}

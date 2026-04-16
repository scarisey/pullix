{
  description = "Pullix - NixOS deployment automation tool";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    devenv.url = "github:cachix/devenv/v1.11.1";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    devenv,
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
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
      in {
        packages = {
          inherit pullix;
          default = pullix;
          devenv = devenv.packages."${system}".default;
        };

        formatter = pkgs.alejandra;
      }
    )
    // {
      nixosModules.default = import ./nix/nixos-module.nix {inherit self;};
      homeManagerModules.default = import ./nix/hm-module.nix {inherit self;};
    };

  nixConfig = {
    extra-substituters = [
      "https://nix-community.cachix.org?priority=31"
      "https://scarisey-public.cachix.org?priority=32"
      "https://devenv.cachix.org?priority=33"
      "https://cachix.cachix.org?priority=34"
    ];
    extra-trusted-public-keys = [
      "scarisey-public.cachix.org-1:kabqlCM0Wwd3iOh+C62WQg7vUO7vX3JbKKraSmxr2n8="
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
      "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw="
      "cachix.cachix.org-1:eWNHQldwUO7G2VkjpnjDbWwy4KQ/HNxht7H4SSoMckM="
    ];
  };
}

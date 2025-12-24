{
  description = "Continuous review system for codebases";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config.allowUnfree = true;
        };
        
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" ];
        };
        
        panopticon-server = pkgs.rustPlatform.buildRustPackage {
          pname = "panopticon-server";
          version = "0.1.0";

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = with pkgs; [
            pkg-config
          ];

          buildInputs = with pkgs; [
            openssl
            libiconv
          ];

          # Pass git revision to the build
          PANOPTICON_GIT_HASH = if (self ? rev) && (self.rev != null) then self.rev else "dirty";

          # Build only the server binary
          buildAndTestSubdir = "panopticon-server";

          meta = with pkgs.lib; {
            description = "Panopticon continuous code review server";
            homepage = "https://github.com/Smaug123/panopticon";
            license = licenses.mit;
            maintainers = [ ];
          };
        };
      in
      {
        packages = {
          default = panopticon-server;
          panopticon-server = panopticon-server;
        };
        
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.cargo
            pkgs.pkg-config
            pkgs.openssl
            pkgs.libiconv
            pkgs.claude-code
            pkgs.codex
          ];

          RUST_BACKTRACE = "1";
        };
      });
}

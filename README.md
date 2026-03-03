# Pullix - pull based deployment tool for NixOS flakes

This is actually a toy project for me to learn Rust. I also wanted a deployment tool not owned by anybody else.

## How to use in your NixOS flake

```flake.nix
{
  inputs = {
    pullix.url = "github:scarisey/pullix";
  };
}
```

Add the nixos module in the imports, for example:

```configuration.nix
{...}:{
  imports = [
    ./hardware.nix
    inputs.pullix.nixosModules.default
  ];
}
```
Then configure the service:

```configuration.nix
{...}:{
  services.pullix = {
    enable = true;
    hostname = "foo";
    pollIntervalSecs = 10;
    flakeRepo = {
      type = "GitHub";
      repo = "scarisey/nixos-dotfiles";
      prodSpec = {
        ref = "prod";
      };
      testSpec = {
        ref = "test";
      };
    };
    environmentFile = config.sops.secrets."nix_config_env".path;
    verbose_logs = true;
  };
}
```

## What Pullix does

Pullix is a pull-based deployment daemon that periodically polls a remote Git repository and deploys NixOS configurations automatically. Here is how it works:

1. **Polling**: At a configurable interval, Pullix fetches all refs from the remote repository.
2. **Commit resolution**: It resolves two configured refs — a **test** ref and a **prod** ref (branches or tags) — to their latest commit SHAs, and computes the distance (number of commits) between them.
3. **Deployment decision**: Based on the commit distance and deployment history:
   - If **test is ahead** of prod, the test commit is deployed (if not already seen).
   - If **prod is ahead** of or equal to test, the prod commit is deployed (if not already seen).
   - If only one ref exists, that ref is deployed.
   - Previously **failed commits are not retried** unless a new commit appears on that ref.
4. **NixOS activation**:
   - *Test* deployments run `nix build` then `switch-to-configuration test` — the configuration is activated but **not** added as the boot default.
   - *Prod* deployments run `nix build` then `switch-to-configuration switch` — the configuration is activated **and** set as the boot default.
5. **State persistence**: Deployment history (successes and failures) is saved to a local JSON file, ensuring Pullix never redeploys an already-processed commit across restarts.

## Module options reference

### `services.pullix.enable`

- **Type:** `bool`
- **Default:** `false`
- **Description:** Whether to enable the Pullix deployment service.

### `services.pullix.flakeRepo`

- **Type:** attribute set (submodule)
- **Description:** Flake reference configuration for the repository to deploy from.

#### `services.pullix.flakeRepo.type`

- **Type:** one of `"Git"`, `"GitHttp"`, `"GitHttps"`, `"GitSsh"`, `"GitFile"`, `"GitHub"`, `"GitLab"`, `"SourceHut"`, `"Mercurial"`, `"Tarball"`, `"File"`, `"Path"`, `"Indirect"`
- **Required**
- **Description:** Type of flake reference.

#### `services.pullix.flakeRepo.repo`

- **Type:** `str`
- **Required**
- **Description:** Repository URL or identifier (e.g. `"owner/repo"` for GitHub/GitLab/SourceHut, or a full URL for Git types).

#### `services.pullix.flakeRepo.host`

- **Type:** `null` or `str`
- **Default:** `null`
- **Description:** Custom host for self-hosted git servers (e.g. a self-hosted GitLab instance).

#### `services.pullix.flakeRepo.prodSpec`

- **Type:** `null` or attribute set (submodule)
- **Default:** `null`
- **Description:** Config for prod deployment (`switch-to-configuration switch`). When the prod ref is ahead of or equal to the test ref, Pullix activates the configuration and sets it as the boot default.

##### `services.pullix.flakeRepo.prodSpec.ref`

- **Type:** `null` or `str`
- **Default:** `null`
- **Description:** Git reference (branch name) to track for prod.

##### `services.pullix.flakeRepo.prodSpec.rev`

- **Type:** `null` or `str`
- **Default:** `null`
- **Description:** Git revision (commit SHA or tag) to pin for prod.

#### `services.pullix.flakeRepo.testSpec`

- **Type:** `null` or attribute set (submodule)
- **Default:** `null`
- **Description:** Config for test deployment (`switch-to-configuration test`). When the test ref is ahead of prod, Pullix activates the configuration without making it the boot default.

##### `services.pullix.flakeRepo.testSpec.ref`

- **Type:** `null` or `str`
- **Default:** `null`
- **Description:** Git reference (branch name) to track for test.

##### `services.pullix.flakeRepo.testSpec.rev`

- **Type:** `null` or `str`
- **Default:** `null`
- **Description:** Git revision (commit SHA or tag) to pin for test.

### `services.pullix.pollIntervalSecs`

- **Type:** `int`
- **Default:** `60`
- **Description:** Polling interval in seconds. Pullix will fetch the remote repository and evaluate whether a deployment is needed at this interval.

### `services.pullix.appDir`

- **Type:** `str`
- **Default:** `"/var/lib/pullix"`
- **Description:** Directory for Pullix state files. A `state.json` file tracking deployment history is stored here. The directory is automatically created via `systemd.tmpfiles`.

### `services.pullix.hostname`

- **Type:** `str`
- **Default:** `config.networking.hostName`
- **Description:** Hostname used for the `nixosConfigurations.<hostname>` lookup in the flake. This determines which NixOS configuration from the flake is built and deployed.

### `services.pullix.environmentFile`

- **Type:** `null` or `path`
- **Default:** `null`
- **Description:** Path to an additional environment file to source for the Pullix systemd service. Useful for passing secrets such as Nix access tokens without storing them in the Nix store. Example content:

  ```
  NIX_CONFIG=access-tokens = github.com=ghp_xxx
  ```

### `services.pullix.prometeheusExporterEndpoint`

- **Type:** `null` or `str`
- **Default:** `null`
- **Description:** If set, enables a Prometheus metrics exporter at the given endpoint. When Prometheus is also enabled on the host, a scrape config for Pullix is automatically added.

### `services.pullix.privateKey`

- **Type:** `null` or attribute set (submodule)
- **Default:** `null`
- **Description:** SSH private key configuration for accessing private Git repositories over SSH.

#### `services.pullix.privateKey.path`

- **Type:** `path`
- **Required**
- **Description:** Path to the SSH private key file.

#### `services.pullix.privateKey.passphrase_path`

- **Type:** `path`
- **Required**
- **Description:** Path to a file containing the passphrase for the private key.

### `services.pullix.verbose_logs`

- **Type:** `bool`
- **Default:** `false`
- **Description:** When enabled, sets `RUST_LOG=DEBUG` in the service environment, making logs very verbose. Useful for troubleshooting deployment issues.

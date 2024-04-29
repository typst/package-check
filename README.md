# Typst package check

A tool to report common errors in Typst packages.

This tool can be used in three ways:

- `typst-package-check check`, to check a single package, in the current directory.
- `typst-package-check check @preview/NAME:VERSION` to check a given package in a clone of the `typst/packages` repository.
  This command should be run from the `packages` sub-directory. In that configuration, imports will be resolved in the local
  clone of the repository, nothing will be fetched from the network.
- `typst-package-check server` to start a HTTP server that listen for GitHub webhooks, and run checks when a PR is opened against
  `typst/packages` (or any repository with a similar structure).

## Using this tool

You can install this tool with Cargo:

```bash
cargo install --git https://github.com/typst/package-check.git
cd my-package
typst-package-check check
```

You can also run it with Nix:

```bash
nix run github:typst/package-check -- check
```

Finally a Docker image is available:

```bash
docker run -v .:/data ghcr.io/typst/package-check check
```

When running with Docker, `/data` is the directory in which the tool will look for files to check.

## Configuring the webhook handler

The following environment variables are used for configuration.
They are all mandatory.
`.env` is supported.

- `PACKAGES_DIR`, path to a local clone of `typst/packages`
- `GITHUB_APP_IDENTIFIER`, the ID of the GitHub app submitting reviews.
  This app should have the `checks:write` permission.
- `GITHUB_WEBHOOK_SECRET`, the secret provided by GitHub when enabling webhook handling.
- `GITHUB_PRIVATE_KEY`, the private key of the GitHub app, in PEM format.
  Directly in the environment variable, not a path to an external file.
  Note that you can (and should probably) use double-quotes in the `.env` file for multi-line variables.

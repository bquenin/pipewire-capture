# Release Process

## Steps

1. **Create a version bump PR**
   - Update `version` in `Cargo.toml`
   - Run `cargo check` to update `Cargo.lock`
   - Commit and open a PR

2. **Merge the PR**
   - Ensure CI passes (lint, build, test-python)
   - Squash merge into `main`

3. **Create a GitHub Release**
   - Go to [Releases](https://github.com/bquenin/pipewire-capture/releases/new)
   - Create a new tag matching the version (e.g., `v0.2.9`)
   - Target: `main`
   - Write release notes describing the changes
   - Publish the release

4. **Automated publishing**
   - The `publish.yml` workflow triggers on release publication
   - Builds manylinux wheels for x86_64 and aarch64
   - Tests that wheels don't bundle libpipewire
   - Publishes to [PyPI](https://pypi.org/project/pipewire-capture/) via trusted publishing (OIDC)

## Version Scheme

Follows [Semantic Versioning](https://semver.org/). The version in `Cargo.toml` is the single source of truth — `pyproject.toml` reads it dynamically via maturin.

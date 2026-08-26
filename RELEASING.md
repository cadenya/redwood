# Releasing Redwood

Redwood releases are automated. Do not create release tags manually during the
normal release flow.

1. Merge conventional commits into `main`.
2. Review and merge the Release Please pull request.
3. Release Please updates `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md`, then
   creates the matching `vX.Y.Z` tag and GitHub release.
4. The tag starts the Publish Release workflow. GoReleaser cross-compiles the
   `redwood` binary, signs and notarizes macOS artifacts, publishes archives,
   checksums and Linux packages, and updates `cadenya/homebrew-tools`.
5. Verify the GitHub release and run:

   ```sh
   brew update
   brew install cadenya/tools/redwood
   redwood --version
   ```

The `production` GitHub environment must provide:

- `MACOS_SIGN_P12`
- `MACOS_SIGN_PASSWORD`
- `MACOS_NOTARY_ISSUER_ID`
- `MACOS_NOTARY_KEY_ID`
- `MACOS_NOTARY_KEY`
- `HOMEBREW_TAP_GITHUB_TOKEN`

The repository also requires `RELEASE_PLEASE_TOKEN`. Signing credentials are
mandatory: the publishing workflow fails before GoReleaser runs if any are
missing.

For a local packaging check without publishing:

```sh
goreleaser release --snapshot --clean --skip=publish \
  --config .github/goreleaser.yaml
```

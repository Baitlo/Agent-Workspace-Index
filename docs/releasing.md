# Releasing AWI

AWI publishes native GNU/Linux binaries for x86_64 and arm64. Both artifacts
must be compiled and tested on matching GitHub-hosted hardware:

| Target | GitHub runner |
|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` |

The release workflow does not cross-compile arm64. Each native job runs
formatting, Clippy, Rust tests, semantic-sidecar helper tests, and a release
build before packaging. GitHub then creates provenance attestations and one
`SHA256SUMS` file.

## Release Checklist

1. Ensure `Cargo.toml` contains the intended version.
2. Merge to `main` and wait for both native CI jobs to pass.
3. Create an annotated `v<version>` tag at that tested commit.
4. Push the tag. `.github/workflows/release.yml` validates that the tag matches
   the Cargo package version and publishes the GitHub Release.
5. Verify both archives, `SHA256SUMS`, and attestations on the release page.
6. Download each archive and run `awi --version` on matching hardware.

Example:

```bash
git tag -a v0.1.0 -m "AWI v0.1.0"
git push origin v0.1.0
```

Release assets intentionally do not include Python, LanceDB,
`llama-cpp-python`, or GGUF model weights. Those remain optional semantic
retrieval dependencies.

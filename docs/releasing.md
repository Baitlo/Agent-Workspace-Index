# Releasing AWI

AWI publishes native GNU/Linux binaries for x86_64 and arm64, plus native
macOS binaries for Apple Silicon and Intel. All artifacts must be compiled
and tested on matching GitHub-hosted hardware:

| Target | GitHub runner |
|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` |
| `aarch64-apple-darwin` | `macos-15` |
| `x86_64-apple-darwin` | `macos-15-intel` |

The workflows do not cross-compile arm64. CI runs the complete Clippy and Rust
test suite on x86_64. The other jobs perform a native release build plus
reconcile/search smoke tests on the resulting binary. The release workflow
repeats a release-profile test on x86_64 and a native release build on the
remaining targets before packaging. All jobs run the semantic-sidecar helper
tests. GitHub then creates provenance attestations and one `SHA256SUMS` file.

## Release Checklist

1. Ensure `Cargo.toml` contains the intended version.
2. Merge to `main` and wait for both native CI jobs to pass.
3. Create an annotated `v<version>` tag at that tested commit.
4. Push the tag. `.github/workflows/release.yml` validates that the tag matches
   the Cargo package version and publishes the GitHub Release.
5. Verify every target archive, `SHA256SUMS`, and attestations on the release page.
6. Download each archive and run `awi --version` on matching hardware.
7. Run the public `install-release.sh --workspace <fixture>` path and verify the
   binary, local index, managed top-level `AGENTS.md` block, and client
   integration report.

Example:

```bash
git tag -a v0.2.0 -m "AWI v0.2.0"
git push origin v0.2.0
```

Release assets intentionally do not include Python, LanceDB,
`llama-cpp-python`, or GGUF model weights. Those remain optional semantic
retrieval dependencies.

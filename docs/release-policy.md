# Release Policy

RedDB releases are tag-driven. Stable releases are built from immutable
`vX.Y.Z` tags and all official artifacts for that version should come from the
same commit.

## Channels

| Channel | Source | Intended use |
| --- | --- | --- |
| Stable (`vX.Y.Z`) | Immutable Git tag | Production and normal upgrades |
| Release candidate (`vX.Y.Z-rc.N`) | Latest eligible `main` commit | Installer, packaging, and integration testing |
| `next` | CI prerelease channel | Early validation only |

Release candidates and `next` builds are not production support lines.

## Versioning

RedDB uses SemVer for the software version and calls out database-specific
compatibility in release notes.

- Patch releases should not intentionally change storage format, wire protocol,
  or public API behavior.
- Minor releases may add storage features, protocol capabilities, APIs, and
  drivers. Existing supported clients should remain compatible unless the
  release notes say otherwise.
- Major releases may include breaking storage, protocol, API, or operational
  changes.

Any release that changes storage format, wire protocol compatibility, or upgrade
requirements must include explicit upgrade notes in the GitHub Release body.

## Artifact Contract

Stable GitHub Releases publish:

- `red-*` server binaries.
- `red_client-*` thin-client binaries.
- Per-asset `.sha256` files for compatibility.
- `checksums.txt` for automatic installers.
- `SHA256SUMS` for standard manual verification.
- `artifact-sizes.md` as release-gate evidence.

Official JavaScript package publication is gated on the GitHub Release carrying
the postinstall-required binary assets and checksum manifests. Release artifacts
are attested with GitHub Artifact Attestations from the aggregate checksum
manifest.

## Container Tags

Stable GHCR images publish immutable and moving tags:

- `vX.Y.Z` and `X.Y.Z`: immutable release tags.
- `X.Y`: latest patch in the minor line.
- `X`: latest minor in the major line.
- `latest`: latest stable release.

Prerelease images publish `next`.

Server and thin-client images are multi-arch for `linux/amd64` and
`linux/arm64`, with Cosign keyless signatures, BuildKit provenance, and SBOM
attestations.

## Verification

Manual binary verification:

```bash
curl -fsSLO https://github.com/reddb-io/reddb/releases/download/vX.Y.Z/red-linux-x86_64
curl -fsSL https://github.com/reddb-io/reddb/releases/download/vX.Y.Z/SHA256SUMS -o SHA256SUMS
grep -E '  red-linux-x86_64$' SHA256SUMS | sha256sum -c -
gh attestation verify red-linux-x86_64 --repo reddb-io/reddb
```

Container inspection:

```bash
docker buildx imagetools inspect ghcr.io/reddb-io/reddb:vX.Y.Z
docker buildx imagetools inspect ghcr.io/reddb-io/reddb-client:vX.Y.Z
cosign verify ghcr.io/reddb-io/reddb:vX.Y.Z \
  --certificate-identity "https://github.com/reddb-io/reddb/.github/workflows/release.yml@refs/tags/vX.Y.Z" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

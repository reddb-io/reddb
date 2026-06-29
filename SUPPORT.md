# Support Policy

RedDB support follows the public release stream. Use GitHub issues for bugs,
regressions, packaging failures, and compatibility reports. Report security
issues privately through `SECURITY.md`.

## Supported Versions

| Version line | Support level |
| --- | --- |
| Current stable minor | Bug fixes, packaging fixes, and security fixes |
| Previous stable minor | Critical security and data-loss fixes when feasible |
| Older stable minors | Unsupported; upgrade to a supported line |
| Release candidates / `next` | Testing only; no production support promise |

Patch releases are the preferred vehicle for fixes that do not require a
storage-format, wire-protocol, or public API change. Minor releases may add
capabilities and compatibility surfaces. Breaking changes require a major
release unless a release note explicitly calls out a narrower migration.

## Compatibility Reports

Include:

- RedDB version and install channel.
- Server/client versions when they differ.
- OS, architecture, and container digest when relevant.
- Storage mode and transport used.
- Minimal reproduction, logs, and the exact query or command.

## Packaging Reports

For installer or release-asset failures, include:

- Release tag.
- Asset name or package name.
- `SHA256SUMS` / `checksums.txt` verification output.
- `gh attestation verify <asset> --repo reddb-io/reddb` output when applicable.

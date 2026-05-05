# Update release pipeline to ship red_client alongside red [AFK]

GitHub issue: reddb-io/reddb#64
Parent PRD: reddb-io/reddb#54
Blocked by: #60, #62

Update `.github/workflows/release.yml` (and packaging scripts / Dockerfiles / homebrew taps) so every release produces `red_client` artifacts on the same platforms as `red`: Linux x86_64+aarch64, macOS x86_64+aarch64, Windows x86_64. Same naming convention, checksum, signing. Container image strategy chosen + implemented. Release notes template lists `red_client`.

## Acceptance Criteria
- [ ] Release workflow builds `red_client` for every platform `red` builds for
- [ ] Artifact names parallel `red` artifacts
- [ ] Checksums + signing cover `red_client`
- [ ] Container image strategy documented + implemented
- [ ] Package-manager publishing carries `red_client`
- [ ] Release notes template lists `red_client`
- [ ] Workflow runs green on dry-run / pre-release tag
- [ ] Binary-size guard (#62) runs against published artifact

## Feedback Loops
- Dry-run release workflow on test tag
- `gh workflow run release.yml`

# Container image strategy for red_client [HITL]

GitHub: reddb-io/reddb#68
Parent: #54

Pick: separate `red_client` image (~5 MB), or carry red_client inside main red image, or both. HITL — needs ops decision.

## Acceptance Criteria
- [ ] Decision recorded (ADR or release note)
- [ ] Image build wired into release.yml with parallel tag scheme
- [ ] README / install guide updated
- [ ] If separate image: < 10 MB

## Feedback Loops
- Release workflow dry-run

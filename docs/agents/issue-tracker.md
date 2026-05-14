# Issue Tracker

This repository tracks work in GitHub Issues for `reddb-io/reddb`.

Use the GitHub CLI from the repository root:

```bash
gh issue create --repo reddb-io/reddb
gh issue view <number> --repo reddb-io/reddb
gh issue list --repo reddb-io/reddb
```

New PRDs and implementation slices should be opened as GitHub issues and labelled `needs-triage` so they enter the normal triage flow. When splitting a PRD into implementation issues, publish blockers first so dependent issues can link to concrete issue numbers.

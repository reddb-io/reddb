---
"@reddb-io/cli": minor
---

Cluster bootstrap & operational telemetry groundwork.

- **Cluster bootstrap authority**: fail-closed seam for cluster-shaped auth bootstrap (#1229), real auth-store wiring for cluster vault first boot (#1231), write-if-absent initial config on fenced bootstrap manifest apply (#1232), bootstrap-completion marker observed at boot through the authority seam (#1230), and cloud policy-first bootstrap manifest protections (#1233).
- **Helm/Compose**: cluster bootstrap contract documented and render-checked; cluster members carry no bootstrap credentials, gated cluster auth/vault path with fail-closed messaging (#1234), plus duplicate/concurrent bootstrap drills (#1235).
- **Durability**: collision-proof WAL backup temp paths so segment digests stay honest (#1294).
- **Operational telemetry**: Phase-0 substrate contract (ADR 0060) defining the store/read-model boundary, retention/cardinality budgets, and redaction rules ahead of the metric slices (#1247).
- **Process**: binding merge gate + green ratchet on `main` (ADR 0059, #975).

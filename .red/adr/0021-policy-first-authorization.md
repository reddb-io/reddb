# Policy-first authorization

RedDB authorization is based on users plus policies: a user is the only login principal type, and high privilege comes from attached policies rather than a separate owner/superuser class. `Admin` is a conventional bootstrap shape, not an authorization bypass; explicit Deny statements must still win over broad allow policies so managed policies, managed config namespaces, and system-owned users can be protected consistently.

Bootstrap presets may install an allow-all policy for the initial admin and a protect-managed policy for operator-owned guardrails. Managed policies are self-described by metadata and pinned by an internal integrity registry so a caller cannot unlock them by rewriting the policy document itself. This trades away the simplicity of a legacy `Role::Admin` bypass in favor of one authorization model that is customizable, auditable, and safe for managed bootstrap scenarios.

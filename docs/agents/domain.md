# Domain Docs

This repository uses a single-context domain layout.

Before producing PRDs, issues, architecture analysis, or implementation plans, read:

- `CONTEXT.md` for canonical RedDB vocabulary.
- `docs/adr/` for architectural decisions relevant to the touched area.

Important domain terms include `Collection`, `CollectionDescriptor`, `Statement frame`, `Wire adapter`, `KV`, `Config`, `Vault`, `AskPipeline`, `Result cache`, and `AggregateQueryPlanner`.

For database-engine work, correctness is more important than convenience. Pay particular attention to transaction semantics, WAL behavior, persistence ordering, locking, tenancy, RLS, and wire compatibility.

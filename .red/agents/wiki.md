# Wiki — Schema

This document teaches the agent how to maintain this repo's LLM Wiki. It is read every time `/wiki` operates.

## Domain

**What this wiki is about:** external engineering-practice references (style guides, design docs, papers) studied for ideas to adopt into reddb.

**Accepted source types:** web articles (URL fetch), PDFs (papers, books), personal notes (markdown drop).

**Voice:** solo — first person, no "we"/"the team".

## Layout

```
.red/wiki/
├── raw/                # immutable sources — agent reads, never edits
│   └── assets/         # images downloaded from online sources
├── pages/              # agent-generated pages — flat, kebab-case.md
├── index.md            # catalogue grouped by type
└── log.md              # append-only "## [date] op | title"
```

The entire `.red/wiki/` directory is in `.gitignore`. **Never** commit it.

## Page conventions

**Filename:** `kebab-case.md`. Display name from frontmatter `title:`.

**Mandatory frontmatter:**

```yaml
---
title: ...
type: entity                # entity | concept | source | synthesis | comparison
tags: [...]
created: YYYY-MM-DD
updated: YYYY-MM-DD
sources: [slug, ...]
---
```

**Cross-links:** standard markdown `[X](./x.md)`. No Obsidian wikilinks.

## Operations

- **Ingest** — URL/file → `raw/<slug>.md`; read; discuss takeaways; write `pages/<slug>.md` (`type: source`) + entity/concept pages; flag contradictions; update `index.md`; append `log.md`.
- **Query** — read `index.md`, drill into pages, synthesise with citations `(via [page](./pages/page.md))`; optionally file back as `type: synthesis`.
- **Lint** — contradictions, stale, orphans, stubs, unpaged concepts, gaps. Reports; does not auto-fix.

## Anti-patterns

- ❌ Don't treat the wiki as a spec or changelog — it's knowledge accumulation.
- ❌ Don't commit `.red/wiki/`.
- ❌ Don't edit `raw/` — sources are immutable.
- ❌ Don't create Obsidian wikilinks `[[...]]`.

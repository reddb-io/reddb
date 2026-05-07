# Python SDK: cache.* methods in drivers/python [AFK]

GitHub: reddb-io/reddb#197
Parent: #188

Additive — only touch drivers/python/. Mirror JS/TS API surface in idiomatic Python.

```python
client.cache.get(namespace: str, key: str) -> bytes | None
client.cache.put(namespace, key, value, *, ttl_ms=None, ...) -> None
client.cache.exists(namespace, key) -> Literal['present','absent','maybe']
client.cache.invalidate(namespace, key) -> None
client.cache.invalidate_prefix(namespace, prefix) -> int
client.cache.invalidate_tags(namespace, tags: list[str]) -> int
client.cache.flush_namespace(namespace) -> None
```

Typed via TypedDict / dataclass for opts.

## Acceptance Criteria

- [ ] drivers/python exposes client.cache.* API.
- [ ] Round-trip integration test against local server.
- [ ] Mock-server tests.
- [ ] mypy / pyright clean on new types.
- [ ] pytest passes.

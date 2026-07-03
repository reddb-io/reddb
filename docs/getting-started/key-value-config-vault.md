# KV, Config, and Vault Quickstart

Use this when the Collection is a lookup map, runtime configuration namespace,
or secret namespace. The Collection is the universal container; KV/config/vault
commands are the semantic layer.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
KV PUT app_settings.'feature.checkout' = 'enabled';
KV GET app_settings.'feature.checkout';
SET CONFIG red.demo.mode = 'quickstart';
SHOW CONFIG red.demo.mode;
SET SECRET demo.api_key = 'sk_test_local';
SHOW SECRET demo.api_key;
```

First meaningful result: the KV read returns the feature flag, the config read
returns the persistent setting, and the secret read returns vault metadata
without exposing the secret value.

Where to go next: [Key-Value](/data-models/key-value.md),
[Configuration](/getting-started/configuration.md), and [Vault](/security/vault.md).

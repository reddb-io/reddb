# Secret & Password Types

RedDB has two security-native column types for storing sensitive values so
they never sit in plaintext at rest. Both are typed literal constructors on
`INSERT`: the parser recognises `SECRET('…')` and `PASSWORD('…')` and hands
the plaintext to the executor, which applies the crypto transform before the
value is written.

| Type       | At rest                                   | Insert with        | Read back                          |
| ---------- | ----------------------------------------- | ------------------ | ---------------------------------- |
| `Secret`   | AES-256-GCM ciphertext                    | `SECRET('value')`  | decrypt/secret policy (admin) |
| `Password` | argon2id hash                             | `PASSWORD('plain')`| never — verify instead             |

## Secret

`Secret` stores an AES-256-GCM ciphertext keyed by the vault's master AES key.
Use it for API keys, tokens, and other credentials your service must be able
to read back in full.

```sql
CREATE TABLE integrations (
  id      Uuid NOT NULL,
  name    Text NOT NULL,
  api_key Secret
)
```

Insert plaintext with the `SECRET(...)` constructor. The value is encrypted at
write time; the plaintext never touches storage.

```sql
INSERT INTO integrations VALUES ('...', 'stripe', SECRET('sk_live_...'))
```

Reads require a decrypt/secret policy — for now, admin. When the vault is
sealed, reads return `***` instead of the ciphertext or plaintext; the
plaintext is only returned to an authorised reader while the vault is
unsealed.

```rust
Value::Secret(vec![/* AES-256-GCM ciphertext bytes */])
```

## Password

`Password` stores an argon2id hash. It is a write-and-verify type: the
plaintext is hashed on insert and is **never** round-tripped back to a client.
Use it for user login credentials.

```sql
CREATE TABLE users (
  id       Uuid PRIMARY KEY,
  email    Text,
  password Password
)
```

Insert with the `PASSWORD(...)` constructor, which hashes the plaintext with
argon2id before storage.

```sql
INSERT INTO users VALUES ('...', 'alice@example.com', PASSWORD('hunter2'))
```

```rust
Value::Password("$argon2id$v=19$...".to_string())
```

### VERIFY_PASSWORD

Because the hash cannot be reversed, you compare a candidate against a stored
`Password` column with the `VERIFY_PASSWORD` function rather than an equality
check. It takes the `Password` column and a candidate `Text` value and returns
a boolean.

```
VERIFY_PASSWORD(password_column, 'candidate')
```

Authenticate a user by filtering on the verification result:

```sql
SELECT id, email FROM users WHERE VERIFY_PASSWORD(password, 'hunter2')
```

Selecting the `password` column directly does not expose the hash as plaintext
— it is redacted to `***`.

## Worked Example

A `users` table combining both types — an argon2id-hashed login password and
an encrypted API key:

```sql
CREATE COLLECTION users (
  id       Uuid PRIMARY KEY,
  email    Text,
  password Password,
  api_key  Secret
)
```

```sql
INSERT INTO users VALUES ('...', 'alice@example.com', PASSWORD('hunter2'), SECRET('sk_live_...'))
```

```sql
SELECT id, email FROM users WHERE VERIFY_PASSWORD(password, 'hunter2')
```

## See Also

- [Type System Overview](/types/overview.md)
- [Primitive Types](/types/primitives.md)
- [Validation & Coercion](/types/validation.md)
</content>
</invoke>

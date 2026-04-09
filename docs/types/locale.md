# Locale Types

Types for internationalization, country codes, languages, and currencies.

## Country2

ISO 3166-1 alpha-2 country code (2 bytes). Examples: `US`, `BR`, `DE`, `JP`.

```sql
CREATE TABLE users (country Country2)
```

```rust
Value::Country2("BR")
```

## Country3

ISO 3166-1 alpha-3 country code (3 bytes). Examples: `USA`, `BRA`, `DEU`, `JPN`.

```rust
Value::Country3("BRA")
```

## Lang2

ISO 639-1 language code (2 bytes). Examples: `en`, `pt`, `de`, `ja`.

```sql
CREATE TABLE content (language Lang2 NOT NULL)
```

```rust
Value::Lang2("pt")
```

## Lang5

IETF language tag (5 bytes). Examples: `en-US`, `pt-BR`, `de-DE`, `ja-JP`.

```rust
Value::Lang5("pt-BR")
```

## Currency

ISO 4217 currency code (3 bytes). Examples: `USD`, `BRL`, `EUR`, `JPY`.

```sql
CREATE TABLE prices (currency Currency NOT NULL)
```

```rust
Value::Currency("BRL")
```

## Example: Multi-Locale Product Catalog

```sql
CREATE TABLE products (
  name Text NOT NULL,
  price Decimal NOT NULL,
  currency Currency NOT NULL,
  origin Country2,
  language Lang5 DEFAULT 'en-US'
)
```

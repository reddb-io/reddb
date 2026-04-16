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

Use `Currency` when the domain is strictly fiat ISO 4217. For crypto or mixed-asset ledgers, use
`AssetCode` or `Money`.

## AssetCode

Validated uppercase asset identifier for fiat or crypto symbols. Examples: `USD`, `BTC`, `ETH`,
`USDT`, `STETH`.

```sql
CREATE TABLE balances (asset AssetCode NOT NULL)
```

```rust
Value::AssetCode("BTC")
```

`AssetCode` is more general than `Currency`: it accepts normalized asset symbols beyond 3-letter
ISO 4217 codes.

## Money

Exact monetary value stored as:

- `asset_code`
- `minor_units` as signed integer
- `scale` as explicit decimal places

This avoids float rounding in storage and supports both fiat and crypto.

```sql
CREATE TABLE balances (
  balance Money NOT NULL
)
```

Accepted text coercions:

- `BRL 10.99`
- `0.00012345 BTC`
- `USD:1.23`

Internally:

- `BRL 10.99` -> `asset_code=BRL`, `minor_units=1099`, `scale=2`
- `BTC 0.00012345` -> `asset_code=BTC`, `minor_units=12345`, `scale=8`

Query helpers:

- `MONEY('BTC 0.125')`
- `MONEY('BTC', '0.125')`
- `MONEY_ASSET(balance)`
- `MONEY_MINOR(balance)`
- `MONEY_SCALE(balance)`

See [Scalar Functions](/query/scalar-functions.md#financial-functions).

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

## Example: Fiat + Crypto Pricing

```sql
CREATE TABLE quotes (
  base AssetCode NOT NULL,
  quote AssetCode NOT NULL,
  mid_price Money NOT NULL,
  venue Text
)
```

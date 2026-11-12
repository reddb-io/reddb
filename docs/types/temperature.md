# Temperature

`TEMPERATURE` is a native measured quantity. It follows `MONEY`'s exact-decimal
representation: signed 64-bit `minor_units`, an explicit decimal `scale` from 0
through 18, and a unit carried by the value (`C`, `F`, or `K`). RedDB uses one
temperature type rather than separate Celsius, Fahrenheit, and Kelvin types.

```sql
CREATE TABLE readings (id INTEGER, value TEMPERATURE);
INSERT INTO readings (id, value) VALUES (1, CAST('20 C' AS TEMPERATURE));
INSERT INTO readings (id, value) VALUES (2, CAST('68 F' AS TEMPERATURE));
INSERT INTO readings (id, value) VALUES (3, CAST('293.15 K' AS TEMPERATURE));

SELECT value, TEMPERATURE_UNIT(value), TEMPERATURE_VALUE(value, 'C')
FROM readings;
```

Storage and JSON preserve the supplied unit. Decimal parsing normalizes trailing
zeroes; it does not preserve the original spelling. The portable value codec
uses 11 bytes including its tag. JSON exposes `unit`, `minor_units`, and `scale`.

Equality, ordering, and hashing compare exact rational Kelvin values, so
`20 C = 68 F = 293.15 K`. Scale overflow, unknown units, and values below
absolute zero are rejected by construction, explicit casts, and decoding.
`TEMPERATURE_UNIT(value)` returns the stored unit. `TEMPERATURE_VALUE(value,
unit)` returns an approximate `DOUBLE`, making numeric conversion explicit:

```sql
SELECT AVG(TEMPERATURE_VALUE(value, 'C')) AS mean_celsius FROM readings;
```

`TEMPERATURE(text)` follows the scalar-constructor convention: invalid input
returns `NULL`. Use `CAST(... AS TEMPERATURE)` when invalid input must raise an
error. Absolute temperatures are distinct from temperature differences.

The portable datatype tag is 56 and the native-store value tag is 53; existing
tags were not renumbered. A paged `.rdb` that may contain temperature values
declares the temperature capability in both superblock copies before publishing
the schema or any dependent value. Older readers therefore reject an unknown
capability before scanning pages instead of failing on an arbitrary record.

Legacy wire transports encode a temperature as text containing its unit. They
do not round-trip a typed temperature parameter and no new legacy wire tag was
introduced. Canonical-key indexes do not yet encode temperature keys.

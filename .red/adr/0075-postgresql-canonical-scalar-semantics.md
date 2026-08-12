# ADR 0075 — PostgreSQL-canonical scalar semantics

**Status:** Accepted
**Date:** 2026-08-12
**Related:** Spec [#2110](https://github.com/reddb-io/reddb/issues/2110), decision issue
[#2121](https://github.com/reddb-io/reddb/issues/2121), and the forensic
[`evaluator_divergence_matrix_v1.md`](../../crates/reddb-server/tests/fixtures/evaluator_divergence_matrix_v1.md)

## Context

RedDB has nine scalar evaluators or comparison helpers. The divergence matrix records different
answers for NULL logic, numeric boundaries, arithmetic errors, text coercion, decimal ordering,
LIKE, temporal values, and Booleans. Consolidating those implementations without first choosing
the semantics would merely make one implementation's accidents authoritative.

RedDB also exposes a PostgreSQL-wire surface and exercises RQL through sqllogictest. Maintaining a
separate RedDB truth table where PostgreSQL has a defined behavior would create a permanent
compatibility split. PostgreSQL semantics are therefore the canonical oracle. The few cases that
PostgreSQL does not model directly, such as signed-to-unsigned integer comparison, follow the same
numeric-value principle rather than representation-specific casts.

This ADR decides semantics only. It does not select the consolidation architecture or migrate any
evaluator.

## Decision

### 1. NULL uses three-valued logic

Comparisons involving NULL, including `NULL = NULL`, evaluate to NULL. A predicate keeps only rows
for which it evaluates to TRUE, so both FALSE and NULL drop a row. `IS NULL` and `IS NOT NULL` are
the explicit two-valued null tests.

AND, OR, and NOT use the Kleene truth tables:

| A | B | A AND B | A OR B |
| --- | --- | --- | --- |
| TRUE | NULL | NULL | TRUE |
| FALSE | NULL | FALSE | NULL |
| NULL | NULL | NULL | NULL |

`NOT NULL` is NULL. The existing `typed-expr` behavior wins for NULL equality and Boolean logic;
the other evaluators must not translate an unknown result to `true`, `false`, or `none`.

An unknown column is an error, regardless of whether it occurs under a comparison or `IS NULL`.
The phase may differ: `typed-expr`'s runtime error and `compiled-filter`'s compile-time error both
conform. The silent `none`, FALSE, and TRUE behaviors do not.

### 2. Numeric comparison is by numeric value

Signed and unsigned integers compare in an exact widened domain; they are never compared after a
lossy or overflowing cast. Thus `i64::MAX` equals the same `u64`, every negative signed integer is
less than every unsigned integer, and `i64::MAX < u64::MAX` is TRUE. The current `runtime-expr`,
`compiled-scalar`, `runtime-filter`, `types-compare`, and `runtime-compare` value-based behavior
wins; the legacy and compiled filters' cross-sign equality behavior does not.

Decimal and DecimalText values also compare by numeric value, at decimal precision, never by their
lexical representation. Therefore `Decimal(1.0000) = DecimalText('1')` and
`DecimalText('2') < DecimalText('10')` are TRUE. `types-compare` is the current winning behavior for
both matrix cases.

Integer, unsigned, decimal, and floating values are one numeric comparison family, subject to
exact conversion and the float rules below. An attempted numeric conversion that is invalid or
out of range is an error; implementations must not silently round merely to make values
comparable.

### 3. Arithmetic failures are errors

Integer arithmetic is checked. `i64::MAX + 1` is an overflow error; it does not saturate. Division
by zero is an error; it does not become NULL or `none`. The current `typed-expr` behavior wins in
both cases.

### 4. Text-to-number coercion is narrow and explicit in direction

When one comparison operand is numeric and the other is text, RedDB attempts to parse the text as
the numeric operand's type and then performs numeric comparison. `Integer(5) = Text('5')` is TRUE.
Unparseable or out-of-range text is an error, not FALSE or NULL. The successful-cast behavior of
`runtime-expr`, `compiled-scalar`, `runtime-filter`, and `runtime-compare` wins for the matrix case;
the mandatory error on an invalid cast is the selected third behavior.

This exception does not establish general implicit stringification or coercion. For incompatible
types outside the numeric/text rule and the explicitly supported scalar families in this ADR,
comparison is an operator/type error. The strict `typed-expr` posture wins over evaluators that
return FALSE or NULL for a type mismatch.

### 5. Floating point uses PostgreSQL's total ordering

Floating comparison and sorting use PostgreSQL's total order: NaN is greater than every non-NaN
value, and NaN equals NaN. Consequently `NaN < +inf` is FALSE and `NaN > +inf` is TRUE. The ordering
is a selected third behavior because the matrix evaluators currently either reject ordering or
return FALSE for every ordered NaN comparison. `NaN = NaN` being TRUE is also a selected third
behavior; all current matrix evaluators return FALSE.

Negative infinity sorts below positive infinity. Negative zero equals positive zero and neither
sorts before the other. Every current evaluator agrees on `-inf < +inf` and `-0.0 = 0.0`; that
shared behavior is canonical.

Aggregates do not discard NaN as though it were NULL. MIN and MAX use the same total order, so MAX
is NaN if any non-NULL input is NaN, while MIN is NaN only when every non-NULL input is NaN. SUM
and AVG over a NaN input produce NaN according to PostgreSQL floating arithmetic. COUNT treats NaN
as a value. This aggregate behavior is a selected third behavior where current evaluator helpers
do not provide the operation.

### 6. Text is not normalized implicitly

Text equality is exact UTF-8 byte equality. RedDB performs no implicit Unicode normalization, so a
combining `e` plus acute accent does not equal precomposed `é`. Every current evaluator agrees;
that behavior is canonical. Callers that require normalization must normalize before storage or
comparison.

LIKE is case-sensitive; ILIKE is the case-insensitive operator. `_` consumes exactly one Unicode
scalar value, not one UTF-8 byte and not one grapheme cluster. Thus `AbC LIKE 'a%'` is FALSE,
`café LIKE 'caf_'` is TRUE, and a combining `e` plus acute accent requires two `_` wildcards.
There is no single current winner: the canonical behavior combines `runtime-filter`'s case
sensitivity with `legacy-filter` and `compiled-filter`'s character-based non-ASCII match. It is a
selected third behavior as a complete contract.

### 7. Temporal and Boolean values are ordered

Timestamp, Date, and Time values support same-type chronological ordering. The current
`runtime-expr`, `compiled-scalar`, `runtime-filter`, and `runtime-compare` behavior wins for Date
and Time; all supporting evaluators already agree for Timestamp. Cross-type temporal comparison is
not implied and remains an operator/type error unless separately defined.

Booleans order FALSE before TRUE, so `FALSE < TRUE` is TRUE. Every matrix implementation except
`typed-expr` already has the winning behavior.

## Divergence matrix disposition

This table is the row-by-row migration authority for matrix v1. “Third” means no one current
evaluator supplies the complete chosen behavior.

| Matrix case | Canonical result | Current behavior selected |
| --- | --- | --- |
| `NULL = NULL` | NULL | `typed-expr` |
| `NULL AND TRUE` | NULL | `typed-expr` |
| `NULL OR FALSE` | NULL | `typed-expr` |
| missing column `= NULL` | error | `typed-expr`; `compiled-filter` also conforms earlier |
| missing column `IS NULL` | error | `typed-expr`; `compiled-filter` also conforms earlier |
| `NaN = NaN` | TRUE | third: PostgreSQL equality |
| `NaN < +inf` | FALSE; NaN sorts above `+inf` | third: PostgreSQL total order |
| `-inf < +inf` | TRUE | all current evaluators |
| `-0.0 = 0.0` | TRUE | all current evaluators |
| `i64::MAX = same u64` | TRUE | value-based evaluators; not the legacy/compiled filters |
| `i64::MIN < -1` | TRUE | all current evaluators |
| `i64::MAX < u64::MAX` | TRUE | value-based evaluators; not `typed-expr`'s narrowing cast |
| `i64::MAX + 1` | overflow error | `typed-expr` |
| `1 / 0` | division-by-zero error | `typed-expr` |
| `Integer(5) = Text('5')` | TRUE; invalid text errors | successful cast from runtime evaluators plus third error rule |
| `Decimal(1.0000) = DecimalText('1')` | TRUE | `types-compare` |
| `DecimalText('2') < DecimalText('10')` | TRUE | `types-compare` |
| combining acute equals precomposed | FALSE | all current evaluators |
| `AbC LIKE 'a%'` | FALSE | `runtime-filter` |
| `café LIKE 'caf_'` | TRUE | `legacy-filter` and `compiled-filter` |
| combining `e` plus acute `LIKE '_'` | FALSE | all current filter evaluators |
| Turkish dotted `İ LIKE 'i'` | FALSE | `runtime-filter` |
| Turkish dotless `ı LIKE 'I'` | FALSE | all current filter evaluators |
| `Timestamp(1) < Timestamp(2)` | TRUE | all supporting current evaluators |
| `Date(1) < Date(2)` | TRUE | runtime evaluators |
| `Time(1) < Time(2)` | TRUE | runtime evaluators |
| `FALSE < TRUE` | TRUE | all current evaluators except `typed-expr` |

## Migration

This is a clean break: there is no compatibility flag and no deprecation window. Eight of the
nine matrix implementations currently answer `NULL = NULL` as TRUE. Migrating them to NULL changes
observable filter results for queries that relied on equality to match NULLs; such queries must
use `IS NULL`. The break is accepted so the PG-wire and RQL surfaces do not preserve incompatible
predicate semantics.

Each evaluator migration is a separate slice and must cite this ADR. Its tests must cover the
applicable rows above, including error outcomes rather than only successful values. The divergence
matrix remains forensic evidence: update a cell only alongside a reviewed migration to this
contract. Consolidation may remove implementations, but it must not silently choose a different
winner.

## Consequences

- Every query surface has one scalar truth table and one error posture to target.
- Filters must preserve unknown through expression evaluation and apply row-elimination only at
  the predicate boundary.
- Comparison helpers need exact cross-representation numeric comparison rather than fallible
  narrowing or lexical DecimalText ordering.
- Float sort keys and aggregate comparisons need an explicit PostgreSQL-compatible NaN order.
- LIKE implementations must operate on Unicode scalar values and separate LIKE from ILIKE.
- Previously permissive missing-column and mismatched-type paths become loud errors.

## Alternatives considered

1. **Keep the majority evaluator behavior.** Rejected: implementation count is not a semantics
   argument and would preserve `NULL = NULL` as a RedDB-specific PG incompatibility.
2. **Use a compatibility flag or staged deprecation for 3VL.** Rejected: it would create two truth
   tables during the consolidation and prolong the compatibility split.
3. **Make all numeric/text comparisons errors.** Rejected in favor of the ruled PostgreSQL-style
   numeric cast of text, with invalid input still failing loudly.
4. **Copy one evaluator wholesale.** Rejected: no evaluator implements the complete contract;
   NaN ordering and LIKE in particular require explicit third behaviors.

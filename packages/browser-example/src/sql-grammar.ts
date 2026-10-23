// From the DS v2026.10.30 red-ui query reference (apps/showcase/src/lib/sql-grammar.ts).
import type { CodeFieldGrammar, CodeFieldToken } from "@reddb-io/design-system/data/CodeField";

const SQL_KEYWORDS = new Set(
  "select from where join left inner on and or not in as order by group having limit offset desc asc distinct count sum avg min max case when then else end is null like between with union all explain"
    .split(" "),
);

/** A small SQL grammar for the CodeField highlight layer: keywords and strings. */
export const sqlGrammar: CodeFieldGrammar = {
  tokenize(value) {
    const tokens: CodeFieldToken[] = [];
    const pattern = /'[^']*'?|[A-Za-z_][\w]*|\s+|[^\sA-Za-z_']+/g;
    for (const match of value.matchAll(pattern)) {
      const text = match[0];
      const start = match.index;
      // A name after `.` is a field (`r.from`), never a keyword.
      const qualified = value[start - 1] === ".";
      const kind = text.startsWith("'")
        ? "string"
        : !qualified && SQL_KEYWORDS.has(text.toLowerCase())
          ? "keyword"
          : "text";
      const previous = tokens.at(-1);
      if (kind === "text" && previous?.kind === "text") {
        previous.value += text;
        previous.end = start + text.length;
      } else {
        tokens.push({ kind, value: text, start, end: start + text.length });
      }
    }
    return tokens;
  },
};

/** Pure token and completion contracts for the dependency-free Data Kit CodeField. */

export interface CodeFieldToken {
  /** Grammar-owned token kind. `text` is reserved for unparsed source spans. */
  kind: string;
  /** Exact source text represented by this token. */
  value: string;
  start: number;
  end: number;
  /** Optional semantic name exposed by the grammar. */
  name?: string;
  /** Optional grammar-owned state used by the highlight layer. */
  state?: string;
}

export interface CodeFieldCompletion {
  query: string;
  replaceStart: number;
  replaceEnd: number;
}

export interface CodeFieldCompletionResult {
  value: string;
  cursor: number;
}

export interface CodeFieldGrammar {
  tokenize(value: string): readonly CodeFieldToken[];
  completionAt?(value: string, cursor: number): CodeFieldCompletion | null;
  complete?(
    value: string,
    completion: CodeFieldCompletion,
    suggestion: string,
  ): CodeFieldCompletionResult;
}

export interface CodeFieldSuggestion {
  value: string;
  label?: string;
  disabled?: boolean;
}

export type VariableResolver = (name: string) => boolean;
export type VariableResolution = readonly string[] | ReadonlySet<string> | VariableResolver;

function resolverFor(resolved: VariableResolution): VariableResolver {
  if (typeof resolved === "function") return resolved;
  const names = resolved instanceof Set ? resolved : new Set(resolved);
  return (name) => names.has(name);
}

const VARIABLE = /\{\{\s*([A-Za-z_$][\w.$-]*)\s*\}\}/g;
const COMPLETION_FRAGMENT = /^\s*([A-Za-z_$][\w.$-]*)?$/;
const COMPLETION_SUFFIX = /^([\w.$-]*)(\s*\}\})?/;

/**
 * Reference `{{variable}}` grammar.
 *
 * Resolution is caller-owned: pass known names or a predicate. Unknown names
 * remain tokens and carry the `unresolved` state instead of being discarded.
 */
export function variableTokenGrammar(resolved: VariableResolution = []): CodeFieldGrammar {
  const isResolved = resolverFor(resolved);

  return {
    tokenize(value) {
      const tokens: CodeFieldToken[] = [];
      let position = 0;

      for (const match of value.matchAll(VARIABLE)) {
        const start = match.index;
        if (start > position) {
          tokens.push({ kind: "text", value: value.slice(position, start), start: position, end: start });
        }

        const source = match[0];
        const name = match[1]!;
        const end = start + source.length;
        tokens.push({
          kind: "variable",
          value: source,
          name,
          state: isResolved(name) ? "resolved" : "unresolved",
          start,
          end,
        });
        position = end;
      }

      if (position < value.length) {
        tokens.push({ kind: "text", value: value.slice(position), start: position, end: value.length });
      }
      return tokens;
    },

    completionAt(value, cursor) {
      const boundedCursor = Math.min(value.length, Math.max(0, cursor));
      const opening = value.lastIndexOf("{{", boundedCursor);
      if (opening < 0 || value.lastIndexOf("}}", boundedCursor - 1) > opening) return null;

      const fragment = value.slice(opening + 2, boundedCursor);
      const match = COMPLETION_FRAGMENT.exec(fragment);
      if (!match) return null;

      const query = match[1] ?? "";
      const suffix = COMPLETION_SUFFIX.exec(value.slice(boundedCursor));
      return {
        query,
        replaceStart: boundedCursor - query.length,
        replaceEnd: boundedCursor + (suffix?.[1]?.length ?? 0),
      };
    },

    complete(value, completion, suggestion) {
      const afterReplacement = value.slice(completion.replaceEnd);
      const closing = /^\s*\}\}/.exec(afterReplacement);
      const after = closing ? afterReplacement.slice(closing[0].length) : afterReplacement;
      const inserted = `${suggestion}}}`;
      const nextValue =
        value.slice(0, completion.replaceStart) + inserted + after;
      return {
        value: nextValue,
        cursor: completion.replaceStart + inserted.length,
      };
    },
  };
}

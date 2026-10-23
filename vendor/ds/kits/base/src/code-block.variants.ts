import { tv, type VariantProps } from "tailwind-variants";

export const codeBlock = tv({
  slots: {
    // The root does not clip: an overflowing pre draws the canonical focus
    // ring 2px outside itself (wave 5A), and clipping would cut it off.
    root: "min-w-0 [contain:inline-size] rounded-lg border border-muted bg-transparent text-foreground",
    toolbar:
      "flex items-center justify-between gap-[var(--reddb-spatial-gap-md)] border-b border-muted px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)]",
    language: "min-w-0 truncate text-sm text-ink-muted",
    // Its own panel is the root: the pre states no edge or ground, so a
    // document around it (Prose) cannot add a second one.
    // While it overflows it is a focusable region (wave 5A) with the ink ring.
    // Its bottom corners follow the panel's, so its scrollbar stays inside.
    pre: [
      "m-0 overflow-x-auto rounded-t-none rounded-b-lg border-0 bg-transparent p-[var(--reddb-spatial-inset-md)]",
      "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
    ].join(" "),
    code: "block whitespace-pre font-mono text-sm leading-relaxed text-foreground",
  },
});

export type CodeBlockVariants = VariantProps<typeof codeBlock>;

/** One span of highlighted source: its exact text and the grammar's kind for it. */
export interface CodeToken {
  /** Grammar-owned kind; `keyword`, `string`, `number` and `comment` have a role, the rest read as text. */
  kind: string;
  /** Exact source text of the span; the spans concatenate back to the code. */
  value: string;
}

/**
 * A synchronous tokenizer. It runs wherever the CodeBlock renders — on the
 * server too — so the highlighted spans are in the prerendered HTML and
 * hydration adopts them instead of repainting the block. The Data Kit's
 * `CodeFieldGrammar` satisfies it, so one grammar highlights both.
 */
export interface CodeTokenizer {
  tokenize(code: string): readonly CodeToken[];
}

/** Token kinds with a highlight role; every other kind keeps the block's foreground. */
export const CODE_TOKEN_CLASSES: Readonly<Record<string, string>> = {
  keyword: "text-primary-text",
  string: "text-[var(--reddb-color-feedback-success-foreground)]",
  number: "text-[var(--reddb-color-feedback-info-foreground)]",
  comment: "text-ink-muted italic",
};

/** The tokens, or the whole code as one text token when they would not reproduce it. */
export function codeTokens(code: string, tokenizer?: CodeTokenizer): readonly CodeToken[] {
  if (!tokenizer) return [{ kind: "text", value: code }];
  const tokens = tokenizer.tokenize(code);
  // A grammar that drops or rewrites text would show something other than what
  // the copy button copies; the exact source wins.
  return tokens.map((token) => token.value).join("") === code ? tokens : [{ kind: "text", value: code }];
}

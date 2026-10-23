// Prose: rendered long-form HTML on the Theme's type roles (ADR 0025, 0027).
//
// Prose styles the elements a Markdown renderer emits — the full CommonMark
// and GFM set — from the outside, through descendant variants. Every selector
// is wrapped as `:where(&) <element>`, so a rule costs only its element's own
// specificity: a Kit component placed inside a document (a CodeBlock, an
// Alert, a Link) keeps every property its own classes set, while bare
// rendered markup gets the document look.
//
// What each element reads as:
//
//   - h1 is the title role, h2 and h3 the heading role, h4 and h5 the body
//     role at the Brand's bold and medium weights, h6 the eyebrow role. The
//     Theme decides the sizes, so a Marketing island reads larger with no prop.
//   - Running copy is the body role in ink; captions, footnotes and table
//     captions the caption role in secondary ink (`ink-muted`, never `muted`).
//   - Links are ink with a persistent underline (WCAG 1.4.1, ADR 0023) that
//     thickens on hover, and draw the ink focus outline.
//   - Lists keep visible markers at every depth (disc, circle, square;
//     decimal, lower-alpha, lower-roman), and a GFM task item trades its
//     marker for its checkbox.
//
// Vertical rhythm is Density's layout tier (ADR 0024): blocks sit
// `layout-gap-sm` apart, a heading opens `layout-gap-lg` (h1, h2) or
// `layout-gap-md` (h3–h6) above itself, and the component tier spaces list
// items and the parts inside a block. Horizontal indents are typographic
// (`em`), because a marker's room is a glyph width, not a Density step.
//
// The measure is the reading width, on DS-named container widths (ADR 0021):
// `prose` (65ch of the current body size, the default), `content` or `full`.
import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

const FLOW = [
  // Blocks: one layout step apart, nothing above the first.
  "[:where(&)>*+*]:mt-[var(--reddb-spatial-layout-gap-sm)]",
  "[:where(&)>:first-child]:mt-0",
  // Blocks nested in a container element keep the same rhythm inside it.
  "[:where(&)_:is(blockquote,details,figure,[data-footnotes])>*+*]:mt-[var(--reddb-spatial-layout-gap-sm)]",
  // List items and the paragraphs or lists inside one: the component tier.
  "[:where(&)_li+li]:mt-[var(--reddb-spatial-gap-md)]",
  "[:where(&)_li>*+*]:mt-[var(--reddb-spatial-gap-md)]",
  "[:where(&)_dd>*+*]:mt-[var(--reddb-spatial-gap-md)]",
];

const HEADINGS = [
  "[:where(&)_:is(h1,h2,h3,h4,h5)]:text-foreground",
  "[:where(&)_:is(h1,h2,h3,h4,h5,h6)]:text-balance",
  "[:where(&)_h1]:text-title",
  "[:where(&)_h2]:text-heading",
  "[:where(&)_h3]:text-heading",
  "[:where(&)_h4]:text-body",
  "[:where(&)_h4]:font-bold",
  "[:where(&)_h5]:text-body",
  "[:where(&)_h5]:font-medium",
  "[:where(&)_h6]:text-eyebrow",
  "[:where(&)_h6]:uppercase",
  "[:where(&)_h6]:text-ink-muted",
  // A heading opens a section, so the space above it is the larger step;
  // what it introduces follows close, at the component tier.
  "[:where(&)_:is(h1,h2)]:mt-[var(--reddb-spatial-layout-gap-lg)]",
  "[:where(&)_:is(h3,h4,h5,h6)]:mt-[var(--reddb-spatial-layout-gap-md)]",
  "[:where(&)_:is(h1,h2,h3,h4,h5,h6)+*]:mt-[var(--reddb-spatial-gap-md)]",
];

const INLINE = [
  // Links: ink, a persistent underline, a thicker one on hover, the ink focus.
  "[:where(&)_a]:text-foreground",
  "[:where(&)_a]:underline",
  "[:where(&)_a]:decoration-current",
  "[:where(&)_a]:underline-offset-[var(--reddb-spatial-gap-sm)]",
  "[:where(&)_a:hover]:decoration-2",
  "[:where(&)_a:focus-visible]:rounded-sm",
  "[:where(&)_a:focus-visible]:outline-2",
  "[:where(&)_a:focus-visible]:outline-offset-2",
  "[:where(&)_a:focus-visible]:outline-focus",
  "[:where(&)_:is(strong,b)]:font-bold",
  "[:where(&)_:is(del,s)]:text-ink-muted",
  "[:where(&)_abbr[title]]:underline",
  "[:where(&)_abbr[title]]:decoration-dotted",
  // Inline code and key caps: the mono family on a quiet surface, at the
  // size of the copy around them.
  "[:where(&)_code]:font-mono",
  "[:where(&)_code]:rounded-sm",
  "[:where(&)_code]:bg-muted",
  "[:where(&)_code]:px-[0.3em]",
  "[:where(&)_code]:py-[0.1em]",
  "[:where(&)_kbd]:font-mono",
  "[:where(&)_kbd]:rounded-sm",
  "[:where(&)_kbd]:border",
  "[:where(&)_kbd]:border-b-2",
  "[:where(&)_kbd]:border-muted",
  "[:where(&)_kbd]:px-[0.35em]",
  "[:where(&)_kbd]:whitespace-nowrap",
  // Highlight: the warning tint under ink, never the accent.
  "[:where(&)_mark]:rounded-sm",
  "[:where(&)_mark]:bg-feedback-warning-surface",
  "[:where(&)_mark]:text-foreground",
  "[:where(&)_mark]:px-[0.15em]",
];

const LISTS = [
  "[:where(&)_ul]:ps-[1.5em]",
  "[:where(&)_ol]:ps-[2em]",
  "[:where(&)_ul]:list-disc",
  "[:where(&)_ul_ul]:list-[circle]",
  "[:where(&)_ul_ul_ul]:list-[square]",
  "[:where(&)_ol]:list-decimal",
  "[:where(&)_ol_ol]:list-[lower-alpha]",
  "[:where(&)_ol_ol_ol]:list-[lower-roman]",
  "[:where(&)_li::marker]:text-ink-muted",
  // GFM task items: the checkbox is the marker. Checked is the Brand accent,
  // as on every checked control (DESIGN.md).
  "[:where(&)_li:has(>input[type=checkbox])]:list-none",
  "[:where(&)_li>input[type=checkbox]]:me-[0.5em]",
  "[:where(&)_li>input[type=checkbox]]:align-middle",
  "[:where(&)_li>input[type=checkbox]]:accent-primary",
  // Definition lists.
  "[:where(&)_dt]:font-medium",
  "[:where(&)_dd]:ps-[1.5em]",
  "[:where(&)_dt+dd]:mt-[var(--reddb-spatial-gap-sm)]",
  "[:where(&)_dd+dt]:mt-[var(--reddb-spatial-layout-gap-sm)]",
];

const BLOCKS = [
  "[:where(&)_blockquote]:border-s-2",
  "[:where(&)_blockquote]:border-ink-muted",
  "[:where(&)_blockquote]:ps-[var(--reddb-spatial-inset-md)]",
  "[:where(&)_blockquote]:text-ink-muted",
  // Code blocks: a sunken, bordered panel that scrolls rather than widening
  // the measure; the code inside it drops the inline treatment.
  "[:where(&)_pre]:overflow-x-auto",
  "[:where(&)_pre]:rounded-md",
  "[:where(&)_pre]:border",
  "[:where(&)_pre]:border-muted",
  "[:where(&)_pre]:bg-elevation-sunken-surface",
  "[:where(&)_pre]:p-[var(--reddb-spatial-inset-md)]",
  "[:where(&)_pre]:font-mono",
  "[:where(&)_pre]:text-caption",
  "[:where(&)_pre]:text-elevation-sunken-foreground",
  "[:where(&)_pre_code]:rounded-none",
  "[:where(&)_pre_code]:bg-transparent",
  "[:where(&)_pre_code]:p-0",
  "[:where(&)_hr]:border-muted",
  "[:where(&)_hr]:my-[var(--reddb-spatial-layout-gap-lg)]",
  "[:where(&)_img]:max-w-full",
  "[:where(&)_img]:h-auto",
  "[:where(&)_img]:rounded-md",
  "[:where(&)_figcaption]:mt-[var(--reddb-spatial-gap-md)]",
  "[:where(&)_figcaption]:text-caption",
  "[:where(&)_figcaption]:text-ink-muted",
  // Disclosure: a quiet bordered box whose summary is the control.
  "[:where(&)_details]:rounded-md",
  "[:where(&)_details]:border",
  "[:where(&)_details]:border-muted",
  "[:where(&)_details]:px-[var(--reddb-spatial-inset-md)]",
  "[:where(&)_details]:py-[var(--reddb-spatial-inset-sm)]",
  "[:where(&)_summary]:cursor-pointer",
  "[:where(&)_summary]:font-medium",
  "[:where(&)_summary:focus-visible]:rounded-sm",
  "[:where(&)_summary:focus-visible]:outline-2",
  "[:where(&)_summary:focus-visible]:outline-offset-2",
  "[:where(&)_summary:focus-visible]:outline-focus",
  // GFM footnotes: the section a renderer appends, in the caption role.
  "[:where(&)_[data-footnotes]]:mt-[var(--reddb-spatial-layout-gap-lg)]",
  "[:where(&)_[data-footnotes]]:border-t",
  "[:where(&)_[data-footnotes]]:border-muted",
  "[:where(&)_[data-footnotes]]:pt-[var(--reddb-spatial-layout-gap-sm)]",
  "[:where(&)_[data-footnotes]]:text-caption",
  "[:where(&)_[data-footnotes]]:text-ink-muted",
];

const TABLES = [
  "[:where(&)_table]:w-full",
  "[:where(&)_table]:border-collapse",
  "[:where(&)_caption]:pb-[var(--reddb-spatial-gap-md)]",
  "[:where(&)_caption]:text-start",
  "[:where(&)_caption]:text-caption",
  "[:where(&)_caption]:text-ink-muted",
  "[:where(&)_:is(th,td)]:px-[var(--reddb-spatial-inset-sm)]",
  "[:where(&)_:is(th,td)]:py-[var(--reddb-spatial-gap-md)]",
  "[:where(&)_:is(th,td)]:align-top",
  "[:where(&)_th]:font-medium",
  "[:where(&)_th]:border-b-2",
  "[:where(&)_th]:border-muted",
  "[:where(&)_td]:border-b",
  "[:where(&)_td]:border-muted",
  "[:where(&)_td]:tabular-nums",
  // GFM column alignment arrives as `align`; a cell without one reads from
  // the start edge, so a right-to-left document stays right.
  "[:where(&)_:is(th,td):not([align])]:text-start",
  "[:where(&)_[align=left]]:text-start",
  "[:where(&)_[align=center]]:text-center",
  "[:where(&)_[align=right]]:text-end",
];

const MEASURE = {
  prose: "max-w-prose",
  content: "max-w-content",
  full: "max-w-none",
} as const;

export const prose = tv(
  {
    base: [
      "block min-w-0 text-body text-foreground wrap-break-word",
      ...FLOW,
      ...HEADINGS,
      ...INLINE,
      ...LISTS,
      ...BLOCKS,
      ...TABLES,
    ].join(" "),
    variants: {
      measure: MEASURE,
      striped: {
        true: "[:where(&)_tbody_tr:nth-child(even)]:bg-muted",
        false: "",
      },
    },
    defaultVariants: { measure: "prose", striped: false },
  },
  typeRoleMerge,
);

export type ProseVariants = VariantProps<typeof prose>;
export type ProseMeasure = NonNullable<ProseVariants["measure"]>;
export const PROSE_MEASURES = Object.keys(MEASURE) as readonly ProseMeasure[];
/** The elements Prose may render as: a neutral block, or a document landmark. */
export const PROSE_ELEMENTS = ["div", "article", "section"] as const;
export type ProseElement = (typeof PROSE_ELEMENTS)[number];

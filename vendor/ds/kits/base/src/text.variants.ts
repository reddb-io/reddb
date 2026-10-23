// Text: running copy in one of the Theme's reading roles (ADR 0025).
//
// Body is the reading role; caption is the smaller, supporting one. Size, line
// height, tracking and weight are the role's, so Application reads at 14px,
// Marketing at 18px and a document at 16px with no prop changing.
import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

const ROLE = {
  body: "text-body",
  caption: "text-caption",
} as const;

/**
 * Ink, or the secondary reading role (`ink-muted`, never the `muted` surface).
 * It is `ink`, not `tone`: `tone` is the semantic vocabulary (ADR 0026).
 */
const INK = {
  default: "text-foreground",
  muted: "text-ink-muted",
} as const;

export const text = tv(
  {
    base: "",
    variants: { role: ROLE, ink: INK },
    defaultVariants: { role: "body", ink: "default" },
  },
  typeRoleMerge,
);

export type TextVariants = VariantProps<typeof text>;
export type TextRole = NonNullable<TextVariants["role"]>;
export type TextInk = NonNullable<TextVariants["ink"]>;
export const TEXT_ROLES = Object.keys(ROLE) as readonly TextRole[];
export const TEXT_INKS = Object.keys(INK) as readonly TextInk[];
/** The elements Text may render: block copy, an inline run, or fine print. */
export const TEXT_ELEMENTS = ["p", "span", "div", "small"] as const;
export type TextElement = (typeof TEXT_ELEMENTS)[number];

/** A caption supports what it sits under, so it reads muted unless told otherwise. */
export function defaultTextInk(role: TextRole): TextInk {
  return role === "caption" ? "muted" : "default";
}

/** @deprecated Text's ink is `ink` (ADR 0026); use `TextInk`. Removed next release. */
export type TextTone = TextInk;
/** @deprecated Text's ink is `ink` (ADR 0026); use `TEXT_INKS`. Removed next release. */
export const TEXT_TONES = TEXT_INKS;
/** @deprecated Text's ink is `ink` (ADR 0026); use `defaultTextInk`. Removed next release. */
export const defaultTextTone = defaultTextInk;

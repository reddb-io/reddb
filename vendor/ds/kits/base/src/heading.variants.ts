// Heading: a document level and a type role, decided separately (ADR 0025).
//
// The level is the outline (`h1`…`h6`); the role is the look, and it is one of
// the Theme's heading roles. Every size, line height, tracking and weight
// comes from the role's own text utility, so a Marketing island renders
// larger type than the Application page around it without the Heading
// knowing either direction exists.
import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

const ROLE = {
  display: "text-display",
  title: "text-title",
  heading: "text-heading",
} as const;

export const heading = tv(
  {
    base: "text-foreground text-balance",
    variants: { role: ROLE },
    defaultVariants: { role: "heading" },
  },
  typeRoleMerge,
);

export type HeadingVariants = VariantProps<typeof heading>;
export type HeadingRole = NonNullable<HeadingVariants["role"]>;
export const HEADING_ROLES = Object.keys(ROLE) as readonly HeadingRole[];
export const HEADING_LEVELS = [1, 2, 3, 4, 5, 6] as const;
export type HeadingLevel = (typeof HEADING_LEVELS)[number];

/** The role a level reads as when none is chosen: a page title, then section headings. */
export function defaultHeadingRole(level: HeadingLevel): HeadingRole {
  return level === 1 ? "title" : "heading";
}

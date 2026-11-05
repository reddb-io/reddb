// NodeBadge's styling. See button.variants.ts for the split, the colour rule
// and the spatial rule — under which the dot keeps its own size, because it is
// a mark read at a glance rather than a space between things.
//
// A node's reachability is a domain reading, not a tone: the word the cluster
// reports (`online`, `degraded`, `offline`, `unknown`) is what the badge says,
// and it is always in the DOM (see NodeBadge.svelte). Its colour, though,
// speaks the one semantic vocabulary (ADR 0026) through a documented mapping,
// `NODE_STATUS_TONES`, so a degraded node reads as the same warning a Badge,
// an Alert or a StatusIndicator draws — and never as the Brand red, which on a
// status means danger (ADR 0023). Fill is a second cue: a node with no reading
// gets a hollow dot, every reading a filled one.
//
// The marks are StatusIndicator's (each holds 3:1 on every ground, WCAG
// 1.4.11): the role's text-grade stop ringed by its edge stop, and neutral in
// the control edge around secondary ink.

import type { Tone } from "@reddb-io/design-system/base/recipes";
import { tv, type VariantProps } from "tailwind-variants";

/** What each reachability reading means, as a tone (ADR 0026). */
export const NODE_STATUS_TONES = {
  /** Reachable, and keeping up. */
  online: "success",
  /** Reachable, and behind — lagging, rebalancing, or under pressure. */
  degraded: "warning",
  /** Known to the cluster, and not answering. */
  offline: "danger",
  /** The default: no reading yet. */
  unknown: "neutral",
} as const satisfies Record<string, Tone>;

const MARK = {
  neutral: "border-control-edge bg-ink-muted",
  info: "border-[var(--reddb-color-feedback-info-border)] bg-[var(--reddb-color-feedback-info-foreground)]",
  success:
    "border-[var(--reddb-color-feedback-success-border)] bg-[var(--reddb-color-feedback-success-foreground)]",
  warning:
    "border-[var(--reddb-color-feedback-warning-border)] bg-[var(--reddb-color-feedback-warning-foreground)]",
  danger:
    "border-[var(--reddb-color-feedback-danger-border)] bg-[var(--reddb-color-feedback-danger-foreground)]",
} as const satisfies Record<Tone, string>;

const STATUS = {
  online: { dot: MARK[NODE_STATUS_TONES.online] },
  degraded: { dot: MARK[NODE_STATUS_TONES.degraded] },
  offline: { dot: MARK[NODE_STATUS_TONES.offline] },
  // Hollow: no reading is drawn as an empty mark, not a grey fill.
  unknown: { dot: "border-control-edge bg-transparent" },
} as const satisfies Record<keyof typeof NODE_STATUS_TONES, { dot: string }>;

const LABELLED = {
  /** The default: the status word is drawn next to the node's name. */
  true: { status: "text-ink-muted" },
  /** Announced, never drawn — for a dense cluster list. */
  false: { status: "sr-only" },
} as const;

export const nodeBadge = tv({
  slots: {
    root: "inline-flex items-center gap-[var(--reddb-spatial-gap-md)] rounded-full border border-muted px-2.5 py-1 text-xs leading-tight whitespace-nowrap",
    dot: "size-2 shrink-0 rounded-full border",
    name: "font-mono text-foreground",
    status: "",
  },
  variants: { status: STATUS, labelled: LABELLED },
  defaultVariants: { status: "unknown", labelled: true },
});

export type NodeBadgeVariants = VariantProps<typeof nodeBadge>;
export type NodeStatus = NonNullable<NodeBadgeVariants["status"]>;

export const NODE_STATUSES = Object.keys(STATUS) as readonly NodeStatus[];

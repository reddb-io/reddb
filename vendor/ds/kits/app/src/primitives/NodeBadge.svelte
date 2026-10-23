<!--
  NodeBadge — a Primitive: it imports no other Kit component.

  The one component in this Kit that knows a reddb word: a node, and whether
  the cluster can currently reach it. It is here rather than in an application
  because every reddb surface that lists nodes needs the same marker, and the
  four statuses are the DS's to keep consistent.

  It does not import Badge. It could look like one, and reaching for it would
  make this a Composite in exchange for classes it does not want anyway — a
  Badge is rectangular and takes no dot.

  Its reachability is a domain reading, coloured through the documented
  `NODE_STATUS_TONES` mapping onto the shared tone vocabulary (ADR 0026), and
  announced as `data-tone` alongside `data-node-status`.

  The status word is always rendered, `sr-only` when it is not drawn: a dot is
  a colour and a fill, and neither is available to a screen reader or to anyone
  who cannot tell the two dots apart.
-->
<script lang="ts">
  import type { NodeBadgeContract } from "../core/contracts/nodebadge";
  import type { HTMLAttributes } from "svelte/elements";
  import { NODE_STATUS_TONES, nodeBadge, type NodeStatus } from "./node-badge.variants";

  interface Props extends NodeBadgeContract, Omit<HTMLAttributes<HTMLSpanElement>, keyof NodeBadgeContract | "class"> {
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  const {
    name,
    status = "unknown",
    showStatus = true,
    class: className,
    ...rest
  }: Props = $props();

  const slots = $derived(nodeBadge({ status, labelled: showStatus }));
  const tone = $derived(NODE_STATUS_TONES[status]);
</script>

<span {...rest} class={slots.root({ class: className })} data-node-status={status} data-tone={tone}>
  <span class={slots.dot()} aria-hidden="true"></span>
  <span class={slots.name()}>{name}</span>
  <span class={slots.status()}>{status}</span>
</span>

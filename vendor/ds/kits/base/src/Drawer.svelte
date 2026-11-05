<!-- An edge overlay that inherits the canonical Dialog lifecycle. -->
<script lang="ts">
  import type { ComponentProps } from "svelte";
  import Dialog from "./Dialog.svelte";
  import { drawer, type DrawerSide } from "./drawer.variants";

  type Props = Omit<ComponentProps<typeof Dialog>, "class"> & {
    /** Edge that owns the Drawer. Defaults to `right`. */
    side?: DrawerSide;
    /** Extra classes merged onto the edge surface. */
    class?: string;
  };

  let { side = "right", open = $bindable(false), class: className, ...dialogProps }: Props = $props();
</script>

<Dialog {...dialogProps} bind:open data-drawer="" class={drawer({ side, class: className })} />

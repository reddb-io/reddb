<script lang="ts">
  import { transfer } from "./transfer.variants";
  import { transferPresentation } from "./core/operational-motion";
  import type { TransferContract } from "./core/contracts/transfer";
  interface Props extends TransferContract {}
  let {
    fromId,
    toId,
    trigger,
    duration = 400,
    paused = false,
  }: Props = $props();
  let host: HTMLSpanElement;
  let initial = true;
  let previous: string | number | undefined;
  $effect(() => {
    const key = trigger;
    if (initial) {
      initial = false;
      previous = key;
      return;
    }
    const changed = key !== previous;
    previous = key;
    if (changed && !paused && host)
      return transferPresentation(host, fromId, toId, duration);
  });
</script>

<span bind:this={host!} data-transfer aria-hidden="true" class={transfer()}
></span>

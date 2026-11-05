<script lang="ts">
  import { Button, Select, Slider } from "@reddb-io/design-system/base";
  import type { PlaybackControlsContract } from "../core/contracts/playbackcontrols";
  import { PLAYBACK_SPEEDS } from "../core/operational";
  import { operational } from "../operational.variants";
  interface Props extends PlaybackControlsContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    playing,
    position,
    duration,
    speed = 1,
    onplayingchange,
    onpositionchange,
    onspeedchange,
    onstep,
    class: className,
  }: Props = $props();
  const s = operational();
  const speeds = PLAYBACK_SPEEDS;
  const end = $derived(Number.isFinite(duration) ? Math.max(0, duration) : 0);
</script>

<div
  data-playback-controls
  role="group"
  aria-label={label}
  class={s.body({ class: className })}
>
  <div class={s.actions()}>
    <Button
      disabled={!onplayingchange || !end}
      onclick={() => onplayingchange?.(!playing)}
      >{playing ? "Pause" : "Play"}</Button
    ><Button
      disabled={!onstep || playing || position >= end}
      onclick={() => onstep?.()}>Next step</Button
    >
    <div class={s.speed()}>
      <Select
        aria-label="Playback speed"
        value={String(speed)}
        options={speeds}
        disabled={!onspeedchange}
        onvaluechange={(value) => onspeedchange?.(Number(value))}
      />
    </div>
  </div>
  <Slider
    fieldClass={s.seek()}
    label="Replay position"
    value={position}
    min={0}
    max={Math.max(1, end)}
    disabled={!onpositionchange || !end}
    onvaluechange={(value) => onpositionchange?.(value)}
    formatValue={(value) => `${value} / ${end}`}
  />
</div>

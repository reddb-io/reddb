<!-- A consumer form exercising range and pressed-state controls together. -->
<script lang="ts">
  import {
    Form,
    Rating,
    Slider,
    ToggleButton,
    ToggleGroup,
  } from "@reddb-io/design-system/base";

  interface Props {
    sliderOninput?: (event: Event) => void;
    ratingOnkeydown?: (event: KeyboardEvent) => void;
    toggleOnclick?: (event: MouseEvent) => void;
    groupOnvaluechange?: (value: string) => void;
  }

  const { sliderOninput, ratingOnkeydown, toggleOnclick, groupOnvaluechange }: Props = $props();
  const alignments = [
    { value: "left", label: "Left" },
    { value: "center", label: "Center" },
    { value: "right", label: "Right" },
  ] as const;
</script>

<Form data-testid="range-pressed-form">
  <Slider
    label="Deployment capacity"
    name="capacity"
    min={0}
    max={100}
    step={10}
    value={40}
    formatValue={(current) => `${current}%`}
    oninput={sliderOninput}
  />

  <Rating
    legend="Release confidence"
    name="confidence"
    max={5}
    value={3}
    onkeydown={ratingOnkeydown}
  />

  <ToggleButton label="Pin deployment" onclick={toggleOnclick} />

  <ToggleGroup
    legend="Alignment"
    name="alignment"
    options={alignments}
    value="center"
    onvaluechange={groupOnvaluechange}
  />

  <div
    data-appearance-scope
    data-theme="marketing"
    data-color-scheme="dark"
    data-density="compact"
  >
    <Slider label="Nested capacity" value={20} />
    <Rating legend="Nested confidence" name="nested-confidence" value={2} />
    <ToggleButton label="Nested pin" pressed />
    <ToggleGroup legend="Nested alignment" options={alignments} value="left" />
  </div>
</Form>

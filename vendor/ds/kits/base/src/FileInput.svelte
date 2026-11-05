<!-- A native file control composed through canonical Field and Input, with
     selected filenames kept as visible polite feedback. -->
<script lang="ts">
  import { afterFormReset, fileNames } from "./core/native-controls";
  import type { HTMLInputAttributes } from "svelte/elements";
  import Field from "./Field.svelte";
  import Input from "./Input.svelte";
  import { fileInput } from "./file-input.variants";

  interface Props extends Omit<HTMLInputAttributes, "children" | "class" | "id" | "required" | "type" | "value" | "defaultValue"> {
    /** The required visible accessible name supplied through Field. */
    label: string;
    /** Supporting text announced with the file control. */
    help?: string;
    /** Current validation error announced with the file control. */
    error?: string;
    /** Makes the requirement visible and native. */
    required?: boolean;
    /** Explicit control id; otherwise Field supplies a stable generated one. */
    id?: string;
    /** Native files; bind:files follows selection, and FileList/null sets or clears it. */
    files?: FileList | null;
    /** Called with the native FileList after user selection. */
    onfileschange?: (files: FileList | null) => void;
    /** The native file input for imperative focus or file inspection. */
    ref?: HTMLInputElement;
    /** Extra classes merged onto the native file control. */
    class?: string;
    /** Extra classes merged onto the containing Field. */
    fieldClass?: string;
  }

  let {
    label,
    help,
    error,
    required = false,
    id,
    ref = $bindable(),
    files = $bindable(),
    onfileschange,
    class: className,
    fieldClass,
    multiple = false,
    onchange,
    ...rest
  }: Props = $props();

  let element = $state<HTMLInputElement>();
  let selectedNames = $state<string[]>([]);
  const styles = fileInput();

  function handleChange(event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    files = input.files;
    selectedNames = fileNames(input.files);
    onfileschange?.(input.files);
    onchange?.(
      event as Parameters<NonNullable<HTMLInputAttributes["onchange"]>>[0],
    );
  }
  $effect(() => { if (ref !== element) ref = element; });

  $effect(() => {
    if (!element || files === undefined) return;
    if (files === null) element.value = "";
    else if (element.files !== files) element.files = files;
    selectedNames = fileNames(files);
  });

  $effect(() => {
    // Changing the native form target must move the reset listener too.
    rest.form;
    return afterFormReset(element?.form, () => {
      files = null;
      selectedNames = [];
    });
  });
</script>

<div class={styles.root()} data-file-input>
  <Field {label} {help} {error} {required} {id} class={styles.field({ class: fieldClass })}>
    {#snippet children(control)}
      <Input
        {...rest}
        {...control}
        bind:ref={element}
        type="file"
        {multiple}
        onchange={handleChange}
        class={styles.control({ class: className })}
      />
    {/snippet}
  </Field>
  <p class={styles.filename()} aria-live="polite" data-file-name>
    {#if selectedNames.length > 0}
      {selectedNames.join(", ")}
    {:else if multiple}
      No files selected
    {:else}
      No file selected
    {/if}
  </p>
</div>

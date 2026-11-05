<!-- A caller-owned code-aware field whose native control is mirrored by a token highlight layer. -->
<script module lang="ts">
  export { variableTokenGrammar } from "./code-field";
  export type {
    CodeFieldCompletion,
    CodeFieldCompletionResult,
    CodeFieldGrammar,
    CodeFieldSuggestion,
    CodeFieldToken,
    VariableResolution,
    VariableResolver,
  } from "./code-field";
</script>

<script lang="ts">
 import type {CodeFieldContract} from "./core/contracts/codefield";
  import { tick } from "svelte";
  import { warnDeprecated } from "@reddb-io/design-system/base";
  import {
    variableTokenGrammar,
    type CodeFieldCompletion,
    type CodeFieldGrammar,
    type CodeFieldSuggestion,
    type VariableResolution,
  } from "./code-field";

  interface Props extends CodeFieldContract {
    /** Reports every edit and accepted completion (ADR 0026's value dialect). */
    onvaluechange?: (value: string) => void;
    /** @deprecated Renamed `onvaluechange` (ADR 0026); removed next release. */
    onValue?: (value: string) => void;
    /**
     * Extra classes, merged onto the outer wrapper that holds the label, the control and the help
     * text.
     */
    class?: string;
  }

  let {
    label,
    value = $bindable(""),
    onvaluechange,
    onValue,
    grammar,
    resolved = [],
    suggestions = [],
    gutter = false,
    rows = 5,
    id,
    name,
    placeholder,
    help,
    required = false,
    disabled = false,
    class: className = "",
    inputClass = "",
  }: Props = $props();

  const uid = $props.id();
  const inputId = $derived(id ?? `${uid}-input`);
  const helpId = `${uid}-help`;
  const listboxId = `${uid}-suggestions`;
  const activeGrammar = $derived(grammar ?? variableTokenGrammar(resolved));
  const tokens = $derived(activeGrammar.tokenize(value));
  const lineNumbers = $derived(Array.from({ length: value.split("\n").length }, (_, index) => index + 1));
  const normalizedSuggestions = $derived(
    suggestions.map((suggestion) =>
      typeof suggestion === "string" ? { value: suggestion, label: suggestion } : suggestion,
    ),
  );

  let inputElement = $state<HTMLInputElement | HTMLTextAreaElement>();
  let highlightElement = $state<HTMLElement>();
  let gutterElement = $state<HTMLElement>();
  let completion = $state<CodeFieldCompletion | null>(null);
  let activeIndex = $state(-1);

  const filteredSuggestions = $derived.by(() => {
    if (!completion) return [];
    const query = completion.query.toLocaleLowerCase();
    return normalizedSuggestions.filter((suggestion) =>
      suggestion.value.toLocaleLowerCase().startsWith(query),
    );
  });
  const open = $derived(!disabled && completion !== null && filteredSuggestions.length > 0);

  function optionId(index: number): string {
    return `${uid}-suggestion-${index}`;
  }

  function tokenClass(kind: string, state?: string): string {
    if (state === "resolved") {
      return "text-[var(--reddb-color-feedback-success-foreground)]";
    }
    if (state === "unresolved") {
      return "text-[var(--reddb-color-feedback-danger-foreground)] underline decoration-wavy";
    }
    return kind === "text" ? "text-foreground" : "text-primary-text";
  }

  function syncScroll(): void {
    if (!inputElement || !highlightElement) return;
    highlightElement.scrollLeft = inputElement.scrollLeft;
    highlightElement.scrollTop = inputElement.scrollTop;
    if (gutterElement) gutterElement.scrollTop = inputElement.scrollTop;
  }

  function refreshCompletion(element = inputElement): void {
    if (!element || disabled || normalizedSuggestions.length === 0) {
      completion = null;
      activeIndex = -1;
      return;
    }
    completion = activeGrammar.completionAt?.(value, element.selectionStart ?? value.length) ?? null;
    activeIndex = -1;
  }

  function reportValue(): void {
    onvaluechange?.(value);
    onValue?.(value);
  }

  $effect(() => {
    if (onValue) warnDeprecated("CodeField", "onValue", "onvaluechange");
  });

  function handleInput(event: Event): void {
    const element = event.currentTarget as HTMLInputElement | HTMLTextAreaElement;
    value = element.value;
    reportValue();
    refreshCompletion(element);
    syncScroll();
  }

  function moveActive(direction: 1 | -1): void {
    if (filteredSuggestions.length === 0) return;
    let next = activeIndex;
    for (let count = 0; count < filteredSuggestions.length; count += 1) {
      next = (next + direction + filteredSuggestions.length) % filteredSuggestions.length;
      if (!filteredSuggestions[next]?.disabled) {
        activeIndex = next;
        return;
      }
    }
  }

  function choose(index: number): void {
    const suggestion = filteredSuggestions[index];
    if (disabled || !suggestion || suggestion.disabled || !completion) return;

    const result = activeGrammar.complete?.(value, completion, suggestion.value) ?? {
      value:
        value.slice(0, completion.replaceStart) +
        suggestion.value +
        value.slice(completion.replaceEnd),
      cursor: completion.replaceStart + suggestion.value.length,
    };
    value = result.value;
    reportValue();
    completion = null;
    activeIndex = -1;
    void tick().then(() => {
      inputElement?.focus();
      inputElement?.setSelectionRange(result.cursor, result.cursor);
      syncScroll();
    });
  }

  function handleKeydown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      if (!open) refreshCompletion(event.currentTarget as HTMLInputElement | HTMLTextAreaElement);
      if (!open) return;
      event.preventDefault();
      moveActive(event.key === "ArrowDown" ? 1 : -1);
    } else if ((event.key === "Enter" || event.key === "Tab") && open && activeIndex >= 0) {
      event.preventDefault();
      choose(activeIndex);
    } else if (event.key === "Escape" && open) {
      event.preventDefault();
      completion = null;
      activeIndex = -1;
    }
  }

  const controlClass = $derived(
    gutter
      ? "min-h-[calc(var(--reddb-spatial-control-height-lg)*3)]"
      : "h-[var(--reddb-spatial-control-height-md)]",
  );
  const paddingClass = $derived(
    gutter
      ? "py-[var(--reddb-spatial-inset-sm)] pr-[var(--reddb-spatial-inset-md)] pl-[calc(3ch+var(--reddb-spatial-inset-lg))]"
      : "px-[var(--reddb-spatial-inset-md)] py-0",
  );
  // A single line centres its text by line box, not by padding: the control is
  // one control-height tall at every Density stop, and vertical insets that
  // outgrow it (compact, tiny) would push the highlight layer off the caret.
  const lineClass = $derived(
    gutter ? "leading-relaxed" : "leading-[calc(var(--reddb-spatial-control-height-md)-2px)]",
  );
</script>

<div data-code-field data-mode={gutter ? "gutter" : "single-line"} class={`grid gap-[var(--reddb-spatial-gap-sm)] ${className}`.trim()}>
  <label for={inputId} class="text-sm font-medium text-foreground">
    {label}{#if required}<span aria-hidden="true"> *</span>{/if}
  </label>

  <div class="relative">
    <div
      class={`relative overflow-visible rounded-md ${gutter ? "border border-control-edge" : ""} bg-background font-mono text-editable-md text-foreground focus-within:outline-2 focus-within:outline-offset-2 focus-within:outline-focus ${controlClass}`}
    >
      {#if gutter}
        <div
          bind:this={gutterElement}
          data-code-gutter
          aria-hidden="true"
          class="pointer-events-none absolute inset-y-0 left-0 z-10 w-[calc(3ch+var(--reddb-spatial-inset-md))] overflow-hidden border-r border-muted bg-muted py-[var(--reddb-spatial-inset-sm)] text-right leading-relaxed text-ink-muted select-none"
        >
          {#each lineNumbers as line}
            <span data-code-line-number class="block pr-[var(--reddb-spatial-gap-sm)]">{line}</span>
          {/each}
        </div>
      {/if}

      <pre
        bind:this={highlightElement}
        data-code-highlight
        aria-hidden="true"
        class={`pointer-events-none absolute inset-0 m-0 box-border overflow-hidden whitespace-pre ${gutter ? "" : "border border-transparent"} ${paddingClass} ${lineClass}`}
      >{#each tokens as token, index (`${token.start}:${token.end}:${index}`)}<span
            data-code-token
            data-token-kind={token.kind}
            data-token-state={token.state}
            class={tokenClass(token.kind, token.state)}
          >{token.value}</span>{/each}</pre
      >

      {#if gutter}
        <!--
          ARIA in HTML forbids role=combobox on a <textarea> (wave 5A). The
          multi-line editor keeps its native textbox role and carries the
          suggestion contract through the attributes a textbox supports:
          aria-autocomplete, aria-controls and aria-activedescendant. The
          single-line <input> below is a combobox, as the APG pattern allows.
        -->
        <textarea
          bind:this={inputElement}
          id={inputId}
          {name}
          {placeholder}
          {required}
          {disabled}
          {rows}
          value={value}
          aria-autocomplete="list"
          aria-controls={open ? listboxId : undefined}
          aria-activedescendant={open && activeIndex >= 0 ? optionId(activeIndex) : undefined}
          aria-describedby={help ? helpId : undefined}
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          data-code-input
          class={`relative z-10 block min-h-[inherit] w-full resize-y border-0 bg-transparent text-editable-md text-transparent caret-foreground outline-hidden placeholder:text-ink-muted disabled:cursor-not-allowed disabled:opacity-50 ${paddingClass} leading-relaxed ${inputClass}`.trim()}
          oninput={handleInput}
          onscroll={syncScroll}
          onkeydown={handleKeydown}
          onfocus={(event) => refreshCompletion(event.currentTarget)}
          onclick={(event) => refreshCompletion(event.currentTarget)}
        ></textarea>
      {:else}
        <input
          bind:this={inputElement}
          id={inputId}
          {name}
          {placeholder}
          {required}
          {disabled}
          type="text"
          value={value}
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={open}
          aria-controls={open ? listboxId : undefined}
          aria-activedescendant={open && activeIndex >= 0 ? optionId(activeIndex) : undefined}
          aria-describedby={help ? helpId : undefined}
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          data-code-input
          class={`relative z-10 block h-full w-full rounded-md border border-control-edge bg-transparent text-editable-md text-transparent caret-foreground outline-hidden placeholder:text-ink-muted disabled:cursor-not-allowed disabled:opacity-50 ${paddingClass} ${lineClass} ${inputClass}`.trim()}
          oninput={handleInput}
          onscroll={syncScroll}
          onkeydown={handleKeydown}
          onfocus={(event) => refreshCompletion(event.currentTarget)}
          onclick={(event) => refreshCompletion(event.currentTarget)}
        />
      {/if}
    </div>

    {#if open}
      <div
        id={listboxId}
        role="listbox"
        aria-label={`${label} suggestions`}
        class="absolute z-20 mt-[var(--reddb-spatial-gap-sm)] max-h-64 w-full overflow-y-auto rounded-md border border-elevation-overlay-border bg-elevation-overlay-surface p-[var(--reddb-spatial-inset-sm)] text-sm text-elevation-overlay-foreground shadow-elevation-overlay"
      >
        {#each filteredSuggestions as suggestion, index (suggestion.value)}
          <div
            id={optionId(index)}
            role="option"
            tabindex="-1"
            aria-selected={activeIndex === index}
            aria-disabled={suggestion.disabled || undefined}
            data-code-suggestion={suggestion.value}
            class={`flex min-h-[var(--reddb-spatial-control-height-md)] items-center rounded-md px-[var(--reddb-spatial-inset-md)] ${activeIndex === index ? "bg-foreground/10 text-foreground" : "bg-transparent"} ${suggestion.disabled ? "opacity-50" : "cursor-pointer"}`}
            onmousedown={(event) => event.preventDefault()}
            onclick={() => choose(index)}
            onkeydown={(event) => {
              if (event.key === "Enter" || event.key === " ") choose(index);
            }}
          >{suggestion.label ?? suggestion.value}</div>
        {/each}
      </div>
    {/if}
  </div>

  {#if help}
    <p id={helpId} class="m-0 text-sm text-ink-muted">{help}</p>
  {/if}
</div>

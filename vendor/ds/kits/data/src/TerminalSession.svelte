<script module lang="ts">
  export type { TerminalAppearance, TerminalEngine, TerminalEngineFactory, TerminalEngineOptions,
    TerminalInteraction, TerminalSessionHandle } from "./terminal-session";
</script>
<script lang="ts">
  import type { TerminalSessionContract } from "./core/contracts/terminalsession";
  import { onMount, tick, untrack } from "svelte";
  import type { Snippet } from "svelte";
  import { Button, quietControl, useReducedMotion } from "@reddb-io/design-system/base";
  import { terminalAppearance } from "./terminal-session";
  import type { TerminalEngine, TerminalEngineFactory, TerminalInteraction, TerminalSessionHandle } from "./terminal-session";
  import { terminalSession } from "./terminal-session.variants";
  interface Props extends TerminalSessionContract {
    /** Reports a live session handle, then null when it is disposed or replaced. */
    onsessionchange?: (session: TerminalSessionHandle | null) => void;
    /** Application transport boundary for input bytes and valid cell-size changes. */
    oninteraction?: (interaction: TerminalInteraction) => void;
    /** Optional caller-owned header controls; the component adds no connection actions. */
    actions?: Snippet;
    /** Classes merged onto the session region. */
    class?: string;
  }
  let { label, engine, transcript = "", rows = 10, loading = "lazy", readOnly = false,
    onsessionchange, oninteraction, actions, loadingLabel = "Loading terminal…", readyLabel = "Terminal ready",
    errorLabel = "The terminal could not be loaded. Your transcript is still available.", retryLabel = "Retry terminal",
    transcriptLabel = "Session transcript", class: className }: Props = $props();
  const styles = terminalSession();
  // Spread updates may invalidate a function prop without replacing its identity.
  // A live terminal restarts only when the caller supplies another factory.
  const engineFactory = $derived(engine);
  const reducedMotion = useReducedMotion();
  let root: HTMLElement;
  let host: HTMLDivElement;
  let requested = $state(false);
  let attempt = $state(0);
  let focusAfterRetry = false;
  let phase = $state<"idle" | "loading" | "ready" | "error">("idle");
  let runtime = $state.raw<TerminalEngine | undefined>();

  onMount(() => {
    if (loading === "eager" || typeof IntersectionObserver === "undefined") { requested = true; return; }
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { requested = true; observer.disconnect(); }
    }, { rootMargin: "200px" });
    observer.observe(root);
    return () => observer.disconnect();
  });

  $effect(() => {
    if (!requested) return;
    const factory = engineFactory;
    attempt;
    const controller = new AbortController();
    let instance: TerminalEngine | undefined;
    let announced = false;
    let disposed = false;
    const dispose = () => { if (instance && !disposed) { disposed = true; instance.dispose(); } };
    const notify = untrack(() => onsessionchange);
    untrack(() => { phase = "loading"; });
    untrack(() => { void (async () => {
      try {
        const options = untrack(() => ({ label, readOnly, appearance: terminalAppearance(host, reducedMotion.current) }));
        instance = await factory(host, {
          ...options, signal: controller.signal,
          input: data => { if (!controller.signal.aborted && !readOnly) oninteraction?.({ type: "data", data }); },
          resize: (columns, rows) => {
            if (!controller.signal.aborted && Number.isInteger(columns) && columns > 0 && Number.isInteger(rows) && rows > 0)
              oninteraction?.({ type: "resize", columns, rows });
          },
        });
        if (controller.signal.aborted) { dispose(); return; }
        instance.update({ label, appearance: terminalAppearance(host, reducedMotion.current), readOnly });
        instance.fit();
        runtime = instance;
        phase = "ready";
        announced = true;
        notify?.(instance);
        const focusRequested = focusAfterRetry;
        focusAfterRetry = false;
        if (focusRequested) {
          await tick();
          // Retry is an explicit user action; respect any focus move made while loading.
          if (!controller.signal.aborted && document.activeElement === document.body) instance?.focus();
        }
      } catch {
        if (controller.signal.aborted) return;
        controller.abort();
        dispose();
        instance = undefined;
        runtime = undefined;
        phase = "error";
        if (announced) { announced = false; notify?.(null); }
      }
    })(); });
    return () => {
      controller.abort();
      dispose();
      runtime = undefined;
      if (announced) notify?.(null);
    };
  });

  $effect(() => {
    const live = runtime;
    if (!live) return;
    const readonly = readOnly, reduced = reducedMotion.current, name = label;
    function refresh() {
      live!.update({ label: name, appearance: terminalAppearance(host, reduced), readOnly: readonly });
      live!.fit();
    }
    refresh();
    const resize = new ResizeObserver(refresh);
    resize.observe(host);
    const appearance = new MutationObserver(refresh);
    // Axes can be redeclared on any ancestor island, including body and html.
    for (let scope: Element | null = root; scope; scope = scope.parentElement)
      appearance.observe(scope, { attributes: true, attributeFilter: ["data-theme", "data-color-scheme", "data-density", "class", "style"] });
    let active = true;
    void document.fonts?.ready.then(() => { if (active) refresh(); });
    return () => { active = false; resize.disconnect(); appearance.disconnect(); };
  });
</script>

<section bind:this={root} data-terminal-session data-phase={phase} aria-label={label} class={styles.root({ class: className })}>
  <header class={styles.header()}><p class={styles.title()}>{label}</p>{#if actions}<div class={styles.actions()}>{@render actions()}</div>{/if}</header>
  <div class={styles.body()} style:height={`calc(var(--reddb-spatial-control-height-md) * ${Math.max(3, Math.min(40, Number.isFinite(rows) ? rows : 10))})`}>
    <div inert={phase !== "ready"} aria-hidden={phase !== "ready"} class={styles.engine()}>
      <div bind:this={host} data-terminal-engine class={styles.mount()}></div>
    </div>
    {#if phase !== "ready"}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex (The bounded transcript is a named keyboard-scrollable region.) -->
      <pre data-terminal-fallback tabindex="0" role="region" aria-label={transcriptLabel} class={styles.fallback()}>{transcript}</pre>
    {/if}
  </div>
  <div class={styles.footer()}>
    <span role="status">{phase === "error" ? errorLabel : phase === "ready" ? readyLabel : loadingLabel}</span>
    {#if phase === "error"}<Button size="sm" variant="secondary" onclick={(event: MouseEvent) => {
      focusAfterRetry = event.currentTarget === document.activeElement; attempt += 1;
    }}>{retryLabel}</Button>{/if}
  </div>
  {#if phase === "ready" && transcript}
    <details><summary class={quietControl({ focus: "inset", class: "cursor-pointer p-[var(--reddb-spatial-inset-md)] text-caption" })}>{transcriptLabel}</summary>
      <pre class={styles.transcript()}>{transcript}</pre>
    </details>
  {/if}
</section>

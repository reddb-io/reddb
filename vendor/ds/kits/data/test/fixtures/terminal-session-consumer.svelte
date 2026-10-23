<script lang="ts">
  import { untrack } from "svelte";
  import TerminalSession from "../../src/TerminalSession.svelte";
  import type { TerminalEngineFactory, TerminalInteraction, TerminalSessionHandle } from "../../src/terminal-session";
  const { factory, session, interaction }: { factory: TerminalEngineFactory; session: (handle: TerminalSessionHandle | null) => void; interaction: (event: TerminalInteraction) => void } = $props();
  let engine = $state.raw(untrack(() => factory));
  let readOnly = $state(false);
  export function replace(next: TerminalEngineFactory) { engine = next; }
  export function lock() { readOnly = true; }
</script>
<TerminalSession label="Transport fixture" {engine} {readOnly} loading="eager" transcript="<script>caller text</script>"
  onsessionchange={session} oninteraction={interaction}/>

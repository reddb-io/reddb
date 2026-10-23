<script lang="ts">
  import { onMount } from 'svelte'
  import {
    Accordion, Alert, Badge, Button, CodeBlock, DescriptionList, Divider, Heading,
    Kbd, Link, LoadingIndicator, Logo, Navbar, Prose, Stack, StatusIndicator, Switch, Tabs, Text,
  } from '@reddb-io/design-system/base'
  import { ApplicationShell, SidebarPanel, Toolbar } from '@reddb-io/design-system/app'
  import CodeField from '@reddb-io/design-system/data/CodeField'
  import Table from '@reddb-io/design-system/data/Table'
  import { sqlGrammar } from './sql-grammar'
  import { BrowserError, openArtifact, type Artifact } from '@reddb-io/browser'
  import RedDBWorker from '@reddb-io/browser/worker?worker'

  let darkScheme = $state(true)
  let sql = $state('SELECT\n  6 * 7 AS resposta')
  let result = $state('Aguardando o engine…')
  let scalar = $state<{ columns: string[]; rows: unknown[][] }>()
  let resultView = $state('table')
  const columns = $derived(scalar?.columns.map((header, index) => ({
    key: String(index), header,
    format: (value: unknown) => typeof value === 'string' ? value : JSON.stringify(value) ?? String(value),
  })) ?? [])
  const rows = $derived(scalar?.rows.map(row => Object.fromEntries(row.map((value, index) => [String(index), value]))) ?? [])
  let error = $state('')
  let artifact: Artifact | undefined = $state()
  let busy = $state(true)
  let engineState = $state('Abrindo o arquivo local…')
  let engineTone = $state<'info' | 'success' | 'danger'>('info')
  let recordCount = $state('…')
  let generation = $state('…')
  let storageStatus = $state('Os bytes ficam no OPFS deste navegador e desta origem.')
  let networkState = $state('')
  let offlineState = $state('Preparando o cache da aplicação…')
  let offlineReady = $state(false)

  async function action(operation: () => Promise<void>) {
    if (busy) return
    busy = true
    error = ''
    try { await operation() }
    catch (failure) {
      const code = failure instanceof BrowserError ? failure.code : 'ERROR'
      const message = code === 'FILE_BUSY'
        ? 'Outra aba está usando este arquivo. Feche a outra aba e tente reabrir.'
        : code === 'STORAGE_UNSUPPORTED'
          ? 'Este navegador não oferece o armazenamento necessário. Experimente Chrome ou Firefox atual em HTTPS ou localhost.'
          : failure instanceof Error ? failure.message : String(failure)
      error = `${code}: ${message}`
    } finally { busy = false }
  }

  async function refreshFile() {
    if (!artifact) return
    const [payloads, current] = await Promise.all([artifact.readWal(), artifact.generation()])
    recordCount = String(payloads.length)
    generation = current
  }

  async function query() {
    if (!artifact) return
    scalar = undefined
    result = 'Executando no Worker…'
    try {
      if (sql.length > 10000) throw new Error('A consulta deve ter no máximo 10.000 caracteres.')
      scalar = await artifact.executeScalar(sql)
      result = JSON.stringify(scalar, null, 2)
    }
    catch (failure) { result = 'A consulta não foi executada.'; throw failure }
  }

  async function connect() {
    const previous = artifact
    artifact = undefined
    if (previous) await previous.close()
    engineState = 'Abrindo o arquivo local…'
    engineTone = 'info'
    const worker = new RedDBWorker()
    try {
      artifact = await openArtifact({ name: 'reddb-browser-playground-v1', worker })
      await refreshFile()
      engineState = 'WASM ativo · arquivo OPFS aberto'
      engineTone = 'success'
    } catch (failure) {
      worker.terminate()
      artifact = undefined
      engineState = 'Não foi possível abrir o arquivo'
      engineTone = 'danger'
      throw failure
    }
  }

  async function appendMarker() {
    // Opaque fixture bytes demonstrate artifact publication, not an engine WAL command.
    await artifact!.append([new Uint8Array([0x52, 0x44, 0x42, 0x01])])
    await refreshFile()
    storageStatus = 'Marcador gravado no OPFS. Reabra o arquivo ou recarregue a página para conferir.'
  }

  async function reopen() {
    await connect()
    storageStatus = 'Arquivo reaberto por um novo Worker. Contador lido dos bytes persistidos.'
  }

  async function prepareOffline() {
    try {
      if (import.meta.env.DEV) {
        offlineState = 'O cache offline é ativado no build publicado. Use pnpm build para testar localmente.'
        return
      }
      const registration = await navigator.serviceWorker.register(new URL('./service-worker.js', document.baseURI), { scope: './' })
      if (!registration.active && registration.installing) {
        const worker = registration.installing
        await new Promise<void>((resolve, reject) => {
          const changed = () => {
            if (worker.state !== 'activated' && worker.state !== 'redundant') return
            worker.removeEventListener('statechange', changed)
            if (worker.state === 'activated') resolve()
            else reject(new Error('Não foi possível guardar todos os assets para uso offline.'))
          }
          worker.addEventListener('statechange', changed)
          changed()
        })
      }
      await navigator.serviceWorker.ready
      if (!navigator.serviceWorker.controller) {
        await new Promise<void>(resolve => navigator.serviceWorker.addEventListener('controllerchange', () => resolve(), { once: true }))
      }
      offlineState = 'Cache pronto. Você pode desligar a rede e recarregar esta página.'
      offlineReady = true
    } catch (failure) {
      offlineState = `Cache offline indisponível: ${failure instanceof Error ? failure.message : String(failure)}`
    }
  }

  const examples = [
    { label: 'Cálculo', sql: 'SELECT 6 * 7 AS resposta' },
    { label: 'Texto', sql: "SELECT 'sem rede' AS modo, 42 AS resposta" },
    { label: 'Limite atual', sql: 'SELECT * FROM saves' },
  ]

  onMount(() => {
    const network = () => { networkState = navigator.onLine ? 'Rede disponível' : 'Navegador sem rede' }
    window.addEventListener('online', network)
    window.addEventListener('offline', network)
    network()
    busy = false
    void action(async () => { await connect(); await query() })
    void prepareOffline()
    return () => {
      window.removeEventListener('online', network)
      window.removeEventListener('offline', network)
      void artifact?.close()
    }
  })
</script>

{#snippet fileName()}<code class="font-mono text-caption break-all">reddb-browser-playground-v1</code>{/snippet}
{#snippet countValue()}<span id="record-count">{recordCount}</span>{/snippet}
{#snippet generationValue()}<span id="generation">{generation}</span>{/snippet}


<ApplicationShell fullWidth class="md:h-dvh md:overflow-hidden">
  {#snippet header()}
    <Navbar label="RedDB Browser" menuLabel="Menu" links={[
      { id: 'playground', label: 'Consulta', href: '#consulta', current: 'page' },
      { id: 'guide', label: 'Guia JS/TS', href: '../#/clients/browser' },
      { id: 'source', label: 'Código-fonte', href: 'https://github.com/reddb-io/reddb/tree/main/packages/browser-example' },
    ]}>
      {#snippet brand()}
        <Stack direction="row" align="center" gap="sm">
          <Logo layout="symbol" size={16} href="../" label="RedDB: documentação" fetchpriority="high" />
          <h1 class="text-body font-medium">Browser playground</h1>
        </Stack>
      {/snippet}
      {#snippet actions()}
        <Badge>0.1.0-alpha.0</Badge>
        <Switch label="Tema escuro" bind:checked={darkScheme} oncheckedchange={(checked) => { document.documentElement.dataset.colorScheme = checked ? 'dark' : 'light' }} />
      {/snippet}
    </Navbar>
  {/snippet}

  <div class="@container/browser h-full min-h-0">
    <div class="browser-regions h-full min-h-0 grid-cols-1 @3xl/browser:grid-cols-[minmax(14rem,1fr)_minmax(0,4fr)] @3xl/browser:grid-rows-[minmax(0,1fr)]">
      <section id="consulta" class="min-h-0 min-w-0 @3xl/browser:col-start-2 @3xl/browser:row-start-1" aria-label="Consulta RQL e resultado">
        <Stack gap="none" class="h-full min-h-0" onkeydown={(event) => {
          if (!event.defaultPrevented && (event.ctrlKey || event.metaKey) && event.key === 'Enter') { event.preventDefault(); void action(query) }
        }}>
          <form id="query-form" class="browser-editor" onsubmit={(event) => { event.preventDefault(); void action(query) }}>
            <Stack direction="row" align="center" justify="between" gap="sm" wrap class="browser-toolbar">
              <h2 class="text-body font-medium">Consulta RQL</h2>
              <Stack direction="row" align="center" gap="sm" wrap>
                <Toolbar label="Exemplos de consulta" size="sm" items={examples.map(example => ({
                  id: example.label, label: example.label, onselect: () => { sql = example.sql },
                }))} />
                <Kbd keys={['Ctrl', 'Enter']} size="sm" />
                <Button id="execute" variant="primary" size="sm" type="submit" disabled={busy || !artifact || sql.length > 10000}>Executar consulta</Button>
              </Stack>
            </Stack>
            <CodeField id="sql" label="Consulta RQL" bind:value={sql} grammar={sqlGrammar} gutter rows={6} class="browser-code-field [&>label]:sr-only" />
            <Text role="caption" class="browser-editor-note">SELECTs escalares no Worker. Coleções salvas e transações ainda estão em desenvolvimento.</Text>
          </form>
          <Alert id="error" tone="danger" hidden={!error}>{error}</Alert>
          <section class="min-h-0 min-w-0 flex-1" aria-label="Resultado da consulta" id="query-result">
            <Tabs label="Visualização do resultado" bind:value={resultView} items={[
              { value: 'table', label: 'Resultado' },
              { value: 'json', label: 'JSON' },
            ]} class="browser-results" listClass="browser-result-tabs" contentClass="browser-result-content">
              {#snippet children({ value })}
                <div class="browser-result-summary" role="status">
                  {#if busy}<LoadingIndicator label="Executando no Worker" size="sm" />
                  {:else if scalar}<StatusIndicator tone="success" label={`${scalar.rows.length} linha · execução local`} />
                  {:else}<Text role="caption">{result}</Text>{/if}
                </div>
                {#if value === 'table'}
                  {#if scalar}
                    <Table caption="Resultado da consulta RQL" captionHidden columnVisibility={false} {columns} {rows} />
                  {/if}
                {:else}
                  <div class="browser-json"><CodeBlock code={result} language="JSON" copyLabel="Copiar resultado" copiedLabel="Copiado" copyErrorLabel="Falha ao copiar. Tente novamente." regionLabel="Resultado da consulta em JSON" /></div>
                {/if}
              {/snippet}
            </Tabs>
          </section>
        </Stack>
      </section>

      <aside aria-label="Persistência local e uso offline" class="min-h-0 min-w-0 @3xl/browser:col-start-1 @3xl/browser:row-start-1">
        <SidebarPanel title="Arquivo local" class="h-full">
          <Stack gap="layout-sm" class="browser-file-content">
            <DescriptionList gap="sm" items={[
              { term: 'Artefato OPFS', detail: fileName },
              { term: 'Marcadores persistidos', detail: countValue },
              { term: 'Geração do artefato', detail: generationValue },
            ]} />
            <Stack gap="sm">
              <Button id="append" size="sm" disabled={busy || !artifact} onclick={() => void action(appendMarker)}>Gravar marcador</Button>
              <Button id="reopen" variant="ghost" size="sm" disabled={busy} onclick={() => void action(reopen)}>Reabrir arquivo</Button>
            </Stack>
            <div id="storage-status" role="status"><Text role="caption">{storageStatus}</Text></div>
            <Text role="caption">Marcador binário de teste; não é um registro ou transação.</Text>
            <Divider />
            <section aria-labelledby="offline-heading">
              <Stack gap="layout-sm">
                <Heading id="offline-heading" level={3}>Testar sem rede</Heading>
                <div id="offline-state" role="status"><Text role="caption">{offlineState}</Text></div>
                <Button id="reload" size="sm" disabled={!offlineReady} onclick={() => window.location.reload()}>Recarregar página</Button>
                <Accordion label="Instruções para teste offline" items={[{ value: 'offline', label: 'Como testar offline' }]}>
                  <Prose>
                    <ol>
                      <li>Grave um marcador e aguarde o cache.</li>
                      <li>Desligue a rede do navegador.</li>
                      <li>Recarregue e confira o contador.</li>
                    </ol>
                  </Prose>
                  <Text role="caption">Limpar os dados do site remove o arquivo e o cache. As políticas de quota do navegador se aplicam.</Text>
                </Accordion>
              </Stack>
            </section>
          </Stack>
        </SidebarPanel>
      </aside>
    </div>
  </div>

  {#snippet footer()}
    <Stack direction="row" align="center" justify="between" gap="sm" wrap class="browser-statusbar">
      <StatusIndicator id="engine-state" tone={engineTone} label={engineState} />
      <Text id="network-state" as="span" role="caption">{networkState}</Text>
      <Link href="../#/clients/browser">Integrar com JS/TS</Link>
    </Stack>
  {/snippet}
</ApplicationShell>

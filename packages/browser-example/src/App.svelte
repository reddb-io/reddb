<script lang="ts">
  import { onMount } from 'svelte'
  import {
    Accordion, Alert, Badge, Button, CodeBlock, Container, DescriptionList, Divider, Field, Heading,
    Link, Logo, Navbar, Prose, SectionHeading, Stack, Switch, Text, Textarea,
  } from '@reddb-io/design-system/base'
  import { ApplicationShell, SidebarLayout, SidebarPanel, Toolbar } from '@reddb-io/design-system/app'
  import { BrowserError, openArtifact, type Artifact } from '@reddb-io/browser'
  import RedDBWorker from '@reddb-io/browser/worker?worker'

  let darkScheme = $state(false)
  let sql = $state('SELECT 6 * 7 AS resposta')
  let result = $state('Aguardando o engine…')
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
    result = 'Executando no Worker…'
    try { result = JSON.stringify(await artifact.executeScalar(sql), null, 2) }
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


<ApplicationShell fullWidth>
  {#snippet header()}
    <Navbar label="RedDB Browser" menuLabel="Menu" links={[
      { id: 'playground', label: 'Browser', href: '#consulta', current: 'page' },
      { id: 'guide', label: 'Guia JS/TS', href: '../#/clients/browser' },
      { id: 'source', label: 'Código-fonte', href: 'https://github.com/reddb-io/reddb/tree/main/packages/browser-example' },
    ]}>
      {#snippet brand()}<Logo href="../" label="RedDB: documentação" fetchpriority="high" />{/snippet}
      {#snippet actions()}
        <Switch label="Tema escuro" bind:checked={darkScheme} oncheckedchange={(checked) => { document.documentElement.dataset.colorScheme = checked ? 'dark' : 'light' }} />
      {/snippet}
    </Navbar>
  {/snippet}

  <Container size="wide" class="playground-workspace">
    <Stack gap="layout-md">
      <Stack gap="sm">
        <Stack direction="row" align="center" gap="md" wrap>
          <Heading level={1}>Browser playground</Heading>
          <Badge>0.1.0-alpha.0</Badge>
        </Stack>
        <Text ink="muted">RedDB em Rust/WASM, executado neste navegador. Sem servidor ou conta.</Text>
      </Stack>

      <SidebarLayout side="end" sidebarLabel="Persistência local e uso offline">
        <Stack gap="layout-md">
          <div class="playground-runtime" aria-live="polite">
            <Stack direction="row" justify="between" align="center" gap="sm" wrap>
              <Badge id="engine-state" tone={engineTone}>{engineState}</Badge>
              <Text id="network-state" as="span" role="caption">{networkState}</Text>
            </Stack>
          </div>
          <Alert id="error" tone="danger" hidden={!error}>{error}</Alert>
          <section id="consulta" aria-labelledby="query-heading">
            <Stack gap="layout-sm">
              <SectionHeading title="Editor RQL" titleId="query-heading" description="Selecione um exemplo ou escreva um SELECT escalar." size="sm">
                {#snippet actions()}
                  <Toolbar label="Exemplos de consulta" items={examples.map(example => ({
                    id: example.label, label: example.label, onselect: () => { sql = example.sql },
                  }))} />
                {/snippet}
              </SectionHeading>
              <div class="@container/query">
                <div class="playground-query grid-cols-1 @3xl/query:grid-cols-2">
                  <form id="query-form" onsubmit={(event) => { event.preventDefault(); void action(query) }}>
                    <Stack gap="layout-sm">
                      <Field id="sql" label="Consulta RQL">
                        {#snippet children(control)}
                          <Textarea {...control} class="font-mono" bind:value={sql} rows={12} maxlength={10000} spellcheck={false} />
                        {/snippet}
                      </Field>
                      <Stack direction="row" align="center" gap="md" wrap>
                        <Button id="execute" variant="primary" type="submit" disabled={busy || !artifact}>Executar consulta</Button>
                        <Text as="span" role="caption">Execução local em um Worker</Text>
                      </Stack>
                    </Stack>
                  </form>
                  <section aria-labelledby="result-heading">
                    <Stack gap="sm">
                      <h3 id="result-heading" class="text-body font-medium">Resultado</h3>
                      <div id="query-result" aria-live="polite"><CodeBlock code={result} language="JSON" copyLabel="Copiar resultado" copiedLabel="Copiado" copyErrorLabel="Falha ao copiar. Tente novamente." regionLabel="Resultado da consulta" /></div>
                    </Stack>
                  </section>
                </div>
              </div>
              <Text role="caption" class="max-w-prose">Esta alpha executa SELECTs escalares e persiste artefatos. Consultas sobre coleções salvas e transações ainda estão em desenvolvimento.</Text>
            </Stack>
          </section>
        </Stack>

        {#snippet sidebar()}
          <SidebarPanel title="Arquivo local">
            <Stack gap="layout-sm" class="playground-sidebar-content">
              <Text role="caption">Artefato OPFS deste navegador e desta origem.</Text>
              <DescriptionList gap="sm" items={[
                { term: 'Arquivo', detail: fileName },
                { term: 'Marcadores persistidos', detail: countValue },
                { term: 'Geração do artefato', detail: generationValue },
              ]} />
              <Stack gap="sm">
                <Button id="append" disabled={busy || !artifact} onclick={() => void action(appendMarker)}>Gravar marcador</Button>
                <Button id="reopen" variant="ghost" disabled={busy} onclick={() => void action(reopen)}>Reabrir arquivo</Button>
              </Stack>
              <div id="storage-status" role="status"><Text role="caption">{storageStatus}</Text></div>
              <Text role="caption">Marcador de teste em bytes; não é um registro ou transação.</Text>
              <Divider />
              <section aria-labelledby="offline-heading">
                <Stack gap="layout-sm">
                  <Heading id="offline-heading" level={3}>Testar sem rede</Heading>
                  <div id="offline-state" role="status"><Text role="caption">{offlineState}</Text></div>
                  <Button id="reload" disabled={!offlineReady} onclick={() => window.location.reload()}>Recarregar página</Button>
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
        {/snippet}
      </SidebarLayout>
    </Stack>
  </Container>

  {#snippet footer()}
    <Container size="wide">
      <Stack direction="row" align="center" justify="between" gap="sm" wrap>
        <Text as="span" role="caption">Worker · WebAssembly · OPFS</Text>
        <Link href="../#/clients/browser">Integrar no seu projeto JS/TS</Link>
        <Text as="span" role="caption">Chromium e Firefox verificados. Safari/iOS sem qualificação.</Text>
      </Stack>
    </Container>
  {/snippet}
</ApplicationShell>

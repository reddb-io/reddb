<script lang="ts">
  import { onMount } from 'svelte'
  import {
    Alert, Badge, Button, CodeBlock, Container, DescriptionList, Divider, Field, Heading,
    Link, Logo, Prose, SectionHeading, SkipLink, Stack, Text, Textarea,
  } from '@reddb-io/design-system/base'
  import { BrowserError, openArtifact, type Artifact } from '@reddb-io/browser'
  import RedDBWorker from '@reddb-io/browser/worker?worker'

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

{#snippet fileName()}<code class="font-mono break-all">reddb-browser-playground-v1</code>{/snippet}
{#snippet countValue()}<span id="record-count">{recordCount}</span>{/snippet}
{#snippet generationValue()}<span id="generation">{generation}</span>{/snippet}


<SkipLink href="#main-content">Ir para o conteúdo</SkipLink>
<header class="playground-header">
  <Container>
    <Stack direction="row" align="center" justify="between" wrap>
      <Stack direction="row" align="center" gap="md">
        <Logo href="../#/clients/browser" label="reddb.io: guia Browser" fetchpriority="high" />
        <Text as="span" ink="muted">Browser</Text>
      </Stack>
      <Link href="https://github.com/reddb-io/reddb/tree/main/packages/browser-example">Ver código TS</Link>
    </Stack>
  </Container>
</header>
<Divider />
<main id="main-content" class="playground-main" tabindex="-1">
  <Container>
    <Stack gap="section">
      <Stack>
        <div><Badge>Playground · 0.1.0-alpha.0</Badge></div>
        <Heading level={1}>O engine roda aqui.</Heading>
        <Text class="max-w-prose">Execute uma consulta em Rust/WASM, grave bytes no arquivo local e reabra sem rede. Sem servidor de banco ou conta.</Text>
        <Text role="caption" class="max-w-prose">Esta alpha executa SELECTs escalares e persiste artefatos. Consultas sobre coleções salvas e transações ainda estão em desenvolvimento.</Text>
      </Stack>
      <div class="playground-runtime" aria-live="polite">
        <Stack direction="row" justify="between" align="center" wrap>
          <Badge id="engine-state" tone={engineTone}>{engineState}</Badge>
          <Text id="network-state" as="span" role="caption">{networkState}</Text>
        </Stack>
      </div>
      <Alert id="error" tone="danger" hidden={!error}>{error}</Alert>
      <section id="consulta" class="playground-section" aria-labelledby="query-heading">
        <Stack>
          <SectionHeading title="01 · Execute no WASM" titleId="query-heading" description="O mesmo parser e avaliador escalar do RedDB nativo, dentro de um Worker." />
          <Stack direction="row" gap="sm" wrap aria-label="Exemplos de consulta">
            {#each examples as example}
              <Button onclick={() => { sql = example.sql }}>{example.label}</Button>
            {/each}
          </Stack>
          <div class="playground-query">
            <form id="query-form" onsubmit={(event) => { event.preventDefault(); void action(query) }}>
              <Stack>
                <Field id="sql" label="Consulta RQL">
                  {#snippet children(control)}
                    <Textarea {...control} bind:value={sql} rows={6} maxlength={10000} spellcheck={false} />
                  {/snippet}
                </Field>
                <div><Button id="execute" variant="primary" type="submit" disabled={busy || !artifact}>Executar consulta</Button></div>
              </Stack>
            </form>
            <Stack>
              <Heading level={3}>Resultado</Heading>
              <div id="query-result" aria-live="polite"><CodeBlock code={result} language="JSON" copyLabel="Copiar resultado" copiedLabel="Copiado" copyErrorLabel="Falha ao copiar. Tente novamente." regionLabel="Resultado da consulta" /></div>
            </Stack>
          </div>
        </Stack>
      </section>
      <section class="playground-section" aria-labelledby="storage-heading">
        <Stack>
          <SectionHeading title="02 · Persista e reabra" titleId="storage-heading" />
          <Text class="max-w-prose">O marcador é um payload binário opaco de teste no artefato <code>.rdb</code>. Demonstra a persistência do arquivo, sem representar uma linha, documento ou transação.</Text>
          <DescriptionList items={[
            { term: 'Arquivo de teste', detail: fileName },
            { term: 'Marcadores persistidos', detail: countValue },
            { term: 'Geração do artefato', detail: generationValue },
          ]} />
          <Stack direction="row" gap="sm" wrap>
            <Button id="append" variant="primary" disabled={busy || !artifact} onclick={() => void action(appendMarker)}>Gravar marcador</Button>
            <Button id="reopen" disabled={busy} onclick={() => void action(reopen)}>Reabrir arquivo</Button>
          </Stack>
          <div id="storage-status" role="status"><Text role="caption" class="max-w-prose">{storageStatus}</Text></div>
        </Stack>
      </section>
      <section class="playground-section" aria-labelledby="offline-heading">
        <Stack>
          <SectionHeading title="03 · Desligue a rede" titleId="offline-heading" />
          <div id="offline-state" role="status"><Text class="max-w-prose">{offlineState}</Text></div>
          <Prose>
            <ol>
              <li>Grave um marcador e aguarde o cache ficar pronto.</li>
              <li>Desligue a rede ou selecione <strong>Offline</strong> no painel Network do navegador.</li>
              <li>Recarregue esta página. O contador deve continuar igual e as consultas devem funcionar.</li>
            </ol>
          </Prose>
          <div><Button id="reload" disabled={!offlineReady} onclick={() => window.location.reload()}>Recarregar página</Button></div>
          <Text role="caption" class="max-w-prose">O cache inclui a página, o Worker e o WASM. Limpar os dados do site remove o arquivo e o cache; as políticas de quota do navegador se aplicam.</Text>
        </Stack>
      </section>
      <Divider />
      <footer>
        <Stack direction="row" justify="between" wrap>
          <Link href="../#/clients/browser">Guia de integração JS/TS</Link>
          <Text role="caption" class="max-w-prose">Chromium e Firefox verificados. Safari/iOS ainda sem qualificação.</Text>
        </Stack>
      </footer>
    </Stack>
  </Container>
</main>

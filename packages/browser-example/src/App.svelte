<script lang="ts">
  import { onMount } from 'svelte'
  import {
    Accordion, Alert, Badge, Button, CodeBlock, DescriptionList, Divider, Field, Heading,
    Kbd, Link, LoadingIndicator, Logo, Navbar, Prose, Stack, StatusIndicator, Switch, Tabs, Text, Textarea,
  } from '@reddb-io/design-system/base'
  import { ApplicationShell, SidebarPanel, Toolbar } from '@reddb-io/design-system/app'
  import CodeField from '@reddb-io/design-system/data/CodeField'
  import Table from '@reddb-io/design-system/data/Table'
  import { sqlGrammar } from './sql-grammar'
  import { BrowserError, openDatabase, type Database } from '@reddb-io/browser'
  import RedDBWorker from '@reddb-io/browser/worker?worker'
  import { version as browserVersion } from '@reddb-io/browser/package.json'

  let darkScheme = $state(true)
  const databaseName = 'reddb-browser-sql-playground-v1'
  const createNotes = 'CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)'
  const selectNotes = 'SELECT id, body FROM notes ORDER BY id'
  let sql = $state(selectNotes)
  let parameters = $state('[]')
  let result = $state('Aguardando o engine…')
  let queryResult = $state<{ columns: string[]; rows: unknown[][] }>()
  let resultView = $state('table')
  const columns = $derived(queryResult?.columns.map((header, index) => ({
    key: String(index), header,
    format: (value: unknown) => typeof value === 'string' ? value : JSON.stringify(value) ?? String(value),
  })) ?? [])
  const rows = $derived(queryResult?.rows.map(row => Object.fromEntries(row.map((value, index) => [String(index), value]))) ?? [])
  let error = $state('')
  let database: Database | undefined = $state()
  let busy = $state(true)
  let engineState = $state('Abrindo o arquivo local…')
  let engineTone = $state<'info' | 'success' | 'danger'>('info')
  let storageStatus = $state('O banco fica no OPFS deste navegador e desta origem.')
  let storageRetention = $state('Consultando a política de armazenamento…')
  let storageEstimate = $state('')
  let persistenceAvailable = $state(false)
  let persistenceGranted = $state(false)
  let networkState = $state('')
  let offlineState = $state('Preparando o cache da aplicação…')
  let offlineReady = $state(false)
  let pendingUpdate = $state<ServiceWorker>()

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

  async function query() {
    if (!database) return
    queryResult = undefined
    result = 'Executando no Worker…'
    try {
      if (sql.length > 10000) throw new Error('A consulta deve ter no máximo 10.000 caracteres.')
      if (parameters.length > 10000) throw new BrowserError('INVALID_ARGUMENT', 'Os parâmetros devem ter no máximo 10.000 caracteres.')
      let values: unknown
      try { values = JSON.parse(parameters) }
      catch { throw new BrowserError('INVALID_ARGUMENT', 'Os parâmetros devem ser um array JSON válido, como [1, "Minha nota"].') }
      if (!Array.isArray(values)) throw new BrowserError('INVALID_ARGUMENT', 'Os parâmetros devem ser um array JSON, como [1, "Minha nota"].')
      queryResult = await database.query(sql, values)
      result = JSON.stringify(queryResult, null, 2)
      storageStatus = 'Comando concluído. Dentro de BEGIN, use COMMIT para persistir as alterações.'
    }
    catch (failure) { result = 'Não foi possível confirmar a execução. Consulte o erro abaixo do editor.'; throw failure }
  }

  async function connect() {
    const previous = database
    database = undefined
    if (previous) await previous.close()
    engineState = 'Abrindo o arquivo local…'
    engineTone = 'info'
    const worker = new RedDBWorker()
    try {
      const opened = await openDatabase({ name: databaseName, create: true, worker })
      try { await opened.query(createNotes) }
      catch (failure) { await opened.close(); throw failure }
      database = opened
      engineState = 'WASM ativo · banco OPFS aberto'
      engineTone = 'success'
    } catch (failure) {
      worker.terminate()
      database = undefined
      engineState = 'Não foi possível abrir o banco'
      engineTone = 'danger'
      throw failure
    }
  }

  async function transaction(command: 'BEGIN' | 'COMMIT' | 'ROLLBACK') {
    sql = command
    parameters = '[]'
    await query()
    storageStatus = command === 'BEGIN'
      ? 'BEGIN concluído. Use COMMIT ou ROLLBACK para encerrar a transação.'
      : command === 'COMMIT'
        ? 'COMMIT concluído. Alterações confirmadas no OPFS.'
        : 'ROLLBACK concluído. Alterações pendentes descartadas.'
  }

  async function reopen() {
    await connect()
    sql = selectNotes
    parameters = '[]'
    await query()
    storageStatus = 'Banco reaberto por um novo Worker. Alterações sem COMMIT foram descartadas.'
  }

  async function inspectStorage() {
    persistenceAvailable = typeof navigator.storage?.persist === 'function'
    try {
      persistenceGranted = await navigator.storage?.persisted?.() ?? false
      storageRetention = persistenceGranted
        ? 'Armazenamento persistente concedido. Limpar os dados do site ainda apaga o banco.'
        : 'O navegador pode descartar os dados sob pressão de espaço. Mantenha um backup.'
      const estimate = await navigator.storage?.estimate?.()
      if (estimate?.usage !== undefined && estimate.quota !== undefined) {
        const format = new Intl.NumberFormat('pt-BR', { maximumFractionDigits: 1 })
        storageEstimate = `Uso estimado desta origem: ${format.format(estimate.usage / 1048576)} MB de ${format.format(estimate.quota / 1048576)} MB.`
      }
    } catch { storageRetention = 'Não foi possível consultar a retenção e a quota do navegador.' }
  }

  async function requestPersistence() {
    const granted = await navigator.storage.persist()
    await inspectStorage()
    if (!granted) storageRetention = 'O navegador não concedeu armazenamento persistente. Os dados continuam locais, sujeitos a descarte.'
  }

  async function activateUpdate() {
    const worker = pendingUpdate
    if (!worker) return
    await database?.close()
    database = undefined
    if (navigator.serviceWorker.controller !== worker) {
      await new Promise<void>(resolve => {
        navigator.serviceWorker.addEventListener('controllerchange', () => resolve(), { once: true })
        worker.postMessage({ type: 'ACTIVATE_UPDATE' })
      })
    }
    window.location.reload()
  }

  async function prepareOffline() {
    try {
      if (import.meta.env.DEV) {
        offlineState = 'O cache offline é ativado no build publicado. Use pnpm build para testar localmente.'
        return
      }
      const registration = await navigator.serviceWorker.register(new URL('./service-worker.js', document.baseURI), { scope: './' })
      const offerUpdate = () => {
        if (!registration.waiting || !navigator.serviceWorker.controller) return
        pendingUpdate = registration.waiting
        offlineReady = false
        offlineState = 'Ative a atualização disponível para preparar esta versão para uso offline.'
      }
      const watchInstalling = () => {
        const worker = registration.installing
        if (!worker) return
        const changed = () => {
          if (worker.state !== 'installed' && worker.state !== 'redundant') return
          worker.removeEventListener('statechange', changed)
          offerUpdate()
        }
        worker.addEventListener('statechange', changed)
        changed()
      }
      registration.addEventListener('updatefound', watchInstalling)
      watchInstalling()
      // An update check may fail offline while the installed application remains usable.
      if (registration.active && navigator.onLine) void registration.update().catch(() => {})
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
      offerUpdate()
    } catch (failure) {
      offlineState = `Cache offline indisponível: ${failure instanceof Error ? failure.message : String(failure)}`
    }
  }

  const examples = [
    { label: 'Ler', sql: selectNotes, params: [] },
    { label: 'Inserir', sql: 'INSERT INTO notes (id, body) VALUES ($1, $2)', params: [1, 'Minha nota offline'] },
    { label: 'Alterar', sql: 'UPDATE notes SET body = $1 WHERE id = $2', params: ['Nota atualizada offline', 1] },
    { label: 'Excluir', sql: 'DELETE FROM notes WHERE id = $1', params: [1] },
    { label: 'Criar tabela', sql: createNotes, params: [] },
  ]

  onMount(() => {
    const network = () => { networkState = navigator.onLine ? 'Rede disponível' : 'Navegador sem rede' }
    window.addEventListener('online', network)
    window.addEventListener('offline', network)
    network()
    busy = false
    void action(async () => { await connect(); await query() })
    void prepareOffline()
    void inspectStorage()
    return () => {
      window.removeEventListener('online', network)
      window.removeEventListener('offline', network)
      void database?.close()
    }
  })
</script>

{#snippet fileName()}<code class="font-mono text-caption break-all">{databaseName}</code>{/snippet}


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
        <Badge>{browserVersion}</Badge>
        <Switch label="Tema escuro" bind:checked={darkScheme} oncheckedchange={(checked) => { document.documentElement.dataset.colorScheme = checked ? 'dark' : 'light' }} />
      {/snippet}
    </Navbar>
  {/snippet}

  <div class="@container/browser h-full min-h-0 overflow-auto">
    <div class="browser-regions min-h-full min-h-0 auto-rows-max grid-cols-1 @3xl/browser:h-full @3xl/browser:auto-rows-auto @3xl/browser:grid-cols-[minmax(14rem,1fr)_minmax(0,4fr)] @3xl/browser:grid-rows-[minmax(0,1fr)]">
      <section id="consulta" class="min-h-0 min-w-0 @3xl/browser:col-start-2 @3xl/browser:row-start-1" aria-label="Consulta RQL e resultado">
        <Stack gap="none" class="h-full min-h-0" onkeydown={(event) => {
          if (!event.defaultPrevented && (event.ctrlKey || event.metaKey) && event.key === 'Enter') { event.preventDefault(); void action(query) }
        }}>
          <Alert id="app-update" tone="info" hidden={!pendingUpdate}>
            <Stack direction="row" align="center" justify="between" gap="sm" wrap>
              <Text>Nova versão disponível. Execute COMMIT antes de atualizar; alterações pendentes serão descartadas.</Text>
              <Button id="update-app" disabled={busy || !pendingUpdate} onclick={() => void action(activateUpdate)}>Atualizar aplicação</Button>
            </Stack>
          </Alert>
          <form id="query-form" class="browser-editor" onsubmit={(event) => { event.preventDefault(); void action(query) }}>
            <Stack direction="row" align="center" justify="between" gap="sm" wrap class="browser-toolbar">
              <h2 class="text-body font-medium">Consulta RQL</h2>
              <Stack direction="row" align="center" gap="sm" wrap>
                <Toolbar label="Exemplos de consulta" size="sm" class="min-w-0 max-w-full flex-wrap" items={examples.map(example => ({
                  id: example.label, label: example.label, onselect: () => { sql = example.sql; parameters = JSON.stringify(example.params) },
                }))} />
                <Kbd keys={['Ctrl', 'Enter']} size="sm" />
                <Button id="execute" variant="primary" size="sm" type="submit" disabled={busy || !database || sql.length > 10000 || parameters.length > 10000}>Executar consulta</Button>
              </Stack>
            </Stack>
            <CodeField id="sql" label="Consulta RQL" bind:value={sql} grammar={sqlGrammar} gutter rows={6} class="browser-code-field [&>label]:sr-only" />
            <Field id="parameters" label="Parâmetros (JSON)" help="Array de valores para $1, $2 ou ?1, ?2. Use [] sem parâmetros. Aceita texto, números seguros, booleanos e null." class="browser-parameters">
              {#snippet children(control)}
                <Textarea {...control} bind:value={parameters} rows={2} spellcheck={false} class="font-mono" />
              {/snippet}
            </Field>
            <Text role="caption" class="browser-editor-note">SQL local no WASM. Os exemplos preenchem o editor; execute um comando por vez. A tabela notes começa vazia.</Text>
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
                  {:else if queryResult}<StatusIndicator tone="success" label={`${queryResult.rows.length} linha(s) no resultado · execução local`} />
                  {:else}<Text role="caption">{result}</Text>{/if}
                </div>
                {#if value === 'table'}
                  {#if queryResult}
                    <Table caption="Resultado da consulta RQL" captionHidden columnVisibility={false} emptyMessage="Sem linhas. Use Inserir para criar uma nota." {columns} {rows} />
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
        <SidebarPanel title="Banco local" class="h-full">
          <Stack gap="layout-sm" class="browser-file-content">
            <DescriptionList gap="sm" items={[
              { term: 'Banco OPFS', detail: fileName },
              { term: 'Acesso', detail: 'Exclusivo desta aba' },
            ]} />
            <Stack gap="sm">
              <Toolbar label="Transação SQL" size="sm" class="min-w-0 max-w-full flex-wrap" items={(['BEGIN', 'COMMIT', 'ROLLBACK'] as const).map(command => ({
                id: command, label: command, disabled: busy || !database, onselect: () => void action(() => transaction(command)),
              }))} />
              <Button id="reopen" variant="ghost" size="sm" disabled={busy} onclick={() => void action(reopen)}>Reabrir banco</Button>
            </Stack>
            <div id="storage-status" role="status"><Text role="caption">{storageStatus}</Text></div>
            <Text role="caption">Após um erro em BEGIN, execute ROLLBACK. Reabrir, atualizar ou fechar a página descarta alterações sem COMMIT.</Text>
            <Text role="caption">O arquivo de marcadores do exemplo anterior é preservado.</Text>
            <Divider />
            <section aria-labelledby="retention-heading">
              <Stack gap="sm">
                <Heading id="retention-heading" level={3}>Retenção dos dados</Heading>
                <div id="storage-retention" role="status"><Text role="caption">{storageRetention}</Text></div>
                {#if storageEstimate}<Text id="storage-estimate" role="caption">{storageEstimate}</Text>{/if}
                <Button id="persist-storage" size="sm" variant="ghost" disabled={busy || !persistenceAvailable || persistenceGranted} onclick={() => void action(requestPersistence)}>Solicitar armazenamento persistente</Button>
                <Text role="caption">F5 preserva os commits. Use o mesmo site, navegador e perfil. A permissão de persistência é decidida pelo navegador.</Text>
              </Stack>
            </section>
            <Divider />
            <section aria-labelledby="offline-heading">
              <Stack gap="layout-sm">
                <Heading id="offline-heading" level={3}>Testar sem rede</Heading>
                <div id="offline-state" role="status"><Text role="caption">{offlineState}</Text></div>
                <Button id="reload" size="sm" disabled={!offlineReady} onclick={() => window.location.reload()}>Recarregar página</Button>
                <Accordion label="Instruções para teste offline" items={[{ value: 'offline', label: 'Como testar offline' }]}>
                  <Prose>
                    <ol>
                      <li>Execute Inserir e confirme com COMMIT se iniciou BEGIN.</li>
                      <li>Aguarde o cache ficar pronto.</li>
                      <li>Desligue a rede do navegador.</li>
                      <li>Recarregue e use Ler para conferir suas notas.</li>
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
      <StatusIndicator id="engine-state" tone={engineTone} label={engineState} aria-live="polite" />
      <Text id="network-state" as="span" role="caption">{networkState}</Text>
      <Link href="../#/clients/browser">Integrar com JS/TS</Link>
    </Stack>
  {/snippet}
</ApplicationShell>

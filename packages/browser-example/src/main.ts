import { BrowserError, openArtifact, type Artifact } from '@reddb-io/browser'
import RedDBWorker from '@reddb-io/browser/worker?worker'
import './style.css'

function element<T extends HTMLElement>(selector: string): T {
  const node = document.querySelector<T>(selector)
  if (!node) throw new Error(`Missing application element: ${selector}`)
  return node
}

const sql = element<HTMLTextAreaElement>('#sql')
const execute = element<HTMLButtonElement>('#execute')
const append = element<HTMLButtonElement>('#append')
const reopen = element<HTMLButtonElement>('#reopen')
const result = element('#query-result')
const error = element('#error')
let artifact: Artifact | undefined
let busy = false

function updateControls() {
  execute.disabled = append.disabled = busy || !artifact
  reopen.disabled = busy
}

async function action(operation: () => Promise<void>) {
  if (busy) return
  busy = true
  error.hidden = true
  updateControls()
  try { await operation() }
  catch (failure) {
    const code = failure instanceof BrowserError ? failure.code : 'ERROR'
    const message = code === 'FILE_BUSY'
      ? 'Outra aba está usando este arquivo. Feche a outra aba e tente reabrir.'
      : code === 'STORAGE_UNSUPPORTED'
        ? 'Este navegador não oferece o armazenamento necessário. Experimente Chrome ou Firefox atual em HTTPS ou localhost.'
        : failure instanceof Error ? failure.message : String(failure)
    error.textContent = `${code}: ${message}`
    error.hidden = false
  } finally {
    busy = false
    updateControls()
  }
}

async function refreshFile() {
  if (!artifact) return
  const [payloads, generation] = await Promise.all([artifact.readWal(), artifact.generation()])
  element('#record-count').textContent = String(payloads.length)
  element('#generation').textContent = generation
}

async function query() {
  if (!artifact) return
  result.textContent = 'Executando no Worker…'
  try { result.textContent = JSON.stringify(await artifact.executeScalar(sql.value), null, 2) }
  catch (failure) { result.textContent = 'A consulta não foi executada.'; throw failure }
}

async function connect() {
  const previous = artifact
  artifact = undefined
  if (previous) await previous.close()
  element('#engine-state').textContent = 'Abrindo o arquivo local…'
  const worker = new RedDBWorker()
  try {
    artifact = await openArtifact({ name: 'reddb-browser-playground-v1', worker })
    await refreshFile()
    element('#engine-state').textContent = 'WASM ativo · arquivo OPFS aberto'
  } catch (failure) {
    worker.terminate()
    artifact = undefined
    element('#engine-state').textContent = 'Não foi possível abrir o arquivo'
    throw failure
  }
}

const examples: Record<string, string> = {
  calculation: 'SELECT 6 * 7 AS resposta',
  text: "SELECT 'sem rede' AS modo, 42 AS resposta",
  unsupported: 'SELECT * FROM saves',
}
for (const button of document.querySelectorAll<HTMLButtonElement>('[data-example]')) {
  button.addEventListener('click', () => { sql.value = examples[button.dataset.example!]; sql.focus() })
}
element<HTMLFormElement>('#query-form').addEventListener('submit', event => {
  event.preventDefault()
  void action(query)
})
append.addEventListener('click', () => void action(async () => {
  // Opaque fixture bytes demonstrate artifact publication only, not an engine WAL command.
  await artifact!.append([new Uint8Array([0x52, 0x44, 0x42, 0x01])])
  await refreshFile()
  element('#storage-status').textContent = 'Marcador gravado no OPFS. Reabra o arquivo ou recarregue a página para conferir.'
}))
reopen.addEventListener('click', () => void action(async () => {
  await connect()
  element('#storage-status').textContent = 'Arquivo reaberto por um novo Worker. Contador lido dos bytes persistidos.'
}))

function networkState() {
  element('#network-state').textContent = navigator.onLine ? 'Rede disponível' : 'Navegador sem rede'
}
window.addEventListener('online', networkState)
window.addEventListener('offline', networkState)
networkState()

async function prepareOffline() {
  const status = element('#offline-state')
  try {
    if (import.meta.env.DEV) {
      status.textContent = 'O cache offline é ativado no build publicado. Use pnpm build para testar localmente.'
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
    status.textContent = 'Cache pronto. Você pode desligar a rede e recarregar esta página.'
    const reload = element<HTMLButtonElement>('#reload')
    reload.disabled = false
    reload.addEventListener('click', () => window.location.reload())
  } catch (failure) {
    status.textContent = `Cache offline indisponível: ${failure instanceof Error ? failure.message : String(failure)}`
  }
}

void action(async () => { await connect(); await query() })
void prepareOffline()

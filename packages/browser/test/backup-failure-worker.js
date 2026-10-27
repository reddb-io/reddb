// Test-only OPFS flush failure, injected before the native SQL publication.
import '../src/worker.js'

let fail = false
let failRead = false
const flush = FileSystemSyncAccessHandle.prototype.flush
const read = FileSystemSyncAccessHandle.prototype.read
FileSystemSyncAccessHandle.prototype.flush = function () {
  if (fail) { fail = false; throw new Error('Injected publication flush failure') }
  return flush.call(this)
}
FileSystemSyncAccessHandle.prototype.read = function (buffer, options) {
  if (failRead) { failRead = false; throw new Error('Injected backup read failure') }
  return read.call(this, buffer, options)
}
self.addEventListener('message', ({ data }) => {
  if (data.method === 'query' && data.params.sql.startsWith('UPDATE')) fail = true
  if (data.method === 'exportBackup') failRead = true
})

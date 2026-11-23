/** Export or verify the actual server schema using only a temporary instance. */
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { parseArgs } from 'node:util'

const { values } = parseArgs({
  options: {
    binary: { type: 'string', default: '../target/debug/shprotify' },
    output: { type: 'string' },
    check: { type: 'boolean', default: false },
  },
})
const output = values.output ?? resolve(
  dirname(fileURLToPath(import.meta.url)), '../public/openapi.json',
)
const temporary = await mkdtemp(resolve(tmpdir(), 'shprotify-openapi-'))
const environment = {
  PATH: process.env.PATH ?? '',
  HOME: temporary,
  TOKIO_WORKER_THREADS: '2',
  SHPROTIFY_HTTP__BIND: '127.0.0.1:0',
  SHPROTIFY_DATABASE__PATH: resolve(temporary, 'catalog.db'),
  SHPROTIFY_DOCS__ENABLED: 'true',
  SHPROTIFY_LOGGING__FORMAT: 'json',
  ...(process.env.SSL_CERT_FILE ? {
    SSL_CERT_FILE: process.env.SSL_CERT_FILE,
  } : {}),
}
const child = spawn(resolve(values.binary), [], {
  cwd: temporary,
  env: environment,
  stdio: ['ignore', 'ignore', 'pipe'],
})
const exited = new Promise((resolveExit) => child.once('close', resolveExit))
let logs = ''

/** Canonicalize objects while preserving semantically ordered arrays. */
function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical)
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value)
      .sort(([a], [b]) => a.localeCompare(b, 'en'))
      .map(([key, item]) => [key, canonical(item)]))
  }
  return value
}

/** Wait for the port actually assigned to this process, with a deadline. */
async function listening() {
  return new Promise((resolveAddress, reject) => {
    const timer = setTimeout(() => reject(new Error(
      `OpenAPI export startup timed out.\n${logs}`,
    )), 30_000)
    timer.unref()
    child.once('error', (error) => {
      clearTimeout(timer)
      reject(error)
    })
    child.once('exit', () => {
      clearTimeout(timer)
      reject(new Error(`OpenAPI export server exited.\n${logs}`))
    })
    child.stderr.setEncoding('utf8')
    child.stderr.on('data', (text) => {
      logs += text
      for (const line of logs.split('\n')) {
        let event
        try { event = JSON.parse(line) } catch { continue }
        if (event.fields?.message !== 'HTTP server listening') continue
        const address = event.fields.address
        if (!/^127\.0\.0\.1:[1-9][0-9]*$/.test(address)) {
          reject(new Error('Export server did not bind loopback'))
          return
        }
        clearTimeout(timer)
        resolveAddress(`http://${address}`)
        return
      }
    })
  })
}

try {
  const address = await listening()
  const response = await fetch(`${address}/openapi.json`, {
    signal: AbortSignal.timeout(10_000),
  })
  if (!response.ok) throw new Error(`OpenAPI export: HTTP ${response.status}`)
  const schema = canonical(await response.json())
  if (!schema.paths?.['/api/v1/health']) {
    throw new Error('Export did not contain the versioned API')
  }
  const text = `${JSON.stringify(schema, null, 2)}\n`
  if (values.check) {
    const previous = canonical(JSON.parse(await readFile(output, 'utf8')))
    if (JSON.stringify(previous) !== JSON.stringify(schema)) {
      throw new Error('OpenAPI snapshot is stale; run npm run api:export')
    }
    console.log('OpenAPI snapshot matches the running server')
  } else {
    await writeFile(output, text)
    console.log(`Exported ${Object.keys(schema.paths).length} API paths`)
  }
} finally {
  if (child.pid && child.exitCode === null && child.signalCode === null) {
    child.kill('SIGTERM')
    const timer = setTimeout(() => child.kill('SIGKILL'), 30_000)
    timer.unref()
    await exited
    clearTimeout(timer)
  }
  await rm(temporary, { recursive: true, force: true })
}

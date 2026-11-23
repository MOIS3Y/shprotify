/** Verify the public reference snapshots and the two reading paths offline. */
import assert from 'node:assert/strict'
import { readFile, readdir } from 'node:fs/promises'
import { dirname, resolve, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Parser } from '@dbml/core'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const schema = JSON.parse(await readFile(resolve(root, 'public/openapi.json'), 'utf8'))
const manifest = await readFile(resolve(root, '../Cargo.toml'), 'utf8')
const version = manifest.match(/^version\s*=\s*"([^"]+)"/m)?.[1]
assert.equal(schema.info.version, version, 'OpenAPI/package version differs')

const identities = new Set()
let operations = 0
for (const [path, methods] of Object.entries(schema.paths)) {
  assert(path.startsWith('/api/v1/'), `Unversioned API path: ${path}`)
  for (const operation of Object.values(methods)) {
    assert(operation.operationId, `Missing operationId: ${path}`)
    assert(!identities.has(operation.operationId), 'Duplicate operationId')
    identities.add(operation.operationId)
    operations++
  }
}

// The current parser supports checks and descending indexes; the legacy
// 'dbml' parser does not implement those parts of the DBML specification.
const database = Parser.parse(await readFile(resolve(root, 'database.dbml'), 'utf8'), 'dbmlv2')
assert(database.schemas.some((schema) => schema.tables.length > 0))

/** Return article paths, independent of traversal order or locale prefix. */
async function articles(directory) {
  const paths = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name)
    if (entry.isDirectory()) paths.push(...await articles(path))
    else if (entry.name.endsWith('.md')) {
      assert((await readFile(path, 'utf8')).trim().length > 200,
        `Empty or placeholder article: ${path}`)
      paths.push(path)
    }
  }
  return paths
}
for (const folder of ['guide', 'clients', 'reference']) {
  const en = resolve(root, folder)
  const ru = resolve(root, 'ru', folder)
  assert.deepEqual((await articles(en)).map((p) => relative(en, p)).sort(),
    (await articles(ru)).map((p) => relative(ru, p)).sort(),
    `Missing translation in ${folder}`)
}
console.log(`Reference valid: ${operations} operations, DBML, bilingual articles`)

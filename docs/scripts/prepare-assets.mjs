/** Publish the complete database schema alongside the generated diagrams. */
import { copyFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
await copyFile(resolve(root, 'database.dbml'), resolve(root, 'public/database.dbml'))

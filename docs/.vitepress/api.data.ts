import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

export interface ApiOperation {
  id: string
  method: string
  path: string
  summary: string
  permission: 'public' | 'authenticated' | 'admin'
}
export interface ApiGroup {
  name: string
  description: string
  operations: ApiOperation[]
}
export declare const data: ApiGroup[]

interface Operation {
  operationId: string
  summary?: string
  tags?: string[]
  security?: Record<string, string[]>[]
}
interface Schema {
  tags: { name: string; description?: string }[]
  paths: Record<string, Record<string, Operation>>
}
const file = fileURLToPath(new URL('../public/openapi.json', import.meta.url))

export default {
  watch: [file],
  load(): ApiGroup[] {
    const schema = JSON.parse(readFileSync(file, 'utf8')) as Schema
    return schema.tags.map((tag) => ({
      name: tag.name,
      description: tag.description ?? '',
      operations: Object.entries(schema.paths).flatMap(([path, operations]) =>
        Object.entries(operations)
          .filter(([, operation]) => operation.tags?.includes(tag.name))
          .map(([method, operation]) => ({
            id: operation.operationId,
            method: method.toUpperCase(), path,
            summary: operation.summary?.replace(/\s+/g, ' ') ?? '',
            permission: tag.name.startsWith('Admin:') ? 'admin' as const
              : operation.security?.length ? 'authenticated' as const
                : 'public' as const,
          }))),
    })).filter((group) => group.operations.length > 0)
  },
}

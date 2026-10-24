export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'HEAD' | 'OPTIONS'
export type BodyKind = 'none' | 'json' | 'text' | 'form' | 'multipart'
export type AuthKind = 'none' | 'basic' | 'bearer' | 'apiKey'

export interface KeyValue {
  key: string
  value: string
  enabled: boolean
}

export interface BodyField extends KeyValue {
  isFile: boolean
  filePath: string
  contentType: string
}

export interface AuthConfig {
  kind: AuthKind
  username: string
  password: string
  token: string
  key: string
  value: string
  location: 'header' | 'query'
}

export interface RequestBody {
  kind: BodyKind
  text: string
  fields: BodyField[]
}

export interface RequestTab {
  id: string
  method: HttpMethod
  url: string
  timeoutMs: number
  params: KeyValue[]
  headers: KeyValue[]
  auth: AuthConfig
  body: RequestBody
  recordHistory: boolean
  saveBodyContent: boolean
  needsSensitiveReview: boolean
  historyId: string | null
  requestState: 'idle' | 'running' | 'success' | 'http-error' | 'error' | 'canceled'
  executionId: string | null
  response: HttpResponse | null
  requestError: HttpError | null
  responseAt: string | null
}

export interface HttpResponse {
  status: number
  statusText: string
  durationMs: number
  responseSize: number
  truncated: boolean
  headers: ResponseHeader[]
  bodyBase64: string
}

export interface ResponseHeader {
  name: string
  value: string
}

export interface HttpError {
  kind: 'invalid_request' | 'timeout' | 'canceled' | 'network' | 'internal'
  message: string
}

export interface SendResult {
  tabId: string
  executionId: string
  response: HttpResponse | null
  error: HttpError | null
}

export const METHODS: HttpMethod[] = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS']

export function emptyBodyField(): BodyField {
  return { key: '', value: '', enabled: true, isFile: false, filePath: '', contentType: '' }
}

export function createTab(): RequestTab {
  return {
    id: crypto.randomUUID(), method: 'GET', url: '', timeoutMs: 30_000,
    params: [], headers: [],
    auth: { kind: 'none', username: '', password: '', token: '', key: '', value: '', location: 'header' },
    body: { kind: 'none', text: '', fields: [] },
    recordHistory: true, saveBodyContent: false, needsSensitiveReview: false, historyId: null,
    requestState: 'idle', executionId: null, response: null, requestError: null, responseAt: null,
  }
}

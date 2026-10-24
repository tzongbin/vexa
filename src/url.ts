export type UrlNormalization =
  | { ok: true; value: string }
  | { ok: false; message: string }

const SCHEME = /^([a-z][a-z\d+.-]*):/i
const HOST_AND_PORT = /^\d+(?:[/?#]|$)/

export function normalizeHttpUrl(input: string): UrlNormalization {
  const value = input.trim()
  if (!value) return { ok: false, message: 'URL 為必填欄位。' }

  const scheme = value.match(SCHEME)
  const hasHostPort = scheme && HOST_AND_PORT.test(value.slice(scheme[0].length))
  let candidate = value

  if (scheme && !hasHostPort) {
    const protocol = scheme[1]!.toLowerCase()
    if (protocol !== 'http' && protocol !== 'https') {
      return { ok: false, message: `不支援「${scheme[1]}」協定，請使用 HTTP 或 HTTPS。` }
    }
    if (!/^https?:\/\/[^/?#]+/i.test(value)) {
      return { ok: false, message: 'URL 格式錯誤，請使用完整的 HTTP 或 HTTPS 網址。' }
    }
  } else {
    let probe: URL
    try { probe = new URL(`https://${value}`) }
    catch { return { ok: false, message: 'URL 格式錯誤，請檢查網址、Port、Path 與 Query。' } }
    const host = probe.hostname.toLowerCase()
    const loopback = host === 'localhost' || host === '127.0.0.1' || host === '[::1]'
    candidate = `${loopback ? 'http' : 'https'}://${value}`
  }

  try {
    const parsed = new URL(candidate)
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      return { ok: false, message: '僅支援 HTTP 或 HTTPS 網址。' }
    }
    if (!parsed.hostname) return { ok: false, message: 'URL 必須包含主機名稱。' }
    return { ok: true, value: candidate }
  } catch {
    return { ok: false, message: 'URL 格式錯誤，請檢查網址、Port、Path 與 Query。' }
  }
}

export function formatTabTitle(method: string, input: string): string {
  const value = input.trim()
  if (!value) return `${method} 新請求`

  const normalized = normalizeHttpUrl(value)
  if (!normalized.ok) return `${method} 新請求`

  try {
    const parsed = new URL(normalized.value)
    const authority = normalized.value.match(/^[a-z][a-z\d+.-]*:\/\/([^/?#]*)/i)?.[1] ?? ''
    const hostPort = authority.slice(authority.lastIndexOf('@') + 1)
    const explicitPort = hostPort.startsWith('[')
      ? hostPort.match(/^\[[^\]]+\]:(\d+)$/)?.[1]
      : hostPort.match(/^[^:]+:(\d+)$/)?.[1]
    const path = parsed.pathname === '/' ? '' : parsed.pathname
    const host = `${parsed.hostname}${explicitPort ? `:${explicitPort}` : ''}`
    return `${method} ${host}${path}`
  } catch {
    return `${method} 新請求`
  }
}

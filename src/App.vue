<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { open, save } from '@tauri-apps/plugin-dialog'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import CodeEditor from './components/CodeEditor.vue'
import JsonNode from './components/JsonNode.vue'
import KeyValueEditor from './components/KeyValueEditor.vue'
import { createTab, emptyBodyField, METHODS, type BodyField, type HttpError, type RequestTab, type SendResult } from './types'
import { normalizeHttpUrl } from './url'

const tabs = ref<RequestTab[]>([createTab()])
const activeId = ref(tabs.value[0]!.id)
const tabsNav = ref<HTMLElement | null>(null)
const tabsOverflow = ref(false)
const tabsCanScrollLeft = ref(false)
const tabsCanScrollRight = ref(false)
const historyOpen = ref(false)
const historyEntries = ref<HistoryEntry[]>([])
const historySearch = ref('')
const historyMethod = ref('ALL')
const historyPageSize = ref(100)
const historyLoadWarning = ref('')
const sessionLoaded = ref(false)
let sessionPersistenceBlocked = false
let historyObserver: ResizeObserver | null = null
let tabAutoScrollPending = false
let sessionSaveTimer = 0
let sessionSaveQueue: Promise<void> = Promise.resolve()
let historySaveQueue: Promise<void> = Promise.resolve()
const split = ref(50)
const theme = ref<'system' | 'light' | 'dark'>('system')
type FontSize = 'small' | 'normal' | 'large' | 'xlarge'
type UiSettings = { version: 1; appearance: { theme: 'system' | 'light' | 'dark'; fontSize: FontSize; sourceFontSize: number | null }; historyLimit: number }
type SettingsLoadResult = { settings: UiSettings; migrated: boolean; warning: string | null }
type StoredRequest = Pick<RequestTab, 'method' | 'url' | 'timeoutMs' | 'params' | 'headers' | 'auth' | 'body'> & { saveBodyContent: boolean; needsSensitiveReview: boolean; bodyContentSaved: boolean }
type HistoryEntry = { id: string; executedAt: string; request: StoredRequest }
type SessionEntry = { id: string; historyId: string | null; recordHistory: boolean; request: StoredRequest }
type JsonLoad<T> = { value: T; warning: string | null }
const HISTORY_LIMITS = [100, 500, 1_000, 5_000, 10_000]
const FONT_SIZES: Array<{ value: FontSize; label: string; pixels: number }> = [
  { value: 'small', label: 'Small', pixels: 14 },
  { value: 'normal', label: 'Normal', pixels: 16 },
  { value: 'large', label: 'Large', pixels: 18 },
  { value: 'xlarge', label: 'XLarge', pixels: 20 },
]
const fontSize = ref<FontSize>('large')
const fontSettingsOpen = ref(false)
const fontSettingsRoot = ref<HTMLDivElement | null>(null)
const settingsLoaded = ref(false)
const historyLimit = ref(1_000)
const historyMenuOpen = ref(false)
const historySettingsRoot = ref<HTMLDivElement | null>(null)
const fontScale = computed(() => String(FONT_SIZES.find((size) => size.value === fontSize.value)!.pixels / 15))
const sourceFontSize = ref<number | null>(null)
const sourceFontPixels = computed(() => sourceFontSize.value ?? FONT_SIZES.find((size) => size.value === fontSize.value)!.pixels)
const requestSection = ref<'params' | 'headers' | 'auth' | 'body'>('params')
const responseSection = ref<'body' | 'headers'>('body')
const bodyView = ref<'formatted' | 'raw' | 'tree' | 'preview'>('formatted')
const search = ref('')
const notice = ref('')
let noticeTimer = 0

const activeTab = computed(() => tabs.value.find((tab) => tab.id === activeId.value) ?? tabs.value[0]!)
const filteredHistory = computed(() => {
  const needle = historySearch.value.trim().toLocaleLowerCase()
  return historyEntries.value.filter((entry) =>
    (historyMethod.value === 'ALL' || entry.request.method === historyMethod.value)
    && (!needle || entry.request.url.toLocaleLowerCase().includes(needle)),
  )
})
const visibleHistory = computed(() => filteredHistory.value.slice(0, historyPageSize.value))
const groupedHistory = computed(() => {
  const groups = new Map<string, HistoryEntry[]>()
  for (const entry of visibleHistory.value) {
    const date = new Date(entry.executedAt)
    const today = new Date()
    const yesterday = new Date(); yesterday.setDate(yesterday.getDate() - 1)
    const key = date.toDateString() === today.toDateString() ? '今天' : date.toDateString() === yesterday.toDateString() ? '昨天' : date.toLocaleDateString('zh-TW')
    const group = groups.get(key) ?? []
    group.push(entry)
    groups.set(key, group)
  }
  return [...groups.entries()]
})
const currentResponse = computed(() => activeTab.value.response)
const responseHeader = computed(() => currentResponse.value?.headers.find((header) => header.name.toLowerCase() === 'content-type')?.value ?? '')
const mimeType = computed(() => responseHeader.value.split(';', 1)[0]?.trim().toLowerCase() ?? '')
const responseCharset = computed(() => responseHeader.value.match(/charset\s*=\s*["']?([^;"']+)/i)?.[1]?.trim() ?? 'utf-8')
const responseKind = computed(() => {
  const type = mimeType.value
  if (type.includes('json') || type.endsWith('+json') || /^[\[{]/.test(responseText.value.trim())) return 'json'
  if (type.includes('html')) return 'html'
  if (type.includes('xml') || type.endsWith('+xml') || (type === '' && /^<\??[\w:!?]/.test(responseText.value.trim()))) return 'xml'
  if (type.includes('javascript') || type.includes('ecmascript')) return 'javascript'
  if (type.includes('css')) return 'css'
  if (type.startsWith('image/')) return 'image'
  if (type.startsWith('text/') || type.includes('x-www-form-urlencoded') || type === '') return 'text'
  return 'binary'
})
const responseIsText = computed(() => ['json', 'html', 'xml', 'javascript', 'css', 'text'].includes(responseKind.value))
const responseText = computed(() => {
  const response = currentResponse.value
  if (!response) return ''
  try {
    const binary = atob(response.bodyBase64)
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0))
    try { return new TextDecoder(responseCharset.value).decode(bytes) }
    catch { return new TextDecoder('utf-8').decode(bytes) }
  } catch {
    return ''
  }
})
const jsonValue = computed<unknown | null>(() => {
  if (responseKind.value !== 'json' || responseText.value.length > 1024 * 1024) return null
  try { return JSON.parse(responseText.value) as unknown } catch { return null }
})
const formattedText = computed(() => {
  if (bodyView.value === 'raw') return responseText.value
  if (responseKind.value === 'xml') return formatXml(responseText.value)
  if (responseKind.value !== 'json' || responseText.value.length > 1024 * 1024) return responseText.value
  if (jsonValue.value === null) return responseText.value
  try { return JSON.stringify(jsonValue.value, null, 2) } catch { return responseText.value }
})
const suggestedFileName = computed(() => {
  const disposition = currentResponse.value?.headers.find((header) => header.name.toLowerCase() === 'content-disposition')?.value
  const match = disposition?.match(/filename\*?=(?:UTF-8''|"?)([^";]+)/i)
  if (!match?.[1]) return 'response-body'
  let name = match[1].replaceAll('"', '')
  try { name = decodeURIComponent(name) } catch { /* Keep the server-provided literal filename. */ }
  return name.split(/[\\/]/).pop() || 'response-body'
})

function tabTitle(tab: RequestTab) {
  const path = tab.url.trim()
  try {
    const parsed = new URL(path)
    return `${tab.method} ${parsed.pathname || '/'}`
  } catch {
    return `${tab.method} ${path || '新請求'}`
  }
}
function addTab() {
  const tab = createTab()
  tabs.value.push(tab)
  tabAutoScrollPending = true
  window.setTimeout(() => { tabAutoScrollPending = false }, 300)
  activeId.value = tab.id
  requestSection.value = 'params'
  responseSection.value = 'body'
  void nextTick(() => scrollTabsToEnd())
}
function scrollTabsToEnd() {
  requestAnimationFrame(() => {
    const navigation = tabsNav.value
    if (navigation) {
      navigation.scrollLeft = Math.max(0, navigation.scrollWidth - navigation.clientWidth)
      const addButton = navigation.querySelector<HTMLElement>('.new-tab')
      if (addButton) {
        const navBounds = navigation.getBoundingClientRect()
        const addBounds = addButton.getBoundingClientRect()
        if (addBounds.right > navBounds.right) navigation.scrollLeft += addBounds.right - navBounds.right
      }
      updateTabScrollState()
    }
  })
}
function updateTabScrollState() {
  const navigation = tabsNav.value
  if (!navigation) return
  tabsOverflow.value = navigation.scrollWidth > navigation.clientWidth + 1
  tabsCanScrollLeft.value = navigation.scrollLeft > 1
  tabsCanScrollRight.value = navigation.scrollLeft + navigation.clientWidth < navigation.scrollWidth - 1
}
function scrollTabs(direction: -1 | 1) {
  const navigation = tabsNav.value
  if (!navigation) return
  navigation.scrollBy({ left: direction * Math.max(160, navigation.clientWidth * 0.65), behavior: 'smooth' })
}
function handleTabWheel(event: WheelEvent) {
  if (!event.shiftKey || !tabsNav.value) return
  event.preventDefault()
  tabsNav.value.scrollLeft += event.deltaY || event.deltaX
}
function activateTab(id: string) {
  activeId.value = id
  void nextTick(() => {
    const navigation = tabsNav.value
    const item = [...(navigation?.querySelectorAll<HTMLElement>('[data-tab-id]') ?? [])].find((element) => element.dataset.tabId === id)
    if (!navigation || !item) return
    const navRect = navigation.getBoundingClientRect()
    const itemRect = item.getBoundingClientRect()
    if (itemRect.left < navRect.left) navigation.scrollLeft -= navRect.left - itemRect.left
    else if (itemRect.right > navRect.right) navigation.scrollLeft += itemRect.right - navRect.right
    updateTabScrollState()
  })
}
function closeTab(id: string) {
  const index = tabs.value.findIndex((tab) => tab.id === id)
  if (index < 0) return
  const tab = tabs.value[index]!
  if (tab.executionId) void invoke<boolean>('cancel_request', { executionId: tab.executionId }).catch(() => undefined)
  tabs.value.splice(index, 1)
  if (!tabs.value.length) addTab()
  else if (activeId.value === id) activateTab(tabs.value[Math.min(index, tabs.value.length - 1)]!.id)
  void nextTick(() => { updateTabScrollState(); scheduleSessionSave() })
}
function message(text: string) {
  notice.value = text
  window.clearTimeout(noticeTimer)
  noticeTimer = window.setTimeout(() => { notice.value = '' }, 3000)
}
function isBusy(tab: RequestTab) { return tab.requestState === 'running' }

function snapshotRequest(tab: RequestTab) {
  return {
    method: tab.method, url: tab.url, timeoutMs: tab.timeoutMs,
    params: tab.params, headers: tab.headers, auth: tab.auth, body: tab.body,
    saveBodyContent: true, needsSensitiveReview: tab.needsSensitiveReview,
  }
}
function cloneData<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

async function loadHistory() {
  try {
    const result = await invoke<JsonLoad<{ version: number; entries: HistoryEntry[] }>>('load_history')
    historyEntries.value = result.value.entries
    historyLoadWarning.value = result.warning ?? ''
    if (result.warning) message(result.warning)
  } catch (error) {
    message(`無法載入 Data/history.json：${String(error)}`)
  }
}

function scheduleSessionSave() {
  if (!sessionLoaded.value || sessionPersistenceBlocked) return
  window.clearTimeout(sessionSaveTimer)
  sessionSaveTimer = window.setTimeout(() => {
    queueSessionSave(true)
  }, 600)
}

function queueSessionSave(showError: boolean) {
  const sessionTabs: SessionEntry[] = tabs.value.map((tab) => ({
    id: tab.id, historyId: tab.historyId, recordHistory: tab.recordHistory,
    request: snapshotRequest(tab) as StoredRequest,
  }))
  const selected = activeId.value
  sessionSaveQueue = sessionSaveQueue.catch(() => undefined)
    .then(() => invoke<void>('save_session', { tabs: sessionTabs, activeTabId: selected }))
    .catch((error: unknown) => { if (showError) message(`無法保存 Data/session.json：${String(error)}`) })
}

function flushSessionSave() {
  if (!sessionLoaded.value || sessionPersistenceBlocked) return
  window.clearTimeout(sessionSaveTimer)
  queueSessionSave(false)
}

async function loadSession() {
  try {
    const result = await invoke<JsonLoad<{ version: number; tabs: SessionEntry[]; activeTabId: string }>>('load_session')
    if (result.warning) {
      sessionPersistenceBlocked = true
      message(result.warning)
    }
    if (result.value.tabs.length) {
      tabs.value = result.value.tabs.map((saved) => ({
        ...createTab(), id: saved.id, method: saved.request.method, url: saved.request.url,
        timeoutMs: saved.request.timeoutMs, params: saved.request.params, headers: saved.request.headers,
        auth: saved.request.auth, body: saved.request.body,
        recordHistory: saved.recordHistory, saveBodyContent: saved.request.saveBodyContent,
        needsSensitiveReview: saved.request.needsSensitiveReview, historyId: saved.historyId,
        requestState: 'idle', executionId: null, response: null, requestError: null, responseAt: null,
      }))
      activeId.value = result.value.tabs.some((tab) => tab.id === result.value.activeTabId)
        ? result.value.activeTabId : tabs.value[0]!.id
      activateTab(activeId.value)
    }
  } catch (error) {
    message(`無法載入 Data/session.json；已使用空白 Tab：${String(error)}`)
  } finally {
    sessionLoaded.value = true
  }
}

function openHistoryEntry(entry: HistoryEntry) {
  const alreadyOpen = tabs.value.find((tab) => tab.historyId === entry.id)
  if (alreadyOpen) {
    activateTab(alreadyOpen.id)
    return
  }
  const tab = createTab()
  tab.historyId = entry.id
  tab.method = entry.request.method
  tab.url = entry.request.url
  tab.timeoutMs = entry.request.timeoutMs
  tab.params = cloneData(entry.request.params)
  tab.headers = cloneData(entry.request.headers)
  tab.auth = cloneData(entry.request.auth)
  tab.body = cloneData(entry.request.body)
  tab.saveBodyContent = entry.request.saveBodyContent
  tab.needsSensitiveReview = entry.request.needsSensitiveReview
  tab.response = null
  tabs.value.push(tab)
  requestSection.value = 'params'
  responseSection.value = 'body'
  activateTab(tab.id)
}

async function clearHistory(scope: 'all' | 'today' | 'older7' | 'older30') {
  const labels = { all: '全部 History', today: '今天的 History', older7: '7 天以前的 History', older30: '30 天以前的 History' }
  if (!window.confirm(`確定清除${labels[scope]}？目前開啟的 Tabs 不會受影響。`)) return
  try {
    const now = new Date()
    const todayDate = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`
    const todayStartUtc = new Date(now.getFullYear(), now.getMonth(), now.getDate()).toISOString()
    const tomorrowStartUtc = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1).toISOString()
    await invoke('clear_history', { scope, todayDate, todayStartUtc, tomorrowStartUtc })
    await loadHistory()
    historyPageSize.value = 100
    message(`${labels[scope]}已清除。`)
  } catch (error) { message(`清除 History 失敗：${String(error)}`) }
}

function formatHistoryTime(value: string) {
  const date = new Date(value)
  return date.toLocaleTimeString('zh-TW', { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false })
}

function handleUrlEnter(event: KeyboardEvent) {
  if (event.isComposing || event.keyCode === 229 || event.ctrlKey || event.metaKey) return
  event.preventDefault()
  void sendRequest()
}

async function sendRequest() {
  const tab = activeTab.value
  if (isBusy(tab)) return
  try {
    const missingFiles = await invoke<string[]>('validate_multipart_files', { body: tab.body })
    if (missingFiles.length) {
      window.alert(`Multipart 原始檔案不存在或超過 50 MiB，請重新選取或移除欄位後再送出：\n\n${missingFiles.join('\n')}`)
      return
    }
  } catch (error) {
    message(`無法檢查 Multipart 檔案；Request 未送出：${String(error)}`)
    return
  }
  if (tab.needsSensitiveReview) {
    if (!window.confirm('此 Request 曾因安全規則省略或遮罩機密欄位，認證或 Body 可能需要重新輸入。請先檢查 Request。仍要直接送出目前內容嗎？')) return
    tab.needsSensitiveReview = false
    scheduleSessionSave()
  }
  const normalizedUrl = normalizeHttpUrl(tab.url)
  if (!normalizedUrl.ok) {
    tab.requestState = 'error'
    tab.requestError = { kind: 'invalid_request', message: normalizedUrl.message }
    return
  }
  tab.url = normalizedUrl.value
  const requestUrl = normalizedUrl.value
  const executionId = crypto.randomUUID()
  tab.executionId = executionId
  tab.requestState = 'running'
  tab.requestError = null
  if (tab.recordHistory) {
    const historySnapshot = cloneData(snapshotRequest(tab))
    historySaveQueue = historySaveQueue.catch(() => undefined)
      .then(() => invoke('record_history', { input: historySnapshot }))
      .then(() => loadHistory())
      .catch((error: unknown) => message(`Request 已送出，但 History 無法保存：${String(error)}`))
  }
  scheduleSessionSave()
  const request = {
    tabId: tab.id, executionId, method: tab.method, url: requestUrl, timeoutMs: tab.timeoutMs,
    params: tab.params, headers: tab.headers, auth: tab.auth, body: tab.body,
  }
  try {
    const result = await invoke<SendResult>('send_request', { request })
    const owner = tabs.value.find((candidate) => candidate.id === result.tabId)
    if (!owner || owner.executionId !== result.executionId) return
    owner.executionId = null
    if (result.response) {
      owner.response = result.response
      owner.responseAt = new Date().toLocaleTimeString('zh-TW', { hour12: false })
      owner.requestError = null
      owner.requestState = result.response.status >= 400 ? 'http-error' : 'success'
    } else {
      const error: HttpError = result.error ?? { kind: 'internal', message: 'HTTP Engine 未回傳結果。' }
      owner.requestError = error
      owner.requestState = error.kind === 'canceled' ? 'canceled' : 'error'
    }
  } catch (error) {
    const owner = tabs.value.find((candidate) => candidate.id === tab.id)
    if (!owner || owner.executionId !== executionId) return
    owner.executionId = null
    owner.requestState = 'error'
    owner.requestError = { kind: 'internal', message: String(error) }
  }
}
async function cancelActive() {
  const tab = activeTab.value
  if (!tab.executionId) return
  try { await invoke<boolean>('cancel_request', { executionId: tab.executionId }) }
  catch (error) { message(`取消失敗：${String(error)}`) }
}
function addMultipartField() { activeTab.value.body.fields.push(emptyBodyField()) }
function patchMultipart(index: number, key: keyof BodyField, value: string | boolean) {
  const field = activeTab.value.body.fields[index]
  if (field) field[key] = value as never
}
function removeMultipartField(index: number) { activeTab.value.body.fields.splice(index, 1) }
async function chooseFile(index: number) {
  try {
    const path = await open({ multiple: false, directory: false, title: '選擇要上傳的檔案' })
    if (typeof path === 'string') patchMultipart(index, 'filePath', path)
  } catch (error) { message(`無法開啟檔案選擇器：${String(error)}`) }
}
async function saveBody() {
  const response = currentResponse.value
  if (!response) return
  try {
    const path = await save({ defaultPath: suggestedFileName.value, title: '另存 Response Body' })
    if (typeof path !== 'string') return
    await invoke('save_response', { path, bodyBase64: response.bodyBase64 })
    message('Response Body 已儲存。')
  } catch (error) { message(`儲存失敗：${String(error)}`) }
}
async function copyBody() {
  try { await writeText(responseText.value); message('Response Body 已複製。') }
  catch (error) { message(`複製失敗：${String(error)}`) }
}
function prettySize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`
}
function formatXml(source: string) {
  if (source.length > 1024 * 1024) return source
  const parsed = new DOMParser().parseFromString(source, 'application/xml')
  if (parsed.querySelector('parsererror')) return source
  const serialized = new XMLSerializer().serializeToString(parsed)
  const tokens = serialized.match(/<!--[^]*?-->|<\?[^]*?\?>|<![^>]*>|<[^>]+>|[^<]+/g) ?? []
  let depth = 0
  return tokens.map((token) => {
    if (!token.trim()) return ''
    if (/^<\//.test(token)) depth = Math.max(0, depth - 1)
    const line = `${'  '.repeat(depth)}${token.trim()}`
    if (/^<[^!?/][^>]*[^/]\s*>$/.test(token) && !/\/>$/.test(token)) depth += 1
    return line
  }).filter(Boolean).join('\n')
}
function statusClass(tab: RequestTab) {
  if (tab.requestState === 'running') return 'state-running'
  if (tab.requestState === 'success') return tab.response && tab.response.status >= 300 && tab.response.status < 400 ? 'state-redirect' : 'state-success'
  if (tab.requestState === 'http-error') return tab.response && tab.response.status < 500 ? 'state-warning' : 'state-error'
  if (tab.requestState === 'error') return 'state-error'
  if (tab.requestState === 'canceled') return 'state-canceled'
  return 'state-idle'
}
function responseStatusClass(status: number) {
  if (status >= 200 && status < 300) return 'status-ok'
  if (status >= 300 && status < 400) return 'status-redirect'
  if (status >= 400 && status < 500) return 'status-client-error'
  if (status >= 500) return 'status-server-error'
  return 'status-neutral'
}
function setFontSize(value: FontSize) {
  fontSize.value = value
  fontSettingsOpen.value = false
}
function handleGlobalWheel(event: WheelEvent) {
  if (!event.ctrlKey) return
  event.preventDefault()
  const target = event.target
  if (!(target instanceof Element) || !target.closest('.response-code')) return
  if (event.deltaY === 0) return
  const current = sourceFontPixels.value
  const next = Math.min(32, Math.max(12, current + (event.deltaY < 0 ? 1 : -1)))
  sourceFontSize.value = next
}
function dragSplit(event: PointerEvent) {
  const grid = event.currentTarget as HTMLElement
  const bounds = grid.parentElement?.getBoundingClientRect()
  if (!bounds) return
  const move = (pointer: PointerEvent) => {
    const ratio = ((pointer.clientX - bounds.left) / bounds.width) * 100
    split.value = Math.min(68, Math.max(32, ratio))
  }
  const stop = () => {
    window.removeEventListener('pointermove', move)
    window.removeEventListener('pointerup', stop)
    grid.releasePointerCapture(event.pointerId)
  }
  grid.setPointerCapture(event.pointerId)
  window.addEventListener('pointermove', move)
  window.addEventListener('pointerup', stop, { once: true })
}
function cycleTheme() { theme.value = theme.value === 'system' ? 'light' : theme.value === 'light' ? 'dark' : 'system' }
function closeSettingsOnOutsidePointer(event: PointerEvent) {
  const target = event.target
  if (target instanceof Node && !fontSettingsRoot.value?.contains(target)) fontSettingsOpen.value = false
  if (target instanceof Node && !historySettingsRoot.value?.contains(target)) historyMenuOpen.value = false
}
function handleShortcut(event: KeyboardEvent) {
  if (event.isComposing || event.keyCode === 229) return
  if (!(event.ctrlKey || event.metaKey) || event.altKey) {
    if (event.key === 'Escape') {
      if (fontSettingsOpen.value) {
        fontSettingsOpen.value = false
        event.preventDefault()
        return
      }
      void cancelActive()
    }
    return
  }
  const key = event.key.toLowerCase()
  if (key === 't') { event.preventDefault(); addTab() }
  else if (key === 'w') { event.preventDefault(); closeTab(activeId.value) }
  else if (event.key === 'Enter') { event.preventDefault(); void sendRequest() }
}

let settingsSaveQueue: Promise<void> = Promise.resolve()
let settingsSaveFailureShown = false
let lastAppliedHistoryLimit = 1_000
watch([fontSize, sourceFontSize, theme, historyLimit], () => {
  if (!settingsLoaded.value) return
  const settings: UiSettings = { version: 1, appearance: { theme: theme.value, fontSize: fontSize.value, sourceFontSize: sourceFontSize.value }, historyLimit: historyLimit.value }
  const refreshHistory = settings.historyLimit !== lastAppliedHistoryLimit
  settingsSaveQueue = settingsSaveQueue.catch(() => undefined).then(() => invoke('save_ui_settings', { settings })).then(() => {
    settingsSaveFailureShown = false
    lastAppliedHistoryLimit = settings.historyLimit
    if (refreshHistory) void loadHistory()
  }).catch((error: unknown) => {
    if (!settingsSaveFailureShown) message(`設定已套用，但無法保存至 Data/settings.json 或套用 History 上限：${String(error)}`)
    settingsSaveFailureShown = true
  })
})

async function loadUiSettings() {
  let legacy: { theme?: string; fontSize?: string; sourceFontSize?: number } = {}
  try {
    const oldTheme = localStorage.getItem('vexa.ui.theme')
    const oldFontSize = localStorage.getItem('vexa.ui.fontSize')
    const oldSourceSize = Number(localStorage.getItem('vexa.response.sourceFontSize'))
    if (oldTheme) legacy.theme = oldTheme
    if (oldFontSize) legacy.fontSize = oldFontSize
    if (Number.isInteger(oldSourceSize) && oldSourceSize >= 12 && oldSourceSize <= 32) legacy.sourceFontSize = oldSourceSize
  } catch { /* The Rust JSON settings remain the only persisted source after migration. */ }
  try {
    const result = await invoke<SettingsLoadResult>('load_ui_settings', { legacy })
    theme.value = result.settings.appearance.theme
    fontSize.value = result.settings.appearance.fontSize
    sourceFontSize.value = result.settings.appearance.sourceFontSize
    historyLimit.value = result.settings.historyLimit ?? 1_000
    lastAppliedHistoryLimit = historyLimit.value
    if (result.warning) message(result.warning)
    else {
      try {
        localStorage.removeItem('vexa.ui.theme')
        localStorage.removeItem('vexa.ui.fontSize')
        localStorage.removeItem('vexa.response.sourceFontSize')
      } catch { /* Legacy browser values are ignored after a valid JSON load. */ }
    }
  } catch (error) {
    message(`無法載入 Data/settings.json；目前使用預設設定：${String(error)}`)
  } finally {
    settingsLoaded.value = true
  }
}
function syntaxMarkup(text: string, language: string, needle: string) {
  const patterns: Record<string, RegExp> = {
    json: /("(?:\\.|[^"\\])*")(?=\s*:)|("(?:\\.|[^"\\])*")|\b(true|false|null)\b|-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g,
    javascript: /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`)|\b(?:const|let|var|function|return|if|else|new|async|await|class|import|export|from|throw|try|catch|true|false|null|undefined)\b|-?\b\d+(?:\.\d+)?/g,
    css: /(\/\*[\s\S]*?\*\/)|("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')|([.#]?[\w-]+)(?=\s*\{)|(-?\b\d+(?:\.\d+)?(?:px|em|rem|%|vh|vw)?)/g,
    xml: /(<\/?[\w:-]+|\/?>)|([\w:-]+)(?=\s*=)|("[^"]*"|'[^']*')/g,
  }
  const pattern = patterns[language]
  if (!pattern) return markEscaped(text, needle)
  const classes: Record<string, string> = language === 'json'
    ? { 1: 'json-key', 2: 'json-string', 3: 'json-literal', 4: 'json-number' }
    : { 1: language === 'xml' ? 'syntax-tag' : 'syntax-comment', 2: language === 'xml' ? 'syntax-attribute' : 'syntax-string', 3: language === 'xml' ? 'syntax-string' : 'syntax-keyword', 4: 'json-number' }
  let html = ''
  let cursor = 0
  for (const match of text.matchAll(pattern)) {
    const index = match.index ?? 0
    html += markEscaped(text.slice(cursor, index), needle)
    const token = match[0]
    const group = match.findIndex((part, i) => i > 0 && part !== undefined)
    html += `<span class="${classes[group] ?? 'syntax-token'}">${markEscaped(token, needle)}</span>`
    cursor = index + token.length
  }
  return html + markEscaped(text.slice(cursor), needle)
}
function markEscaped(text: string, needle: string) {
  const escaped = text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&#39;')
  if (!needle) return escaped
  const safeNeedle = needle.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  return escaped.replace(new RegExp(safeNeedle, 'gi'), (value) => `<mark>${value}</mark>`)
}

onMounted(() => {
  window.addEventListener('keydown', handleShortcut)
  window.addEventListener('wheel', handleGlobalWheel, { capture: true, passive: false })
  window.addEventListener('pointerdown', closeSettingsOnOutsidePointer, true)
  watch(activeId, () => {
    fontSettingsOpen.value = false
    historyMenuOpen.value = false
    void nextTick(() => {
      const navigation = tabsNav.value
      const item = [...(navigation?.querySelectorAll<HTMLElement>('[data-tab-id]') ?? [])].find((element) => element.dataset.tabId === activeId.value)
      if (!navigation || !item) return
      const navRect = navigation.getBoundingClientRect(); const itemRect = item.getBoundingClientRect()
      if (itemRect.left < navRect.left) navigation.scrollLeft -= navRect.left - itemRect.left
      else if (itemRect.right > navRect.right) navigation.scrollLeft += itemRect.right - navRect.right
      updateTabScrollState()
    })
  })
  watch(() => tabs.value.map((tab) => ({
    id: tab.id, historyId: tab.historyId, recordHistory: tab.recordHistory,
    method: tab.method, url: tab.url, timeoutMs: tab.timeoutMs,
    params: tab.params, headers: tab.headers, auth: tab.auth, body: tab.body,
    saveBodyContent: tab.saveBodyContent,
  })), scheduleSessionSave, { deep: true })
  watch(activeId, scheduleSessionSave)
  if (tabsNav.value) {
    historyObserver = new ResizeObserver(() => {
      updateTabScrollState()
      if (tabAutoScrollPending) scrollTabsToEnd()
    })
    historyObserver.observe(tabsNav.value)
    if (tabsNav.value.parentElement) historyObserver.observe(tabsNav.value.parentElement)
  }
  window.addEventListener('resize', updateTabScrollState)
  window.addEventListener('beforeunload', flushSessionSave)
  void nextTick(updateTabScrollState)
  void loadUiSettings()
  void loadHistory()
  void loadSession()
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleShortcut)
  window.removeEventListener('wheel', handleGlobalWheel, true)
  window.removeEventListener('pointerdown', closeSettingsOnOutsidePointer, true)
  window.removeEventListener('resize', updateTabScrollState)
  window.removeEventListener('beforeunload', flushSessionSave)
  flushSessionSave()
  historyObserver?.disconnect()
  window.clearTimeout(noticeTimer)
  window.clearTimeout(sessionSaveTimer)
})
</script>

<template>
  <main class="app-shell" :data-theme="theme" :data-font-size="fontSize" :style="{ '--ui-scale': fontScale }">
    <header class="tabbar">
      <div class="brand" aria-label="Vexa"><img class="brand-mark" src="/vexa-logo.svg" alt="" /></div>
      <div class="tab-strip">
        <button v-if="tabsOverflow" class="tab-scroll-button" aria-label="向左捲動 Tabs" :disabled="!tabsCanScrollLeft" @click="scrollTabs(-1)">‹</button>
        <nav ref="tabsNav" class="tabs" aria-label="Request Tabs" @scroll="updateTabScrollState" @wheel="handleTabWheel">
          <button v-for="tab in tabs" :key="tab.id" :data-tab-id="tab.id" class="tab" :class="{ 'tab-active': activeId === tab.id }" @click="activateTab(tab.id)">
            <span class="tab-state" :class="statusClass(tab)"></span><span class="tab-title">{{ tabTitle(tab) }}</span>
            <span v-if="tab.requestState === 'running'" class="tab-spinner"></span>
            <span class="tab-close" role="button" aria-label="關閉 Tab" @click.stop="closeTab(tab.id)">×</span>
          </button>
          <button class="icon-button new-tab" aria-label="新增 Tab" title="新增 Tab (Ctrl+T)" @click="addTab">＋</button>
        </nav>
        <button v-if="tabsOverflow" class="tab-scroll-button" aria-label="向右捲動 Tabs" :disabled="!tabsCanScrollRight" @click="scrollTabs(1)">›</button>
      </div>
      <div class="top-controls">
        <span class="local-indicator"><span></span>本機</span>
        <button class="history-toggle" :aria-expanded="historyOpen" aria-label="History" title="History" @click="historyOpen = !historyOpen">◷ <span>History</span></button>
        <button class="icon-button theme-toggle" :title="`主題：${theme === 'system' ? '跟隨系統' : theme === 'dark' ? '深色' : '淺色'}`" @click="cycleTheme">◐</button>
        <div ref="fontSettingsRoot" class="font-settings">
          <button class="icon-button settings-toggle" aria-label="字體大小設定" title="字體大小與 History 設定" :aria-expanded="fontSettingsOpen" @click="fontSettingsOpen = !fontSettingsOpen">⚙</button>
        <div v-if="fontSettingsOpen" class="font-menu" role="group" aria-label="字體大小">
          <span class="font-menu-title">字體大小</span>
          <button v-for="size in FONT_SIZES" :key="size.value" type="button" :aria-pressed="fontSize === size.value" :class="{ selected: fontSize === size.value }" @click="setFontSize(size.value)">{{ size.label }}<span>{{ size.pixels }}px</span></button>
          <label class="history-limit-setting">History 上限<select v-model.number="historyLimit" aria-label="History 筆數上限"><option v-for="limit in HISTORY_LIMITS" :key="limit" :value="limit">{{ limit.toLocaleString() }} 筆</option></select></label>
        </div>
      </div>
      </div>
    </header>

    <div class="request-toolbar">
      <select v-model="activeTab.method" class="method-select" aria-label="HTTP Method">
        <option v-for="method in METHODS" :key="method" :value="method">{{ method }}</option>
      </select>
      <input v-model="activeTab.url" class="url-input" type="text" inputmode="url" placeholder="輸入 URL，例如 api.example.com/v1/items" aria-label="Request URL" @keydown.enter="handleUrlEnter" />
      <button v-if="isBusy(activeTab)" class="send-button cancel-button" @click="cancelActive"><span>■</span> 取消</button>
      <button v-else class="send-button" @click="sendRequest"><span>➤</span> Send</button>
    </div>

    <div class="content-row">
      <aside v-if="historyOpen" class="history-panel" aria-label="History 面板">
        <div class="history-heading"><h2>History</h2><button class="icon-button" aria-label="收合 History" title="收合 History" @click="historyOpen = false">×</button></div>
        <label class="history-search">⌕ <input v-model="historySearch" aria-label="搜尋 History URL" placeholder="搜尋 URL" @input="historyPageSize = 100" /></label>
        <select v-model="historyMethod" class="history-method" aria-label="History Method 篩選"><option value="ALL">所有 Method</option><option v-for="method in METHODS" :key="method" :value="method">{{ method }}</option></select>
        <p v-if="historyLoadWarning" class="history-warning">{{ historyLoadWarning }}</p>
        <div class="history-list">
          <template v-for="[date, entries] in groupedHistory" :key="date">
            <h3 class="history-date">{{ date }}</h3>
            <button v-for="entry in entries" :key="entry.id" class="history-entry" :title="entry.request.url" @click="openHistoryEntry(entry)">
              <span class="history-entry-meta"><strong :class="`method-${entry.request.method.toLowerCase()}`">{{ entry.request.method }}</strong><time>{{ formatHistoryTime(entry.executedAt) }}</time></span>
              <span class="history-entry-url">{{ entry.request.url }}</span>
              <span v-if="entry.request.needsSensitiveReview" class="history-redacted">部分機密欄位需重新輸入</span>
            </button>
          </template>
          <div v-if="!visibleHistory.length" class="history-empty">{{ historyEntries.length ? '找不到符合的 History。' : '尚無 History。' }}</div>
          <button v-if="visibleHistory.length < filteredHistory.length" class="history-more" @click="historyPageSize += 100">載入更多</button>
        </div>
        <div class="history-actions"><button @click="clearHistory('today')">清除今天</button><button @click="clearHistory('older7')">清除 7 天以前</button><button @click="clearHistory('older30')">清除 30 天以前</button><button class="history-clear-all" @click="clearHistory('all')">清除全部 History</button></div>
      </aside>
      <section class="workspace" :class="{ 'workspace-with-history': historyOpen }" :style="{ gridTemplateColumns: `minmax(0, ${split}fr) 8px minmax(0, ${100 - split}fr)` }">
      <section class="pane request-pane" aria-label="Request 編輯器">
        <div class="pane-heading"><div><span class="pane-dot request-dot"></span><h1>Request</h1></div><div class="request-pane-options"><label class="record-history-option"><input v-model="activeTab.recordHistory" type="checkbox" />History</label><label class="timeout-label"><span>Timeout</span><input v-model.number="activeTab.timeoutMs" type="number" min="100" max="600000" step="1000" aria-label="Timeout 毫秒" /><span class="timeout-unit">ms</span></label></div></div>
        <div class="section-tabs">
          <button v-for="item in ([['params', 'Params', activeTab.params.filter(row => row.enabled && row.key).length], ['headers', 'Headers', activeTab.headers.filter(row => row.enabled && row.key).length], ['auth', 'Auth', 0], ['body', 'Body', 0]] as const)" :key="item[0]" :class="{ selected: requestSection === item[0] }" @click="requestSection = item[0]">{{ item[1] }}<span v-if="item[2]" class="tab-count">{{ item[2] }}</span></button>
        </div>
        <div v-if="activeTab.needsSensitiveReview" class="sensitive-review-note" role="status">此 Request 的機密欄位已從 History / Session 省略或遮罩。請檢查 Auth、Headers、Query 與 Body；送出前會再次確認。</div>
        <div class="request-scroll" :key="activeTab.id">
          <template v-if="requestSection === 'params'">
            <div class="section-copy"><h2>Query Parameters</h2><p>附加到 URL 的查詢參數，可保留重複的 Key。</p></div>
            <KeyValueEditor v-model="activeTab.params" key-label="Key" value-label="Value" empty-text="尚無 Query Parameters" />
          </template>
          <template v-else-if="requestSection === 'headers'">
            <div class="section-copy"><h2>Request Headers</h2><p>自訂 Header 會依序送出，支援重複名稱。</p></div>
            <KeyValueEditor v-model="activeTab.headers" key-label="Header" value-label="Value" empty-text="尚無自訂 Headers" />
          </template>
          <template v-else-if="requestSection === 'auth'">
            <div class="section-copy"><h2>Authorization</h2><p>憑證不會寫入 History 或 Session；還原後請重新輸入。</p></div>
            <label class="field-label">類型<select v-model="activeTab.auth.kind" class="control-select"><option value="none">None</option><option value="basic">Basic Auth</option><option value="bearer">Bearer Token</option><option value="apiKey">API Key</option></select></label>
            <div v-if="activeTab.auth.kind === 'basic'" class="form-stack"><label class="field-label">Username<input v-model="activeTab.auth.username" autocomplete="off" /></label><label class="field-label">Password<input v-model="activeTab.auth.password" type="password" autocomplete="new-password" /></label></div>
            <label v-else-if="activeTab.auth.kind === 'bearer'" class="field-label">Token<input v-model="activeTab.auth.token" type="password" autocomplete="new-password" placeholder="Bearer token" /></label>
            <div v-else-if="activeTab.auth.kind === 'apiKey'" class="form-stack"><label class="field-label">Key<input v-model="activeTab.auth.key" placeholder="X-API-Key" /></label><label class="field-label">Value<input v-model="activeTab.auth.value" type="password" autocomplete="new-password" /></label><label class="field-label">加入位置<select v-model="activeTab.auth.location" class="control-select"><option value="header">Header</option><option value="query">Query Parameter</option></select></label></div>
            <div v-else class="auth-empty"><span class="auth-lock">⌑</span><p>此 Request 不會加入 Authorization 憑證。</p></div>
          </template>
          <template v-else>
            <div class="section-copy body-section-copy"><div><h2>Request Body</h2><p v-if="activeTab.body.kind === 'multipart'">Multipart 會自動產生必要的 boundary，Content-Type 由 HTTP Engine 設定。</p><p v-else>依照內容類型自動設定 Content-Type，可用自訂 Header 覆寫。</p></div>
              <select v-model="activeTab.body.kind" class="control-select body-type"><option value="none">None</option><option value="json">JSON</option><option value="text">Text</option><option value="form">Form URL Encoded</option><option value="multipart">Multipart Form Data</option></select>
            </div>
            <p class="body-save-warning">History 與 Session 會使用 AES-256-GCM 加密保存完整 Request，每筆新保存內容上限 1 MiB。Multipart 檔案內容不會複製，僅保存原始檔案路徑；搬移 Data 目錄後請確認檔案仍存在。</p>
            <CodeEditor v-if="activeTab.body.kind === 'json' || activeTab.body.kind === 'text'" v-model="activeTab.body.text" :language="activeTab.body.kind === 'json' ? 'json' : 'text'" :placeholder="activeTab.body.kind === 'json' ? '{\n  \n}' : '輸入 Request Body…'" />
            <KeyValueEditor v-else-if="activeTab.body.kind === 'form'" v-model="activeTab.body.fields" key-label="Key" value-label="Value" empty-text="尚無表單欄位" />
            <div v-else-if="activeTab.body.kind === 'multipart'" class="multipart-list">
              <div v-for="(field, index) in activeTab.body.fields" :key="index" class="multipart-row">
                <input type="checkbox" :checked="field.enabled" aria-label="啟用欄位" @change="patchMultipart(index, 'enabled', ($event.target as HTMLInputElement).checked)" />
                <input :value="field.key" placeholder="Field name" aria-label="欄位名稱" @input="patchMultipart(index, 'key', ($event.target as HTMLInputElement).value)" />
                <template v-if="field.isFile"><button class="file-select" @click="chooseFile(index)">{{ field.filePath.split(/[\\/]/).pop() || '選擇檔案…' }}</button><input :value="field.contentType" placeholder="Content-Type (選填)" aria-label="檔案 Content-Type" @input="patchMultipart(index, 'contentType', ($event.target as HTMLInputElement).value)" /></template>
                <input v-else :value="field.value" placeholder="Value" aria-label="欄位值" @input="patchMultipart(index, 'value', ($event.target as HTMLInputElement).value)" />
                <button class="type-toggle" :title="field.isFile ? '切換文字欄位' : '切換檔案欄位'" @click="patchMultipart(index, 'isFile', !field.isFile)">{{ field.isFile ? '檔案' : '文字' }}</button>
                <button class="icon-button remove-button" aria-label="刪除欄位" @click="removeMultipartField(index)">×</button>
              </div>
              <p v-if="!activeTab.body.fields.length" class="empty-inline">尚無 Multipart 欄位</p>
              <button class="text-button add-row" @click="addMultipartField">＋ 新增欄位</button>
              <p class="helper-note">單一上傳檔案上限 50 MB。檔案僅會在傳送 Request 時讀取。</p>
            </div>
            <div v-else class="auth-empty"><span class="body-empty-icon">{ }</span><p>此 Request 不會傳送 Body。</p></div>
          </template>
        </div>
      </section>

      <div class="splitter" role="separator" aria-label="調整 Request 與 Response 寬度" aria-orientation="vertical" @pointerdown="dragSplit" @keydown.left.prevent="split = Math.max(32, split - 2)" @keydown.right.prevent="split = Math.min(68, split + 2)" tabindex="0"><span></span></div>

      <section class="pane response-pane" aria-label="Response 檢視器">
        <div class="pane-heading"><div><span class="pane-dot response-dot"></span><h1>Response</h1></div><div class="response-toolbar">
          <template v-if="currentResponse"><button v-if="responseIsText" class="icon-button action-icon" title="複製 Response Body" aria-label="複製 Response Body" @click="copyBody">▢</button><button class="icon-button action-icon" title="另存 Response Body" aria-label="另存 Response Body" @click="saveBody">↓</button></template>
          <span v-if="activeTab.responseAt" class="response-time">{{ activeTab.responseAt }}</span>
        </div></div>
        <div v-if="currentResponse" class="response-meta">
          <span class="status-pill" :class="responseStatusClass(currentResponse.status)"><span></span>{{ currentResponse.status }} {{ currentResponse.statusText }}</span>
            <span class="meta-item"><span class="meta-icon">◷</span>{{ currentResponse.durationMs }} ms</span><span class="meta-item"><span class="meta-icon">▤</span>{{ currentResponse.truncated ? '≥ ' : '' }}{{ prettySize(currentResponse.responseSize) }}</span>
          <span v-if="currentResponse.truncated" class="truncated-badge">內容已截斷</span>
        </div>
        <div v-else-if="activeTab.requestState === 'running'" class="response-meta running-meta"><span class="loader-dot"></span>正在等待 Response</div>
        <div v-else class="response-meta response-meta-empty"><span class="meta-muted">{{ activeTab.requestState === 'idle' ? '尚未傳送 Request' : activeTab.requestState === 'canceled' ? 'Request 已取消' : '本次 Request 沒有收到 HTTP Response' }}</span></div>
        <div v-if="activeTab.requestError" class="error-banner" :class="`error-${activeTab.requestError.kind}`" role="status"><span class="error-symbol">{{ activeTab.requestError.kind === 'timeout' ? '◷' : activeTab.requestError.kind === 'canceled' ? 'Ⅱ' : '!' }}</span><div><strong>{{ activeTab.requestError.kind === 'timeout' ? 'Request 逾時' : activeTab.requestError.kind === 'canceled' ? '已取消' : activeTab.requestError.kind === 'invalid_request' ? 'Request 設定錯誤' : '連線失敗' }}</strong><p>{{ activeTab.requestError.message }}</p><p v-if="currentResponse" class="error-context">下方 Response 為 {{ activeTab.responseAt }} 收到的前次回應。</p></div></div>
        <div v-if="currentResponse" class="response-tabs"><button :class="{ selected: responseSection === 'body' }" @click="responseSection = 'body'">Body</button><button :class="{ selected: responseSection === 'headers' }" @click="responseSection = 'headers'">Headers <span class="tab-count">{{ currentResponse.headers.length }}</span></button></div>
        <div v-if="currentResponse && responseSection === 'headers'" class="response-scroll header-scroll">
          <table class="header-table"><thead><tr><th>Header</th><th>Value</th></tr></thead><tbody><tr v-for="(header, index) in currentResponse.headers" :key="`${header.name}-${index}`"><td :class="{ 'cookie-header': header.name.toLowerCase() === 'set-cookie' }">{{ header.name }}<span v-if="header.name.toLowerCase() === 'set-cookie'" class="cookie-tag">Cookie</span></td><td>{{ header.value }}</td></tr></tbody></table>
          <div v-if="!currentResponse.headers.length" class="empty-state">此 Response 沒有 Headers。</div>
        </div>
        <div v-else-if="currentResponse && responseSection === 'body'" class="response-content">
          <div class="body-toolbar"><div class="body-view-tabs">
            <template v-if="responseKind === 'html'"><button :class="{ selected: bodyView !== 'preview' }" @click="bodyView = 'raw'">Source</button><button :class="{ selected: bodyView === 'preview' }" @click="bodyView = 'preview'">Preview</button></template>
            <template v-else-if="responseKind === 'xml'"><button :class="{ selected: bodyView !== 'raw' }" @click="bodyView = 'formatted'">Pretty</button><button :class="{ selected: bodyView === 'raw' }" @click="bodyView = 'raw'">Raw</button></template>
            <template v-else-if="responseKind === 'json' && jsonValue !== null"><button :class="{ selected: bodyView !== 'tree' }" @click="bodyView = 'formatted'">Pretty</button><button :class="{ selected: bodyView === 'tree' }" @click="bodyView = 'tree'">Tree</button><button :class="{ selected: bodyView === 'raw' }" @click="bodyView = 'raw'">Raw</button></template>
            <span v-else class="body-type-label">{{ responseKind === 'binary' ? mimeType || 'Binary' : responseKind.toUpperCase() }}</span>
          </div><label v-if="responseIsText" class="response-search"><span>⌕</span><input v-model="search" placeholder="搜尋 Body" aria-label="搜尋 Response Body" /></label></div>
          <div v-if="currentResponse.truncated" class="truncated-note"><strong>Response 超過 5 MiB 上限</strong><span>目前顯示前 5 MiB，Response Body 已截斷且不是完整內容。</span></div>
          <div class="response-scroll body-scroll">
            <iframe v-if="responseKind === 'html' && bodyView === 'preview'" class="html-preview" title="隔離的 HTML 預覽" sandbox="" referrerpolicy="no-referrer" :srcdoc="`<!doctype html><html><head><meta charset=&quot;utf-8&quot;><meta http-equiv=&quot;Content-Security-Policy&quot; content=&quot;default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; font-src data:&quot;><meta name=&quot;viewport&quot; content=&quot;width=device-width, initial-scale=1&quot;><style>body{font:14px system-ui,sans-serif;padding:20px;overflow-wrap:anywhere}img{max-width:100%}</style></head><body>${responseText}</body></html>`"></iframe>
            <div v-else-if="responseKind === 'image'" class="image-preview-wrap"><img class="image-preview" :src="`data:${mimeType};base64,${currentResponse.bodyBase64}`" alt="Response 圖片" /><span>{{ mimeType }} · {{ prettySize(currentResponse.responseSize) }}</span></div>
            <div v-else-if="responseKind === 'binary'" class="binary-state"><span class="binary-icon">▧</span><h2>無法預覽此二進位內容</h2><p>{{ mimeType || '未知格式' }} · {{ prettySize(currentResponse.responseSize) }}</p><button class="secondary-button" @click="saveBody">另存 Body</button></div>
            <JsonNode v-else-if="bodyView === 'tree' && jsonValue !== null" :value="jsonValue" />
            <pre v-else class="response-code" :class="`language-${responseKind}`" :style="{ fontSize: `${sourceFontPixels}px` }" v-html="syntaxMarkup(formattedText, responseKind === 'html' ? 'xml' : responseKind, search)"></pre>
          </div>
        </div>
        <div v-else-if="!activeTab.requestError" class="response-empty"><div class="empty-illustration"><span class="empty-page"></span><span class="empty-arrow">←</span></div><h2>Response 將顯示於此</h2><p>輸入網址並按下 Send，查看回應內容。</p></div>
      </section>
    </section>
    </div>
    <footer class="statusbar"><span class="statusbar-left"><span class="statusbar-led"></span>Vexa v0.2.1 <span class="status-divider">·</span> HTTP requests run locally</span><span class="statusbar-right">{{ activeTab.method }} <span class="status-divider">·</span> {{ tabs.length }} {{ tabs.length === 1 ? 'Tab' : 'Tabs' }} <span class="shortcut-hint">Ctrl T 新增　Ctrl W 關閉　Esc 取消</span></span></footer>
    <Transition name="toast"><div v-if="notice" class="toast-message">{{ notice }}</div></Transition>
  </main>
</template>

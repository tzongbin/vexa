# Vexa v0.2.0

Vexa 是一套輕量、純本機的桌面 HTTP/API Client。打開程式後即可輸入網址、設定 Request 並查看 Response；不需要帳號或雲端服務。

## 技術架構

- **桌面殼層：** Tauri 2
- **介面：** Vue 3 Composition API、TypeScript strict、Vite
- **HTTP Engine：** Rust、reqwest、rustls
- **平台：** Windows 桌面優先；未在其他作業系統驗證
- **儲存：** Rust 管理 EXE 同目錄的 `Data/settings.json`、`Data/history.json`、`Data/session.json`；不使用 SQLite

Vue 只負責編輯與顯示。所有目標 HTTP/HTTPS 請求都由 Rust Engine 發送；前端不使用 `fetch` 呼叫目標 API。前端透過必要的 Tauri 指令呼叫 Rust HTTP Engine、讀寫 JSON 資料及儲存使用者選取的 Response 路徑，不取得一般檔案系統讀寫權限。檔案選擇與剪貼簿透過各自的 Tauri 外掛權限處理。

## 開發環境

- Windows 10/11
- Node.js 20.19+ 或 22.12+、npm
- Rust stable MSVC 工具鏈、Windows SDK、Microsoft C++ Build Tools
- WebView2 Runtime

安裝相依套件：

```powershell
npm install
```

啟動前端與 Tauri 開發模式：

```powershell
npm run tauri dev
```

只執行前端 UI（Tauri IPC 指令不會運作）：

```powershell
npm run dev
```

## 檢查與建置

若 PowerShell 找不到 Cargo，可先將使用者的 Rust 工具目錄加入目前工作階段的 PATH：

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
```

```powershell
cargo fmt --manifest-path src-tauri\Cargo.toml --check
cargo test --manifest-path src-tauri\Cargo.toml
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
npm run build
npm run test:ui
npm run tauri build -- --no-bundle
npm run test:icons
npm run test:tauri
```

`npm run test:ui` 使用本機 Edge 和測試用 Mock IPC 驗證 Vue 互動與 Response Viewer；它不代表 Rust 或 Tauri IPC 已通過。`npm run test:tauri` 啟動 Release EXE、使用 WebView2 DevTools 連接實際視窗，並將請求送到測試期間動態配置的 localhost HTTP Server，也會在隔離暫存 Portable 副本驗證 JSON 資料。此測試需要 Windows WebView2 Runtime 與 DevTools 連線。Rust 單元測試使用隔離暫存目錄及動態 localhost Port；測試服務不會加入正式產品。

正式 Windows 執行檔輸出於 `src-tauri\target\release\vexa.exe`。設定、History 或 Session 首次寫入時建立 EXE 同目錄的 `Data` 資料夾；預設值不會建立 JSON 檔。攜帶資料時請一併複製 EXE 與 `Data` 資料夾。開發模式依 Debug 執行檔位置，在 `src-tauri\target\debug\Data` 使用獨立資料。所有模式都不依賴目前工作目錄。JSON 寫入採暫存檔與原子替換；損壞檔案保留原樣並提示。若需要 NSIS 安裝包，執行 `npm run tauri build`，輸出位於 `src-tauri\target\release\bundle\nsis`。

設定內容只包含主題、全域字級及 Response Source 字級。舊版 `vexa.ui.fontSize`、`vexa.ui.theme` 與 `vexa.response.sourceFontSize` 只會在尚無 JSON 時遷移；JSON 優先。損壞或不支援版本的 JSON 會保留原檔並顯示提示，必須先手動修復才能保存後續變更。

```json
{
  "version": 1,
  "appearance": {
    "theme": "system",
    "fontSize": "large",
    "sourceFontSize": null
  },
  "historyLimit": 1000
}
```

`history.json` 使用 `{ "version": 1, "entries": [...] }`，每筆包含唯一 ID、UTC 執行時間與 Request 快照，不包含 Response。`session.json` 使用 `{ "version": 1, "tabs": [...], "activeTabId": "..." }`。History 與 Session 由 Rust 驗證及讀寫，JSON 檔案上限為 32 MiB。

Windows 應用使用 WebView2；Tauri 設定會在安裝包流程加入 Evergreen Bootstrapper。單獨複製 EXE 到其他電腦時，目標電腦仍須具備 WebView2 Runtime。Node.js 與 Rust 不屬於執行時需求。

## v0.2.0 功能

- Compact Layout：Tab Bar 與 Request / Response 標題列依全域字級縮放保持精簡；新增 Tab 按鈕固定跟隨最後一個 Tab，多 Tab 時可水平捲動。
- Request / Response 左右分割、拖曳調整比例、深色/淺色/跟隨系統主題；藍色互動主題及 2xx/3xx/4xx/5xx 語意色彩。主題、字級及 Source 個別字級自動保存於 Portable JSON 設定檔。
- Small 14px、Normal 16px、Large 18px、XLarge 20px 四級 UI 字體大小；預設 Large。Response Source 可用 Ctrl+滾輪逐次調整 12–32px，預設跟隨全域字級，個別調整後獨立保存並套用所有 Tabs。
- 藍色雙向資料交換 Logo；原始資產為 `public\vexa-logo.svg`，同一圖樣產生 Tauri Windows ICO、工作列/視窗圖示與平台圖示。
- Send 時為未指定協定的網域補上 HTTPS，Loopback 補上 HTTP；保留明確指定的 HTTP/HTTPS 並拒絕其他協定。
- 多 Tabs；各 Tab 獨立保存 Request、Response 與執行狀態，關閉執行中 Tab 會取消 Request。
- Tab Bar 水平捲動；溢出時顯示左右按鈕，支援 Shift+滾輪與觸控板原生水平捲動；新增或切換 Tab 自動捲至可見位置，右側工具列固定。
- Send 按鈕只顯示圖示與 Send；URL Enter 與 Ctrl+Enter 發送，Body Enter 保留換行，IME 組字不會誤送。
- JSON History 與 Session：每次執行保存獨立 Request 快照，不保存 Response；預設上限 1,000，可選 100/500/1,000/5,000/10,000，超限淘汰最舊資料。
- History 可搜尋 URL、篩選 Method、依日期分組、分批顯示及清除全部/今天/7 天以前/30 天以前。點選記錄會開新 Tab，不會重新送出；已開啟的記錄會切換至既有 Tab。
- Session 以 Debounce 保存 Tabs、順序、作用中的 Tab 與 Request；重新啟動後還原 Request，但所有 Response 保持空白。
- Auth 秘密值與常見 Authorization/API Key/Cookie Header、秘密 Query/欄位會遮罩。Body 預設不保存；每個 Request 可明確允許保存 Body。JSON/Text 無法自動辨識所有秘密值；Multipart 檔案路徑永不保存。資料被省略時，還原後會要求檢查並確認才可送出。
- GET、POST、PUT、PATCH、DELETE、HEAD、OPTIONS。
- URL Query、重複 Key/Headers、欄位啟用/停用。
- None、Basic、Bearer、API Key Header / Query Authorization。
- JSON、Text、Form URL Encoded、Multipart Form Data（文字與使用者選取檔案欄位）。
- 預設 30 秒 Timeout、Cancel、最多 10 次 Redirect、正常 TLS 憑證驗證。
- HTTP 狀態、耗時、大小、Headers、Set-Cookie、JSON Pretty/Tree、HTML Source/隔離預覽、XML/JS/CSS/文字語法顯示、圖片預覽、Binary 另存、搜尋與複製。
- 回應 Body 上限 5 MiB；超過時清楚標記並停止讀取後續內容。Multipart 單檔上限 50 MiB。
- `Ctrl+T` 新增 Tab、`Ctrl+W` 關閉 Tab、`Ctrl+Enter` 發送、`Esc` 取消。

## 已知限制

- Response 及 JSON Tree 以記憶體資料顯示；JSON 超過 1 MiB 時不進行格式化與 Tree 解析，Response Body 超過 5 MiB 時會截斷並標示不完整。
- 文字編碼目前以 UTF-8 解碼；未依 Response charset 自動轉碼。PDF、影音及未知 Binary 僅提供另存。
- HTML 預覽使用無權限的 sandbox iframe，阻擋 Script 與網路資源；不可用來驗證需要 JavaScript 或外部資源的頁面。
- 檔案上傳採明確選取的檔案路徑；單檔上限 50 MiB，檔案會載入記憶體後送出。
- JSON/Text Body 的任意內容無法自動判斷所有秘密值；若允許保存 Body，該內容可能留在 History/Session。每個 JSON 資料檔有 32 MiB 上限；磁碟唯讀或資料損壞時會提示並保留損壞原檔。
- Windows Server 2016、Linux、macOS 相容性未驗證；安裝包及其他 Windows 版本需另外驗證。

## 不包含

不包含 SQLite、Collection、Workspace、Environment、變數、腳本、Mock Server、Cloud Sync、登入、AI、自動更新或自訂 TLS 安全設定。

# Vexa — AGENTS.md

## 1. 專案定位

Vexa 是 **Local-first、輕量、跨平台的桌面 HTTP/API 測試工具**。目標是快速輸入網址、送出 Request、查看 Response，而不是建立完整的 API 管理平台。

- 專案目錄：`D:\Dropbox\_prg\_Vexa\`
- 優先平台：Windows 10 / 11、Windows Server 2016（須在實機或對應環境驗證）
- 未來保留 Linux / macOS 相容性，不為尚未需要的平台加入額外複雜度。
- 技術：Tauri 2、Vue 3、TypeScript、Rust；Portable JSON 保存設定、History 與 Session，不使用 SQLite。
- 介面：桌面大畫面優先，Request 與 Response 左右分割，分割比例可調整。
- 發布：Portable 優先、盡可能單一 EXE；Windows 需留意 WebView2 Runtime。不得宣稱所有環境均完全無相依。

## 2. 產品原則及不可擴充範圍

1. **簡單優先**：啟動後應能直接輸入 URL 並測試，不得要求建立任何容器或專案。
2. **最小功能集合**：僅實作需求明確指定的功能；避免「順便」增加新功能。
3. **本機優先**：不需要帳號、登入、雲端服務與遙測；資料保留在本機。
4. **明確不做**：Collection、Workspace、Project、Environment、變數替換、Mock Server、API 測試腳本、團隊協作、雲端同步、自訂 TLS 安全設定頁面。
5. **安全預設**：TLS 憑證正常驗證，不提供預設略過驗證；Request 的 URL、Header 與 Body 必須以使用者輸入為準。
6. 未經使用者明確授權，不變更上述產品邊界或新增相依套件、複雜框架及背景服務。

## 3. 核心功能規格

### 3.1 Request

- 支援基本 HTTP Methods：GET、POST、PUT、PATCH、DELETE、HEAD、OPTIONS。
- 可編輯 URL、Query Parameters、Headers、基本 Authorization（None / Basic / Bearer / API Key）及 Body。
- Body 基本類型：None、JSON、Text、`application/x-www-form-urlencoded`、Multipart Form Data（含基本檔案上傳）。
- 支援 Timeout（建議預設 30 秒）、Cancel、一般 HTTP/HTTPS 與 Redirect；不自行加入進階 API 管理功能。
- **實際 HTTP 請求由 Rust 核心發送**，不要依賴 WebView 的 `fetch` 呼叫目標 API，以免受到瀏覽器 CORS 限制。
- Tauri IPC 只暴露必要指令；前端不得任意取得檔案系統或作業系統權限。

### 3.2 Response Viewer

- 顯示 HTTP Status、耗時、Response Size、Headers、Cookie 與 Body。
- 支援 JSON 格式化、語法高亮、節點折疊；XML、HTML、Text、JavaScript、CSS 的適當文字檢視。
- HTML 提供原始碼與安全預覽；不得在擁有 Tauri 權限的應用主視窗中直接執行不受信任的 HTML/JavaScript。
- 圖片預覽；PDF 預覽視平台能力而定；不支援預覽的 Binary 可儲存檔案。
- 提供搜尋、複製、另存 Response 等基本操作。
- 大型 Response 不得無限制透過 IPC 複製、解析或渲染；必須有大小限制、預覽或串流/檔案處理策略，避免 UI 卡死。

### 3.3 Tabs

- 多 Tab；每個 Tab 擁有獨立的 Request、Response 與執行狀態。
- Request 執行期間切換 Tab，不得將完成結果寫入其他 Tab。
- 調整 Request 不自動清空前次 Response；UI 應標示 Response 對應的執行時間或狀態。
- 自動保存各 Tab 的 Request 設定、順序及最後作用中的 Tab；程式重新開啟時還原。
- **Tab 的 Response 只保留於當次執行記憶體，不跨重啟還原。**
- 關閉最後一個 Tab 時建立空白 Tab。
- 對於尚在執行的 Request，須有一致的取消、關閉或完成結果處理方式。
- Tab Bar 的清單可水平捲動，右側本機/主題/設定工具固定，不因 Tab 數量換行或被擠壓；新增及切換 Tab 時自動捲動至可見範圍。
- Session 保存 Tab Request 與順序/選取狀態；一般 Tab 的 Response 不跨重啟保存。

### 3.4 History

- 每次開始 Request 執行建立獨立記錄；相同 URL 不覆寫，且可在該 Request 停用記錄。
- History 保存於 EXE 同目錄 `Data/history.json`，只保存執行時間與 Request 快照；不保存 Response Body、Headers、Status 或耗時。
- **預設最多 1,000 筆**，可選 100、500、1,000、5,000、10,000；超過上限立即淘汰最舊記錄。
- History 支援 URL 搜尋、Method 篩選、日期分組、分批顯示，以及清除全部/今天/7 天以前/30 天以前。
- 清除 History 不影響目前開啟的 Tabs。
- History 項目點選後在**新 Tab**還原 Request；Response 為空且**不得自動重新送出**。若該 History 已開啟，優先切換到原 Tab。
- 不實作 Collection 或其他 API 管理層。

### 3.5 Session

- 使用 EXE 同目錄 `Data/session.json` 自動保存 Tab 順序、目前選取 Tab 與 Request 設定；採 Debounce，避免每個字元都寫檔。
- 重啟時還原 Tabs 與 Request，所有 Response 皆為空白；關閉最後一個 Tab 時建立空白 Tab。
- Session 與 History 使用版本欄位、格式驗證、原子替換與寫入序列化；損壞檔案保留原樣並提示，不得阻止啟動。

## 4. 資料儲存與敏感資訊

- Portable JSON 是設定、History 與 Session 的儲存機制：`Data/settings.json`、`Data/history.json`、`Data/session.json`。正式路徑以 EXE 所在目錄為準。
- Portable 資料目錄以程式所在目錄的 `Data` 為目標，但必須處理不可寫入、權限不足及多程序同時開啟等情況；不得默默遺失資料。
- JSON 檔案須具版本、格式驗證、原子替換與序列化寫入；損壞時保留原始檔並提示，不可因此崩潰。
- **不可把 Token、Password、API Key、Authorization、Cookie 當作普通明文無條件保存。** History/Session 遮罩常見秘密 Header、Query 參數與欄位；Auth 秘密值不保存，還原後提醒並要求使用者確認才可送出。
- Body 預設不保存；使用者可逐 Request 明確允許保存。常見密碼類表單欄位仍遮罩，Multipart 檔案路徑永不保存。JSON/Text 任意內容無法可靠辨識所有秘密值，UI 必須說明此限制。
- 不向外部服務傳送使用者 Request、Response 或診斷資料，除非使用者明確要求送往其指定目標 API。

## 5. 架構與程式品質

- 前端：Vue 3 Composition API、TypeScript strict；保持狀態與 UI 元件職責清楚。
- 後端：Rust 負責 HTTP、JSON I/O 及平台整合；UI 不直接實作目標 API 傳輸或任意檔案存取。
- Request、Response、Tab 與 History 的模型分離；History 是不可隨當前編輯內容變動的執行快照。
- 每次非同步請求以穩定 Tab ID / Request execution ID 關聯，防止切換頁籤、重送與取消時競態。
- 正確處理 Redirect、壓縮回應、Content-Type / Charset、二進位、網路失敗、TLS 驗證錯誤及 Timeout。
- 檔案路徑、大小限制與輸入資料均需驗證；對不受信任的 Response 做安全隔離，避免 HTML 注入及 Tauri 權限提升。
- 依實際需要選用穩定、維護中的套件；避免引入重量級功能庫只為少量功能。
- 優先保持程式碼簡潔可測，不進行與任務無關的大規模重構。

## 6. UI / UX 約束

- 優先桌面寬螢幕；主要介面為 Tab Bar + 左側 Request + 右側 Response。
- Request / Response 左右分割比例可拖曳調整，雙側各自獨立捲動。
- History 面板可開關，預設不佔用主要工作畫面；不要加上 Collection Sidebar。
- 清楚區分未送出、執行中、成功、HTTP 錯誤、網路錯誤與取消狀態。
- 不因加入次要功能增加常用 Send 流程的操作步驟。
- 確保文字可讀性、鍵盤操作、長 URL/JSON 的呈現及適當的空白/錯誤狀態。

## 7. 開發流程與驗證

- **先檢查現有程式碼、設定及 `AGENTS.md`，再修改。** 不假設專案一定已初始化。
- 任務以可獨立測試的小切片完成；只修改與當次需求有關的檔案。
- 在可用的開發環境下執行對應的 TypeScript 檢查、前端測試、Rust 測試、格式/靜態檢查及建置；不能執行時需明確說明原因。
- 至少覆蓋：Request 組裝、HTTP 成功/失敗、Timeout/Cancel、多 Tab 回應歸屬、Tab 還原、History 自動淘汰、History 清除、History 還原不自動發送、敏感資料與大型 Response 邊界。
- Windows 10/11/Server 2016 相容性以實際測試為準，不憑推測宣稱全面通過。
- 不在測試中使用真正的正式環境憑證或秘密值；不對正式 API 執行具有副作用的測試。
- **除非使用者明確要求，不自行 Commit、Push、建立 Tag、發佈或部署。**
- 不覆蓋使用者原有未提交變更，不修改與任務無關的檔案。

## 8. 任務回報格式

完成每個任務後，請以**繁體中文**簡要回報：

1. 本次完成內容與重要設計決策。
2. 主要修改檔案。
3. 實際執行的測試及結果；未測試項目與原因。
4. 已知限制、風險或需要使用者確認的事項。
5. 是否有 Commit / Push / Deploy（預設均不執行）。

**最高優先原則：Vexa 是一個快速、輕量、純本機的 HTTP/API Client；不要把它擴充成 Postman。**

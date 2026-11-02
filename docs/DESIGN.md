# PadBridge 架構設計

> 狀態：草案 v0.1（2026-11-02）・授權：MIT

PadBridge 讓 iPad 與 Windows 雙向共享畫面，有線（USB）與無線（Wi‑Fi / 區網）都走同一套協定：

| 模式 | 方向 | 說明 |
|---|---|---|
| **Display** | Windows → iPad | iPad 成為 Windows 的擴充（或鏡像）螢幕，觸控 / Apple Pencil 回傳 |
| **Mirror** | iPad → Windows | iPad 整機畫面顯示在 Windows 視窗中 |

## 1. 元件

```
┌──────────────────────── Windows ────────────────────────┐        ┌──────────────── iPad ────────────────┐
│ 虛擬螢幕驅動 (IddCx, 沿用 MIT 開源 VDD)                    │        │ PadBridge App (Swift)                 │
│        │ 新增/移除螢幕、設定解析度                           │        │  ├─ Display 接收端                     │
│ padbridge-host (Rust)                                    │        │  │   Network.framework 監聽 :47800     │
│  ├─ 擷取  Windows.Graphics.Capture / DXGI Duplication    │        │  │   VideoToolbox 解碼                 │
│  ├─ 編碼  Media Foundation 硬體 MFT (H.264 / HEVC)        │  TCP   │  │   AVSampleBufferDisplayLayer        │
│  ├─ 輸入  InjectSyntheticPointerInput (觸控 / 筆)         │◀──────▶│  │   觸控 / Pencil → Pointer 訊息       │
│  ├─ 鏡像檢視器  MF 解碼 + D3D11 呈現                       │        │  └─ Bonjour 廣播 _padbridge._tcp       │
│  ├─ 傳輸  usbmux（USB）/ mDNS + TCP（無線）                │        │ Broadcast Upload Extension (ReplayKit)│
│  └─ 共用 crates/protocol                                 │        │   VTCompressionSession → 監聽 :47801   │
└──────────────────────────────────────────────────────────┘        └───────────────────────────────────────┘
```

## 2. 傳輸層：有線與無線共用一條 TCP 串流

**iPad 一律當伺服器（listen），Windows 一律主動連線。**這樣 USB 與無線只差「怎麼拿到那條 socket」，之後的協定完全相同。

### 2.1 USB：usbmux

- Windows 安裝 Apple 裝置驅動（Microsoft Store 的「Apple 裝置」App 或 iTunes）後，`Apple Mobile Device Service` 會在 `127.0.0.1:27015` 提供 usbmuxd。
- Host 連上 27015，送 plist 封包：
  1. `ListDevices` / `Listen`：取得 / 監看已接上的 iPad 的 `DeviceID`。
  2. `Connect`：帶 `DeviceID` 與 `PortNumber`（**網路位元組序**，即 `htons(port)`）。
  3. 回 `Result = 0` 之後，這條 socket 就成為直通 iPad 該埠的透明管道。
- usbmux 只支援「電腦 → 裝置」方向的連線，這也是 iPad 必須當伺服器的原因。
- 參考：libimobiledevice / usbmuxd（LGPL，僅參考協定，不連結）、peertalk。

### 2.2 無線：mDNS + TCP

- iPad 以 Bonjour 廣播 `_padbridge._tcp`，TXT 記錄帶 `role`、`name`、`proto`（協定版本）。
- Windows 用 `mdns-sd` crate（或 Win10 1809+ 內建的 `DnsServiceBrowse`）瀏覽，取得 IP 後連 TCP。
- iPad 若插 USB‑C 網路卡，則走有線區網，同一條路徑。
- iPad 首次使用本機網路會跳出權限提示（`NSLocalNetworkUsageDescription`、`NSBonjourServices`）。

### 2.3 連接埠

| 埠 | 監聽者 | 用途 |
|---|---|---|
| 47800 | iPad 主 App | Display 模式 |
| 47801 | iPad 廣播擴充 | Mirror 模式 |

## 3. 線路協定（`crates/protocol`）

所有整數皆為 **little-endian**，浮點數為 IEEE‑754 `f32` LE。

```
訊框: [len: u32][kind: u8][body ...]      len = 1 + body 長度，上限 16 MiB
字串: [n: u16][UTF-8 bytes; n]
```

| kind | 訊息 | body |
|---|---|---|
| 0x01 | Hello | magic `"PDBR"`(4) · version u16 · role u8 · name str |
| 0x02 | DisplayInfo | width_px u32 · height_px u32 · scale_milli u16 · max_fps u16 · codecs u8(bitmask) |
| 0x03 | StreamConfig | codec u8 · width u32 · height u32 · fps u16 · bitrate_kbps u32 |
| 0x10 | CodecParams | codec u8 · 參數集（每個 NAL 前置 u32 **big-endian** 長度）至訊框結尾 |
| 0x11 | VideoFrame | pts_us u64 · flags u8(bit0=keyframe, bit1-2=順時針旋轉 90° 次數) · AVCC NALs 至訊框結尾 |
| 0x12 | RequestKeyframe | — |
| 0x20 | Pointer | id u32 · kind u8 · phase u8 · x f32 · y f32 · pressure f32 · altitude f32 · azimuth f32 |
| 0x30 | Ping | nonce u64 · t_us u64 |
| 0x31 | Pong | nonce u64 · t_us u64（原樣回傳） |
| 0x40 | Log | level u8 · text str |
| 0x7F | Bye | — |

列舉值：

- role：0 Host、1 DisplaySink、2 MirrorSource
- codec：1 H.264、2 HEVC（bitmask：bit0 H.264、bit1 HEVC）
- pointer kind：0 Touch、1 Pen、2 Mouse
- phase：0 Down、1 Move、2 Up、3 Cancel、4 Hover

另外有幾條約定：

- 視訊 NAL 使用 AVCC（4 bytes 長度前綴，**big-endian**，與 VideoToolbox 原生格式一致）。Windows 端的 Media Foundation 輸出 Annex‑B，送出前要轉換。
- Pointer 的 `x`、`y` 是相對於視訊畫面的正規化座標（0..1），`altitude` 與 `azimuth` 的單位是弧度。
- 交握流程：連線後雙方各送一次 `Hello`。Display 模式下，iPad 接著送 `DisplayInfo`，Host 回 `StreamConfig` 和 `CodecParams`，然後開始送 `VideoFrame`。
- 版本規則：`version` 不同就送 `Bye` 並中斷。新增訊息一律使用新的 kind；收到不認得的 kind 時直接略過。

## 4. 低延遲策略

- 編碼器關掉 B‑frame，使用 low‑latency / ultra-low-latency 模式，GOP 設長，需要時才插入 IDR。
- **背壓就丟幀**：socket 寫入佇列超過 1～2 幀時丟棄非關鍵幀；丟過之後下一幀強制為關鍵幀（或由接收端送 `RequestKeyframe`）。
- 只在畫面有變動時才編碼（Windows.Graphics.Capture 本身就是事件驅動）。
- iPad 端以 `AVSampleBufferDisplayLayer` 加上 `kCMSampleAttachmentKey_DisplayImmediately` 立即顯示，不排隊。
- 用 `Ping` / `Pong` 量 RTT，並顯示在除錯面板。
- 初始目標：USB 1080p60 的單向延遲 < 50 ms；Wi‑Fi < 80 ms。

### 4.1 Mirror 模式實作（M3）

- iPad：廣播擴充把 ReplayKit 的每一幀交給 `MirrorStreamer`（`PadBridgeKit/PadBridgeMedia`）。
  - 超過 1920 px 的畫面先用 `VTPixelTransferSession` 縮小，再交給 `VTCompressionSession` 編碼。
  - 每個關鍵幀前都會附上 `StreamConfig` 和 `CodecParams`。
  - 旋轉方向取自 `RPVideoSampleOrientationKey`，放在 `VideoFrame` 的 flags 裡。
- 背壓：`Server.broadcastVideo` 對「積壓 ≥ 6 則訊息」或「正在等關鍵幀」的連線直接丟棄差異幀，並要求編碼器產生關鍵幀。
- 靜止畫面：新連線加入，或收到 `RequestKeyframe` 時，如果 ReplayKit 沒有送新幀，就用最後一幀重新編碼成關鍵幀。
- Windows：`padbridge mirror` 用 OpenH264 解碼（目前只保證支援 Constrained Baseline），再用 minifb 顯示；解碼失敗時會送 `RequestKeyframe`。
- 待辦：
  - 改用 Media Foundation 硬體解碼加 D3D11 顯示，支援 High profile 與 HEVC。
  - 改用 Cisco 發佈的 OpenH264 二進位檔，以涵蓋 H.264 專利授權。

## 5. 平台限制與因應

### 5.1 iPad 端：沒有 Mac 的開發流程

- Xcode 專案用 **XcodeGen**（`project.yml`）產生，不手改 `.pbxproj`。
- **GitHub Actions `macos` runner** 以 `xcodebuild archive CODE_SIGNING_ALLOWED=NO` 產出未簽章的 `.ipa`，作為 artifact。
- 在 Windows 上用 **Sideloadly** 以 Apple ID 簽章，再透過 USB 安裝：
  - 免費 Apple ID：每 7 天要重簽，最多 3 個 App，且廣播擴充也會佔一個 App ID。
  - 付費開發者帳號（年費 99 美元）：一年效期，並可使用 TestFlight。
- 除錯手段：
  - 協定內建 `Log` 訊息，iPad 的日誌直接顯示在 Host 端。
  - 系統日誌可用 libimobiledevice 的 `idevicesyslog` 在 Windows 上查看。
  - 需要互動式除錯時，再租用雲端 Mac。
- **廣播擴充不依賴 App Group**（免費帳號的權限受限）。擴充自己開 socket 監聽 47801；設定只透過協定交換。
- 廣播擴充的記憶體上限約 50 MB：只用 VideoToolbox 硬體編碼，不保留幀緩衝。
- Mirror 模式需要使用者每次從控制中心手動開始廣播（`RPSystemBroadcastPickerView` 可放在 App 內），這是 iOS 的限制。

### 5.2 Windows 端：虛擬螢幕驅動

- 不自己寫 IddCx 驅動。沿用 MIT 授權的開源 Virtual Display Driver，並透過它的控制介面新增螢幕、設定解析度與更新率。
- 公開發布的驅動需要微軟簽章，所以直接使用上游已簽章的版本；`padbridge-host` 只負責偵測是否已安裝並引導使用者安裝。
- 公司管理的電腦可能禁止安裝驅動。屆時退回「鏡像模式」：直接擷取既有螢幕，不新增螢幕。

### 5.3 觸控注入

- `InitializeTouchInjection` / `InjectTouchInput`，或 `CreateSyntheticPointerDevice`（PT_PEN / PT_TOUCH）加上 `InjectSyntheticPointerInput`。
- 座標要對應到虛擬螢幕在桌面上的位置（考慮 DPI 縮放）。

## 6. 專案結構

```
padbridge/
├─ LICENSE (MIT)
├─ Cargo.toml                 # Rust workspace
├─ crates/
│  ├─ protocol/               # 線路協定（無相依）
│  ├─ usbmux/                 # usbmuxd 客戶端          (M1)
│  ├─ cli/                    # 測試工具：devices / watch / ping
│  └─ host/                   # Windows 主程式           (M3+)
├─ ios/                       # Swift / XcodeGen       (M2+)
│  ├─ project.yml
│  ├─ App/
│  └─ BroadcastExtension/
├─ .github/workflows/         # Rust 測試 + iOS 未簽章 IPA
└─ docs/
```

## 7. 里程碑

| | 內容 | 驗證方式 |
|---|---|---|
| **M0** | 協定 crate + 測試 + 設計文件 | `cargo test` |
| **M1** | Rust usbmux 客戶端：列出 iPad、建立 Connect 通道 | 接上 iPad 後執行 `padbridge devices` |
| **M2** | iOS 骨架 + CI 產 IPA：監聽、Hello/Ping；Host 經 USB 與 Wi‑Fi 皆可連線並量 RTT | Sideloadly 安裝後量測 |
| **M3** | Mirror：廣播擴充編碼 → Windows 檢視器 | 在 Windows 視窗看到 iPad 畫面 |
| **M4** | Display：VDD + 擷取 + 編碼 → iPad 顯示 | iPad 出現在 Windows 顯示設定中 |
| **M5** | 觸控 / Pencil 回傳 + 壓力感應 | 小畫家 / Krita 可繪圖 |
| **M6** | 系統匣 UI、自動重連、120 Hz、HEVC、參數調整 | — |

## 8. 風險

- **免費 Apple ID 的 7 天限制**，會影響一般使用者的接受度。長期可能還是需要付費帳號才能上 TestFlight 或 App Store。另外，App Store 審核對「把 iPad 當螢幕」這類 App 沒有限制，Duet 已經上架。
- **Apple 裝置驅動是 USB 模式的前提。**README 要寫明安裝方式。
- **ReplayKit 擴充的記憶體與 CPU 限制**：高解析度加高位元率時可能被系統終止。預設先用 1080p 等比縮放。
- **Media Foundation 硬體編碼器的低延遲設定各家 GPU 不一致**：Intel、NVIDIA、AMD 要分別測試。必要時再接 NVENC / AMF SDK。

# PadBridge 架構設計

> 狀態：v0.4.0（2026-11-03）・授權：MIT

PadBridge 讓 iPad 與 Windows 雙向共享畫面，有線（USB）與無線（Wi‑Fi / 區網）都走同一套協定：

| 模式 | 方向 | 說明 |
|---|---|---|
| **Display** | Windows → iPad | iPad 成為 Windows 的擴充（或鏡像）螢幕，觸控 / Apple Pencil 回傳 |
| **Mirror** | iPad → Windows | iPad 整機畫面顯示在 Windows 視窗中 |

## 1. 元件

```
┌──────────────────────── Windows ────────────────────────┐        ┌──────────────── iPad ────────────────┐
│ 虛擬螢幕驅動 (IddCx, 沿用 MIT 開源 VDD)                    │        │ PadBridge App (Swift)                 │
│        │ 設定檔 + named pipe：解析度、螢幕數量               │        │  ├─ Display 接收端                     │
│ padbridge-host (Rust)                                    │        │  │   Network.framework 監聽 :47800     │
│  ├─ 擷取  DXGI Desktop Duplication                       │        │  │   VideoToolbox 解碼                 │
│  ├─ 編碼  MF 硬體 H.264 MFT（可選 GPU）/ OpenH264          │  TCP   │  │   AVSampleBufferDisplayLayer        │
│  ├─ 輸入  InjectSyntheticPointerInput (觸控 / 筆)         │◀──────▶│  │   觸控 / Pencil → Pointer 訊息       │
│  ├─ 鏡像檢視器  MF 硬體解碼 + D3D11 轉換 / OpenH264        │        │  └─ Bonjour 廣播 _padbridge._tcp       │
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

### 2.4 加密與配對（`secure.rs` / `Secure.swift`）

TCP 連上後、任何協定訊息之前，雙方先做握手。每一方都有一把長期的 X25519 身分金鑰。

```
hello（雙方，明文）：  "PBS1" · 身分公鑰 (32) · 臨時公鑰 (32)
transcript h = SHA-256("padbridge-secure-v1" · S_host · E_host · S_ipad · E_ipad)
secret       = DH(e_host, E_ipad) · DH(s_host, E_ipad) · DH(e_host, S_ipad)
keys         = HKDF-SHA256(salt h, secret, "padbridge-secure-v1 keys") → host→iPad (32) · iPad→host (32)
code         = SHA-256(h · "padbridge-secure-v1 code") 前 4 bytes（BE）mod 10^6
record       = 長度 u32 LE · ChaCha20-Poly1305(key, nonce = 0u32 · counter u64 LE, plaintext)
```

- 握手後第一個 record 雙方各送 intro（`0x01` · 名稱），iPad 接著送 verdict（`0x02` · 0 允許／1 使用者拒絕／2 此端點無法配對）。之後的 record 承載一般協定訊框（每個訊框一個 record）。
- 兩個「靜態 × 臨時」DH 項讓只有持有身分私鑰的一方能算出金鑰；已知金鑰無法被冒用。
- 新裝置要兩邊的使用者都確認同一組 6 位數字才配對（數字比對，被中間人攻擊的機率是百萬分之一）。之後雙方記住對方的公鑰，不再詢問。
- 存放：Windows 在 `%APPDATA%\PadBridge`（`identity.key` 以 DPAPI 加密、`paired.json`）；iPad 在 App Group 容器的 `pairing.json`。App Group 名稱從簽章後的 `embedded.mobileprovision` 讀取，因為重簽工具可能改名。
- 廣播擴充沒有 UI。有 App Group 時沿用 App 的配對（CI 以 ad-hoc 簽章寫入 entitlement）；免費 Apple ID 透過 Sideloadly 安裝時沒有 App Group，擴充改用自己的身分與配對清單：遇到未知電腦就回 verdict 3（等待確認），記下這台電腦並以 `finishBroadcastWithError` 結束廣播、顯示確認碼；使用者在 60 秒內重新開始廣播即視為允許（`BroadcastApproval`），Windows 端的自動重連隨後連上。Windows 端在自己的使用者確認後就記住該 iPad，重試時不再詢問。
- Rust 與 Swift 共用一組固定金鑰的測試向量（`secure::tests::test_vectors` / `SecureTests.testVectorsMatchRust`），確保兩邊逐位元組一致。

### 2.5 重新連線

- `DisplayEvent::Ended` / `MirrorEvent::Ended` 帶 `lost`：連線中斷（socket 錯誤、對方關閉、廣播結束）為 true；使用者停止、擷取／編碼錯誤、配對被拒為 false。
- App 對 `lost` 的工作階段每 2～10 秒重試，USB 裝置以序號（UDID）辨識，所以重新插上後 usbmux id 改變也找得到。
- iPad App 回到前景時，若監聽已失效就重新啟動。
- USB ⇄ Wi‑Fi 切換：Bonjour TXT 的 `id` 是 iPad 身分公鑰的前 8 bytes（hex）。Windows 連上後記住「USB 端點 → 指紋」；原端點消失而同一指紋的另一條路存在時，0.3 秒後就改走那條路；連在 Wi‑Fi 時若同一台 iPad 的 USB 出現，就切回 USB。

## 3. 線路協定（`crates/protocol`）

所有整數皆為 **little-endian**，浮點數為 IEEE‑754 `f32` LE。

```
訊框: [len: u32][kind: u8][body ...]      len = 1 + body 長度，上限 16 MiB
字串: [n: u16][UTF-8 bytes; n]
```

| kind | 訊息 | body |
|---|---|---|
| 0x01 | Hello | magic `"PDBR"`(4) · version u16（目前 2）· role u8 · name str |
| 0x02 | DisplayInfo | width_px u32 · height_px u32 · scale_milli u16 · max_fps u16 · codecs u8(bitmask) |
| 0x03 | StreamConfig | codec u8 · width u32 · height u32 · fps u16 · bitrate_kbps u32 |
| 0x10 | CodecParams | codec u8 · 參數集（每個 NAL 前置 u32 **big-endian** 長度）至訊框結尾 |
| 0x11 | VideoFrame | pts_us u64 · flags u8(bit0=keyframe, bit1-2=順時針旋轉 90° 次數) · AVCC NALs 至訊框結尾 |
| 0x12 | RequestKeyframe | — |
| 0x20 | Pointer | id u32 · kind u8 · phase u8 · x f32 · y f32 · pressure f32 · altitude f32 · azimuth f32 |
| 0x21 | Cursor | x f32 · y f32 · visible u8（正規化影像座標，由接收端繪製游標） |
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
- 旋轉：ReplayKit 的 `RPVideoSampleOrientationKey` 與 EXIF 的定義相反（實機確認：`.left` 需要順時針轉 90°），所以 `.left` → 1、`.right` → 3。
- Windows：`run_mirror` 分三個執行緒：
  - 讀取執行緒：解析訊框後放進佇列，讓積壓看得見。
  - 解碼執行緒：每一格都要解碼（後面的格會參考前面的），但只有佇列中**最新**的一格會產生畫面，交給一格大小的交接區（新的覆蓋舊的）。
  - 轉換執行緒：把交接區的畫面轉成 RGB 交給呼叫端。軟體解碼時在這裡做 YUV→RGB；硬體解碼時 GPU 已經轉好。
  - 這樣接收端比 iPad 慢時只會略過畫面，延遲不會累積。解碼失敗時會送 `RequestKeyframe`。
- 軟體路徑的 YUV→RGB（BT.601 limited range）與旋轉合併成一次多執行緒處理：每個輸出像素直接讀取旋轉前的位置，1334×1920 約 3.4 ms（原本分開轉換、重排、旋轉約 13 ms）。
- 實機（RTX 3050 Laptop、軟體解碼）：收到 60 fps、顯示約 60 fps、解碼約 14 ms。
- 待辦：
  - 支援 High profile 與 HEVC。
  - 改用 Cisco 發佈的 OpenH264 二進位檔，以涵蓋 H.264 專利授權。

### 4.2 Display 模式實作（M4）

- Windows（`crates/host`）：
  - `capture`：用 DXGI Desktop Duplication 擷取指定螢幕，程序設為 per-monitor DPI aware，所以拿到的是實體解析度與游標座標。
  - `display::VideoPipeline`：縮放後畫上游標（Desktop Duplication 不含游標），再用 OpenH264（`ScreenContentRealTime`）編碼。縮放比例在 2 倍內用 SWAR bilinear，超過用面積平均。
  - `run_display`：只有畫面或游標有變化時才編碼，並受 `max_fps` 限制；收到 `RequestKeyframe` 時，用最後一幀重新編碼成關鍵幀。
- iPad：`VideoReceiver` 把 `CodecParams` 和 `VideoFrame` 組成 `CMSampleBuffer`，送進 `AVSampleBufferDisplayLayer`（硬體解碼，並設定 `DisplayImmediately`）；串流期間全螢幕顯示。
- 跨平台驗證：`crates/host/examples/display_fixture.rs` 產生的 Windows 編碼串流放在 `PadBridgeMediaTests/Fixtures/windows_h264.bin`，CI 在 macOS 上用 VideoToolbox 解碼它。
- 效能（RTX 5080，release 版）：
  - 1440p → 1080p：約 38 fps，每幀編碼約 24 ms。
  - 4K → 1080p：約 18 fps，每幀約 57 ms。
  - 瓶頸是 CPU 縮放、讀回 4K 畫面、OpenH264 軟體編碼。
- 待辦：讓 iPad 支援 High profile 與 HEVC。

### 4.4 硬體編碼

- `hwenc::HardwareEncoder`：
  - Desktop Duplication 的貼圖先複製成自己的 GPU 貼圖（因為釋放幀之後原本的貼圖就失效），再用 D3D11 Video Processor 縮放並轉成 NV12。
  - 準備 4 張 NV12 貼圖輪流使用，避免非同步編碼器還在讀的時候被覆寫。
  - 用 `MFTEnumEx` 找出硬體 H.264 MFT，透過 `IMFDXGIDeviceManager` 共用同一個 D3D11 裝置，以非同步事件迴圈（NeedInput / HaveOutput）驅動。
  - 用 `ICodecAPI` 設定低延遲、CBR、無 B-frame、強制關鍵幀。
  - 目前輸出 Constrained Baseline，讓 OpenH264（Windows 端工具與測試）也能解碼。
- 擷取裝置以 `VIDEO_SUPPORT` 建立並開啟 multithread protection，因為 Media Foundation 會在自己的執行緒使用它。
- 找不到硬體編碼器時，自動改用 CPU 讀回加 OpenH264。
- 游標改用 `Cursor` 訊息送到 iPad，由 iPad 繪製（`CursorLayer`），所以只有游標移動時不用重新編碼。
- 基準測試：`cargo test --release -p padbridge-host bench_hardware -- --ignored --nocapture`。在 RTX 5080 上，4K → 1080p 每幀 1.94 ms，1440p → 1080p 每幀 1.93 ms。
- 同時有內顯與獨顯時，兩張卡各有自己的編碼器 MFT。`MFTEnumEx` 列出的編碼器會依 `MFT_ENUM_ADAPTER_LUID` 排序，優先使用與 D3D11 裝置同一張 GPU 的那個。

### 4.5 硬體解碼（Mirror）

- `hwdec::HardwareDecoder`：
  - 在選定 GPU 上建立具 video support 與 multithread protection 的 D3D11 裝置，透過 `IMFDXGIDeviceManager` 交給 Media Foundation 的 H.264 解碼器（同步 MFT，DXVA）。
  - 開啟 `CODECAPI_AVLowLatencyMode`，每輸入一格就立刻輸出（有測試確認沒有延遲一格）。
  - 輸出是 NV12 貼圖陣列的一個 slice；D3D11 Video Processor 依 `MF_MT_MINIMUM_DISPLAY_APERTURE` 裁掉 16 像素對齊的補白，順時針旋轉（`VideoProcessorSetStreamRotation`），轉成 BGRA，再讀回系統記憶體。
  - GPU 不支援旋轉時，改在 CPU 旋轉。
- `MirrorDecoder` 依 `DecoderChoice` 選擇硬體或 OpenH264；硬體初始化失敗時自動改用軟體，原因以 `MirrorEvent::Decoder` 回報。
- 基準（1334×1920，解碼＋旋轉＋讀回）：RTX 5080 約 3.8 ms／格；OpenH264 解碼 8.4 ms＋轉換 3.4 ms。

### 4.6 選擇 GPU

- `gpu::list_gpus`：列出 DXGI 的硬體介面卡，略過軟體介面卡（Microsoft Basic Render Driver），以及間接顯示驅動（例如 StarDesk、MuMu、VDD）額外產生、名稱與實體 GPU 相同的介面卡（以 `DisplayConfigGetDeviceInfo` 查出裝置路徑在 `ROOT\` 或 `SWD\` 下）。專用記憶體少於 512 MB 視為內顯。
- `Monitor.gpu`：負責繪製該螢幕的 GPU（也就是 Desktop Duplication 擷取所在的 GPU）。
- `EncoderChoice`（延伸螢幕）：自動（螢幕所在 GPU）／指定 GPU／CPU。指定的 GPU 不是螢幕所在 GPU 時，擷取改為讀回系統記憶體，再用 `UpdateSubresource` 上傳到編碼 GPU 的貼圖（`Upload`）。
- `DecoderChoice`（鏡像）：自動（獨顯優先）／指定 GPU／CPU。

### 4.7 虛擬螢幕

- `vdisplay`：用 `QueryDisplayConfig` 找出每個螢幕實際所屬的介面卡，裝置路徑在 `ROOT\` 或 `SWD\` 下就是虛擬螢幕；也用 SetupAPI 列出已安裝但沒有開螢幕的虛擬顯示卡。DXGI 會把間接顯示的輸出列在負責繪製的 GPU 底下，所以不能只看介面卡名稱。
- `vdd`：整合 Virtual Display Driver（硬體 ID `Root\MttVDD`）。
  - 安裝：用 winget 安裝上游的控制程式（`VirtualDrivers.Virtual-Display-Driver`）並開啟，由使用者在其中安裝已簽章的驅動。
  - 開啟螢幕：對 `\\.\pipe\MTTVirtualDisplayPipe` 送 `SETDISPLAYCOUNT 1`（UTF-16 訊息，任何使用者都能連）。
  - 配合 iPad 解析度：收到 `DisplayInfo` 後，若虛擬螢幕尺寸不同，就把解析度加進 `vdd_settings.xml`（路徑可由登錄檔 `VDDPATH` 改變）、送 `RELOAD_DRIVER`，再用 `ChangeDisplaySettingsEx` 切換（不需系統管理員權限），然後重新連線。

### 4.3 觸控回傳實作（M5）

- iPad：影像畫面上的 `UIView` 收集觸控事件，送出 `Pointer` 訊息。
  - 用 coalesced touches 取得 Pencil 的 120–240 Hz 取樣。
  - `UIHoverGestureRecognizer` 處理 Pencil 懸停與觸控板游標。
  - 座標透過 `PointerMapping`，換算成 aspect-fit 影像內的 0..1 座標：新的觸控落在黑邊會忽略；拖曳超出畫面則固定在邊緣。
  - 開啟 `UIApplicationSupportsIndirectInputEvents`，讓觸控板點擊成為 indirect pointer，送到 Windows 當滑鼠。
- Windows：`input::InputMapper` 是與平台無關的狀態機，有單元測試。
  - 觸控每一幀都要列出所有接觸點（Windows 規定），接觸點編號用 0–9 的槽位。
  - Pencil 抬起後會再送一次「離開範圍」。
  - 傾斜角由 altitude 和 azimuth 換算。
  - `input::Injector` 用 `CreateSyntheticPointerDevice` 加 `InjectSyntheticPointerInput`（Windows 10 1809+）注入觸控與筆；滑鼠用 `SetCursorPos` 加 `SendInput`。
- 座標對應到被擷取螢幕的實體像素矩形（`Monitor.left/top`）。
- 限制：
  - UIPI 會阻擋注入到以系統管理員身分執行的視窗。
  - 筆和滑鼠同時使用時，順序無法保證（指標注入是非同步的）。

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

- 不自己寫 IddCx 驅動。沿用 MIT 授權的開源 Virtual Display Driver，並透過它的設定檔與 named pipe 新增螢幕、設定解析度（見 4.7）。
- 驅動必須簽章，所以直接使用上游已簽章的版本：PadBridge 不散布驅動，只用 winget 取得上游的控制程式，由使用者在其中安裝。上游的安裝程序會把作者的 SignPath 憑證加入「受信任的發行者」。
- 公司管理的電腦可能禁止安裝驅動。屆時退回「鏡像模式」：直接擷取既有螢幕，不新增螢幕。

### 5.3 觸控注入

- `InitializeTouchInjection` / `InjectTouchInput`，或 `CreateSyntheticPointerDevice`（PT_PEN / PT_TOUCH）加上 `InjectSyntheticPointerInput`。
- 座標要對應到虛擬螢幕在桌面上的位置（考慮 DPI 縮放）。

## 6. 專案結構

```
padbridge/
├─ LICENSE (MIT)
├─ THIRD_PARTY_NOTICES.md     # 第三方授權與來源
├─ licenses/                  # 第三方授權全文
├─ Cargo.toml                 # Rust workspace
├─ crates/
│  ├─ protocol/               # 線路協定（無相依）
│  ├─ usbmux/                 # usbmuxd 客戶端          (M1)
│  ├─ host/                   # Windows 端邏輯
│  │   ├─ capture / display / hwenc   # 擷取、軟硬體編碼（Display）
│  │   ├─ mirror / hwdec              # 軟硬體解碼（Mirror）
│  │   ├─ gpu                         # GPU 列表與裝置
│  │   ├─ vdisplay / vdd              # 虛擬螢幕偵測與 VDD 整合
│  │   ├─ secure / pairing            # 加密握手、配對與金鑰存放
│  │   ├─ i18n                        # 中文／英文（tr! 巨集）
│  │   └─ input / discovery / target  # 觸控注入、探索、連線
│  ├─ cli/                    # 命令列 padbridge-cli
│  └─ app/                    # Windows 桌面 App PadBridge.exe（egui），settings.json 保存設定
├─ installer/                 # Inno Setup 安裝程式腳本
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
| **M1** | Rust usbmux 客戶端：列出 iPad、建立 Connect 通道 | 接上 iPad 後執行 `padbridge-cli devices` |
| **M2** | iOS 骨架 + CI 產 IPA：監聽、Hello/Ping；Host 經 USB 與 Wi‑Fi 皆可連線並量 RTT | Sideloadly 安裝後量測 |
| **M3** | Mirror：廣播擴充編碼 → Windows 檢視器 | 在 Windows 視窗看到 iPad 畫面 |
| **M4** | Display：VDD + 擷取 + 編碼 → iPad 顯示 | iPad 出現在 Windows 顯示設定中 |
| **M5** | 觸控 / Pencil 回傳 + 壓力感應 | 小畫家 / Krita 可繪圖 |
| **M6** | 系統匣 UI、自動重連、120 Hz、HEVC、參數調整 | — |

M0–M5 皆已完成，並於 2026-11-03 在實機（iPad、Windows 11、USB 與 Wi‑Fi）驗證；另外完成硬體解碼、GPU 選擇與 VDD 整合。

邁向 1.0：

| 版本 | 內容 |
|---|---|
| 0.2 | 配對與加密、設定保存 |
| 0.3 | 自動重新連線、iPad 回到前景時重啟監聽 |
| 0.4 | 執行檔版本資訊、英文介面、可調畫質、安裝程式、程式碼簽章政策與隱私說明 |

## 8. 風險

- **免費 Apple ID 的 7 天限制**，會影響一般使用者的接受度。長期可能還是需要付費帳號才能上 TestFlight 或 App Store。另外，App Store 審核對「把 iPad 當螢幕」這類 App 沒有限制，Duet 已經上架。
- **Apple 裝置驅動是 USB 模式的前提。**README 要寫明安裝方式。
- **ReplayKit 擴充的記憶體與 CPU 限制**：高解析度加高位元率時可能被系統終止。預設先用 1080p 等比縮放。
- **Media Foundation 硬體編碼器的低延遲設定各家 GPU 不一致**：Intel、NVIDIA、AMD 要分別測試。必要時再接 NVENC / AMF SDK。

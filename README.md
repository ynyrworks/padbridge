# PadBridge

開源的 iPad ⇄ Windows 畫面共享工具，USB 有線與 Wi‑Fi 無線都能用。

> **English:** PadBridge is open-source iPad ⇄ Windows screen sharing over USB or Wi‑Fi: use the iPad as an extended or mirrored Windows display with touch and Apple Pencil, or show the iPad's screen in a Windows window. Every connection is paired and encrypted. The Windows app follows the Windows display language (Chinese or English) and can be switched in the app. See [Code signing policy](#code-signing-policy) and [Privacy](#privacy).

- **延伸螢幕（Display）**：iPad 當 Windows 的延伸或鏡像螢幕，支援觸控、Apple Pencil（壓力、傾斜、懸停）與巧控鍵盤觸控板。
- **鏡像（Mirror）**：iPad 畫面即時顯示在 Windows 視窗中。

兩個方向都已在實機（iPad + Windows 11）上以 USB 與 Wi‑Fi 驗證，鏡像可達 60 fps。架構與協定見 [docs/DESIGN.md](docs/DESIGN.md)。

![PadBridge 桌面 App](docs/app-mirror.png)

## 功能

- **自動找到 iPad**：USB 接上的 iPad 透過 usbmux 出現；同一網路上的 iPad 透過 Bonjour 出現。
- **真正的延伸螢幕**：沒有虛擬螢幕時，App 會提供「安裝虛擬螢幕」按鈕（開源的 [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)），並在連線時自動把虛擬螢幕調成 iPad 的解析度。
- **硬體編解碼**：延伸螢幕用 GPU 縮放加 Media Foundation 硬體 H.264 編碼；鏡像用硬體 H.264 解碼，GPU 同時完成裁切、旋轉與色彩轉換。
- **選擇 GPU**：同時有內顯與獨顯的電腦，可以分別指定編碼和解碼要用哪張顯示卡，或改用 CPU。
- **低延遲**：只在畫面變動時編碼；游標由 iPad 自己繪製；接收端跟不上時只顯示最新一格，延遲不會累積。
- **狀態顯示**：鏡像視窗顯示「顯示 fps／收到 fps／流量／解碼時間」，方便判斷瓶頸。
- **配對與加密**：每條連線（USB 或 Wi‑Fi）都會先驗證雙方身分並加密；第一次連線時，兩邊會顯示同一組 6 位數字讓你確認配對。
- **自動重新連線**：拔掉 USB、換 Wi‑Fi、電腦睡眠、iPad App 切到背景或停止廣播時，會自動重試，恢復後繼續。
- **中文 / English**：介面跟隨 Windows 語言，也可以在 App 中切換。
- **設定會記住**：選的螢幕、編解碼方式、畫質與語言，下次開啟時沿用。

## 安裝

### Windows

1. 安裝 **iTunes**（Apple 官網的桌面版）或 Microsoft Store 的「**Apple 裝置**」App。USB 連線需要它提供的 Apple Mobile Device Service。
2. 從 [Releases](https://github.com/ynyrworks/padbridge/releases/latest) 下載 `PadBridge-…-setup.exe` 安裝（不需要系統管理員權限），或下載 `PadBridge-…-windows-x64.zip` 解壓後直接執行 `PadBridge.exe`。兩者都包含命令列版 `padbridge-cli.exe` 與授權檔。想試最新開發版，可以從 [Actions](https://github.com/ynyrworks/padbridge/actions) 最新一次成功建置下載 `PadBridge-windows`。
3. 第一次執行時，Windows 防火牆可能會詢問，請允許「私人網路」（Bonjour 需要）。

### iPad（不需要 Mac）

1. 從 [Releases](https://github.com/ynyrworks/padbridge/releases/latest) 下載 `PadBridge-…-unsigned.ipa`（開發版則是 Actions 的 `PadBridge-unsigned-ipa`）。
2. 在 Windows 用 [Sideloadly](https://sideloadly.io/) 搭配自己的 Apple ID 簽章，透過 USB 安裝到 iPad。免費 Apple ID 每 7 天要重簽一次。
3. 在 iPad 上：
   - 「設定 → 一般 → VPN 與裝置管理」信任你的開發者描述檔；
   - 「設定 → 隱私權與安全性 → 開發者模式」開啟；
   - 打開 PadBridge，允許「區域網路」權限。

## 使用

### 第一次連線：配對

第一次從某台電腦連到某台 iPad 時，Windows 和 iPad 都會跳出配對確認，顯示同一組 6 位數字。數字相同才在兩邊按「配對」；不同代表可能有人冒充，請取消。配對之後就不再詢問，連線全程加密。

- 已配對的裝置列在 Windows App 左側「已配對的 iPad」與 iPad App 的「已配對的電腦」，都可以取消配對。
- 鏡像用的廣播擴充沒有畫面可以按按鈕，所以電腦第一次要求鏡像時，**廣播會暫停並在 iPad 上顯示確認碼**；與電腦上的數字相同的話，在 60 秒內重新開始廣播就等於允許，Windows 會自動重試並連上。
- 有 App Group 的安裝（付費開發者帳號）會直接沿用 App 裡的配對；用免費 Apple ID 透過 Sideloadly 安裝時沒有 App Group，所以鏡像要照上面方式另外確認一次。

### 延伸螢幕（Windows → iPad）

1. iPad 打開 PadBridge App，保持在前景。
2. Windows 的 PadBridge 選擇「傳送到 iPad 的螢幕」，按該 iPad 的「🖥 延伸螢幕」。
3. 直接在 iPad 上操作：手指是觸控，Apple Pencil 是筆，觸控板是滑鼠。可取消勾選「允許 iPad 操作」。

如果選的是實體螢幕，iPad 會鏡像那個螢幕。要多一個真正的延伸螢幕：

1. 按左側的「⬇ 安裝虛擬螢幕」。PadBridge 會用 winget 下載 Virtual Display Driver 的控制程式並開啟。
2. 在控制程式按 **Install Driver**，允許系統管理員權限。驅動由上游作者簽章與安裝；公司管理的電腦可能不允許安裝驅動。
3. 回到 PadBridge 按 🔄，選擇標示「虛擬」的螢幕。連線時 PadBridge 會把它調成 iPad 的解析度。

預設「虛擬螢幕跟著 PadBridge 開關」：開啟 PadBridge 時打開虛擬螢幕，關閉時把它從桌面移除，視窗會回到實體螢幕。想讓虛擬螢幕一直開著，取消勾選即可。

以系統管理員身分執行的視窗（例如工作管理員）收不到 iPad 的觸控，這是 Windows 的安全限制。

### 鏡像（iPad → Windows）

1. 在 iPad 的 PadBridge App 點「開始 / 停止廣播」，或從控制中心長按「螢幕錄影」，選 **PadBridge Mirror** 開始廣播。畫面不會被錄成檔案，只會即時傳送。
2. Windows 的 PadBridge 按「📺 鏡像」。

### 編碼 / 解碼

左側「編碼 / 解碼」可以分別選擇：

| 選項 | 延伸螢幕編碼 | 鏡像解碼 |
|---|---|---|
| 自動（預設） | 用繪製該螢幕的 GPU，不需額外複製 | 硬體解碼，獨顯優先 |
| 指定 GPU | 若不是繪製該螢幕的 GPU，每格會經過系統記憶體複製一次 | 用該 GPU 硬體解碼 |
| CPU | OpenH264 軟體編碼 | OpenH264 軟體解碼 |

硬體無法使用時會自動改用 CPU，原因會寫在事件紀錄。

同一區還可以設定延伸螢幕的畫質（最大寬度、位元率、最高 fps）與介面語言。

### 連線中斷時

連線意外中斷時（拔掉 USB、Wi‑Fi 斷線、電腦睡眠、iPad App 切到背景、停止廣播），App 會每隔 2～10 秒自動重試，USB 重新插上後也找得到同一台 iPad。按「關閉」取消重試。

**USB ⇄ Wi‑Fi 自動切換**：用 USB 連著時拔掉線，如果同一台 iPad 也在同一個 Wi‑Fi 上，約 0.3 秒後改由 Wi‑Fi 接手；之後再插上 USB，會自動切回 USB。iPad 在 Bonjour 廣播中附上身分指紋，Windows 藉此認出兩條路是同一台；連線時仍以加密握手驗證身分。切回 USB 需要這台 iPad 在本次執行中曾經用 USB 連過。

### 效能參考

| 項目 | 硬體 | 軟體（CPU） |
|---|---|---|
| 延伸螢幕：4K / 1440p → 1080p 編碼 | 約 2 ms／格（RTX 5080） | 4K 約 18 fps，1440p 約 38 fps |
| 鏡像：1334×1920 解碼＋旋轉＋轉色 | 約 3.8 ms／格（RTX 5080） | 約 12 ms／格（解碼 8.4 ms＋轉換 3.4 ms） |

實機鏡像（RTX 3050 Laptop，軟體解碼）：iPad 送 60 fps，Windows 顯示約 60 fps。

## 命令列

```bash
padbridge-cli devices
```

```bash
padbridge-cli ping usb
```

```bash
padbridge-cli monitors
```

```bash
padbridge-cli gpus
```

```bash
padbridge-cli display usb 1
```

```bash
padbridge-cli mirror usb
```

```bash
padbridge-cli paired
```

```bash
padbridge-cli unpair <金鑰開頭或名稱>
```

`<目標>` 可以是 `usb`、`usb:<UDID>` 或 iPad 的 IP。`PadBridge.exe --mirror usb` / `--display usb` 會在啟動後立刻開始，適合做成捷徑。命令列的語言跟隨 Windows，可用環境變數 `PADBRIDGE_LANG=en` 或 `zh` 指定。

## 開發

需要 Rust（stable）與 Windows 10 1809+ / Windows 11。

```bash
cargo test
```

```bash
cargo run --release -p padbridge-app
```

需要 GPU 的測試預設略過，可以這樣執行：

```bash
cargo test --release -p padbridge-host -- --ignored hardware run_mirror_hardware upload_then_encode bench_hardware --nocapture
```

沒有 iPad 也能測試：

```bash
cargo run --release -p padbridge-host --example fake_display
```

```bash
cargo run -p padbridge-host --example fake_mirror
```

iOS 端由 GitHub Actions 的 `ios` job 以 XcodeGen 產生專案、跑 Swift 測試並產出未簽章 IPA。有 Mac 的話，可以在 `ios/` 執行 `xcodegen generate` 後用 Xcode 開啟。

## 已知限制

- 免費 Apple ID 簽的 App 7 天後失效，需要重簽。
- 鏡像需要每次手動開始廣播，這是 iOS 的限制。
- iPad 端目前輸出 H.264 Constrained Baseline；HEVC 與 High profile 尚未支援。
- 從原始碼編譯的 OpenH264 不在 Cisco 的 H.264 專利授權範圍內，詳見 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
- iPad App 的介面目前只有中文。
- 用免費 Apple ID 安裝時，鏡像的配對要另外確認一次（見「第一次連線：配對」）。

## Code signing policy

Windows 版本已申請 [SignPath Foundation](https://signpath.org/) 的免費開源程式碼簽章；核准後，發行版會在 GitHub Actions 由本儲存庫的原始碼自動建置，並在人工核准後簽章。

Windows releases are being set up for free code signing provided by [SignPath.io](https://about.signpath.io/), certificate by [SignPath Foundation](https://signpath.org/). Signed binaries are built by GitHub Actions from this repository's source, and every signing request is approved manually.

| 角色 Role | 成員 Members |
|---|---|
| 作者 Committers and reviewers | [ynyrworks](https://github.com/ynyrworks) |
| 核准者 Approvers | [ynyrworks](https://github.com/ynyrworks) |

iPad App 不經此流程，由使用者以自己的 Apple ID 簽章安裝。The iPad app is not part of this; users sign it with their own Apple ID.

## Privacy

除非使用者明確要求，PadBridge 不會把任何資訊傳送到其他網路系統。This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it.

- 只連線到使用者選擇的 iPad；畫面、觸控與配對資料只在這兩台裝置之間傳送，且全程加密。
- 以 Bonjour 在區域網路上尋找 iPad，不會連到網際網路上的伺服器；沒有遙測、沒有帳號。
- 只有在使用者按下「安裝虛擬螢幕」時，才會透過 winget 從 GitHub 下載 Virtual Display Driver 的控制程式。
- 配對金鑰與設定只存在本機：Windows 在 `%APPDATA%\PadBridge`（金鑰以 DPAPI 加密），iPad 在 App 的 App Group 容器。

## 授權

PadBridge 以 [MIT](LICENSE) 授權。第三方元件（OpenH264、egui 字型、Rust crates、Virtual Display Driver 等）的授權與來源見 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)，全文在 [`licenses/`](licenses/)。

## 致謝

- [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)：Windows 虛擬螢幕。
- [OpenH264](https://github.com/cisco/openh264) 與 [openh264-rs](https://github.com/ralfbiedert/openh264-rs)：軟體 H.264。
- [egui / eframe](https://github.com/emilk/egui)：Windows App 介面。
- [libimobiledevice](https://libimobiledevice.org/) 與 [peertalk](https://github.com/rsms/peertalk)：usbmux 協定的參考。

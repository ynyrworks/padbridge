# PadBridge

開源的 iPad ⇄ Windows 畫面共享工具，USB 有線與 Wi‑Fi 無線皆可。

- **Display 模式**：iPad 當 Windows 的擴充螢幕，支援觸控與 Apple Pencil。
- **Mirror 模式**：iPad 畫面顯示在 Windows 視窗中。

> 🚧 開發中：已完成線路協定（M0）、USB usbmux 通道（M1）、iOS 骨架（M2）、Mirror 模式（M3）、Display 模式（M4）。下一步是觸控回傳（M5）。架構與里程碑見 [docs/DESIGN.md](docs/DESIGN.md)。

## Windows App

![PadBridge 桌面 App 鏡像畫面](docs/app-mirror.png)

`PadBridge.exe`（`crates/app`）提供：

- **裝置清單**：USB 接上的 iPad 會自動出現（透過 usbmux）；同一個網路上開著 PadBridge App 或正在廣播的 iPad，會透過 Bonjour 自動發現。
- **鏡像**：在 App 內顯示 iPad 畫面，並顯示解析度與 fps。
- **測試連線**：量測來回延遲。
- **手動連線**：輸入 IP 或 `usb`。
- `PadBridge.exe --mirror usb`：啟動後立刻開始鏡像，適合做成捷徑。

```bash
cargo run --release -p padbridge-app
```

GitHub Actions 每次建置都會產出 `PadBridge-windows` artifact，內含 `PadBridge.exe` 和 `padbridge-cli.exe`（命令列版）。第一次執行時，Windows 防火牆可能會詢問是否允許區域網路存取（Bonjour 需要）。

## 需求（規劃中）

- Windows 10 1809+ / Windows 11
- USB 模式：安裝 Microsoft Store 的「Apple 裝置」App 或 iTunes（提供 Apple Mobile Device Service）
- Display 模式：開源 Virtual Display Driver
- iPadOS 17+

## 開發

```bash
cargo test
```

iOS 端（不需要 Mac）：

1. 推送到 GitHub 後，Actions 的 `ios` job 會跑 Swift 測試，並產出 `PadBridge-unsigned-ipa` artifact。
2. 在 Windows 上用 [Sideloadly](https://sideloadly.io/) 搭配 Apple ID 簽章，透過 USB 安裝到 iPad（免費帳號每 7 天要重簽一次）。
3. 開啟 App 並允許「區域網路」權限，再從 Windows 測試連線：

```bash
cargo run -p padbridge-cli -- devices
```

```bash
cargo run -p padbridge-cli -- ping usb
```

```bash
cargo run -p padbridge-cli -- ping <iPad 的 IP>
```

`devices` 會列出透過 USB 連接的 iPad，`watch` 會持續顯示插拔事件。

### Display 模式（Windows → iPad）

1. 在 iPad 打開 PadBridge App，畫面保持在前景。
2. 在 Windows 的 PadBridge App 選擇「傳送到 iPad 的螢幕」，再按該 iPad 的「🖥 延伸螢幕」。
3. iPad 會全螢幕顯示該螢幕的畫面，滑鼠游標也會畫上去。

命令列版本：

```bash
cargo run --release -p padbridge-cli -- monitors
```

```bash
cargo run --release -p padbridge-cli -- display usb 1
```

**真正的「延伸」螢幕**需要虛擬顯示器驅動，例如開源的 [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)。裝好後 Windows 會多出一個螢幕，在 PadBridge 選它即可。沒裝的話，PadBridge 會鏡像你選的既有螢幕。

沒有 iPad 也能測試：

```bash
cargo run --release -p padbridge-host --example fake_display
```

```bash
cargo run --release -p padbridge-cli -- display 127.0.0.1
```

目前用 CPU 縮放加 OpenH264 軟體編碼。在 RTX 5080 這台電腦上，1440p 螢幕約 38 fps，4K 螢幕約 18 fps（縮成 1920×1080 後傳送）。之後會改用 GPU 縮放和硬體編碼（Media Foundation / NVENC）。

### Mirror 模式（iPad → Windows）

在 iPad 的 PadBridge App 按廣播按鈕、選「PadBridge Mirror」開始廣播，然後在 Windows 執行：

```bash
cargo run --release -p padbridge-cli -- mirror usb
```

（或 `mirror <iPad 的 IP>`）。按 Esc 或關閉視窗結束。

沒有 iPad 也能測試檢視器：先執行假的鏡像來源，再連到本機：

```bash
cargo run -p padbridge-host --example fake_mirror
```

```bash
cargo run --release -p padbridge-cli -- mirror 127.0.0.1
```

目前檢視器以 OpenH264 軟體解碼，所以 iPad 端使用 H.264 Constrained Baseline，最大 1920 px。之後會換成 Media Foundation 硬體解碼，並支援 High profile 與 HEVC。

有 Mac 的話，可以在 `ios/` 執行 `xcodegen generate`，再用 Xcode 開啟。

## 授權

[MIT](LICENSE)

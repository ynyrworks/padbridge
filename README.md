# PadBridge

開源的 iPad ⇄ Windows 畫面共享工具，USB 有線與 Wi‑Fi 無線皆可。

- **Display 模式**：iPad 當 Windows 的擴充螢幕，支援觸控與 Apple Pencil。
- **Mirror 模式**：iPad 畫面顯示在 Windows 視窗中。

> 🚧 開發中：已完成線路協定（M0）、USB usbmux 通道（M1）、iOS 骨架（M2）與 Mirror 模式（M3）。架構與里程碑見 [docs/DESIGN.md](docs/DESIGN.md)。

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

### Mirror 模式（iPad → Windows）

在 iPad 的 PadBridge App 按廣播按鈕、選「PadBridge Mirror」開始廣播，然後在 Windows 執行：

```bash
cargo run --release -p padbridge-cli -- mirror usb
```

（或 `mirror <iPad 的 IP>`）。按 Esc 或關閉視窗結束。

沒有 iPad 也能測試檢視器：先執行假的鏡像來源，再連到本機：

```bash
cargo run -p padbridge-cli --example fake_mirror
```

```bash
cargo run --release -p padbridge-cli -- mirror 127.0.0.1
```

目前檢視器以 OpenH264 軟體解碼，所以 iPad 端使用 H.264 Constrained Baseline，最大 1920 px。之後會換成 Media Foundation 硬體解碼，並支援 High profile 與 HEVC。

有 Mac 的話，可以在 `ios/` 執行 `xcodegen generate`，再用 Xcode 開啟。

## 授權

[MIT](LICENSE)

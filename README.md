# PadBridge

開源的 iPad ⇄ Windows 畫面共享工具，USB 有線與 Wi‑Fi 無線皆可。

- **Display 模式**：iPad 當 Windows 的擴充螢幕，支援觸控與 Apple Pencil。
- **Mirror 模式**：iPad 畫面顯示在 Windows 視窗中。

> 🚧 開發中，目前只有線路協定（M0）。架構與里程碑見 [docs/DESIGN.md](docs/DESIGN.md)。

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
cargo run -p padbridge-cli -- ping <iPad 的 IP>
```

有 Mac 的話，可以在 `ios/` 執行 `xcodegen generate`，再用 Xcode 開啟。

## 授權

[MIT](LICENSE)

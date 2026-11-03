# 第三方授權與來源

PadBridge 本身以 [MIT](LICENSE) 授權。下列第三方元件有的會編進發行的程式，有的只在執行時使用，或只當作參考。各授權全文放在 [`licenses/`](licenses/)。

## 編進 Windows 程式（PadBridge.exe、padbridge-cli.exe）

### OpenH264（Cisco Systems）

- 用途：軟體 H.264 編碼與解碼（沒有硬體編解碼器，或選擇「CPU」時使用）。
- 來源：<https://github.com/cisco/openh264>，經由 Rust crate [`openh264`](https://github.com/ralfbiedert/openh264-rs) 從**原始碼編譯**並靜態連結。
- 授權：BSD-2-Clause，Copyright (c) 2013, Cisco Systems。全文見 [`licenses/OpenH264-BSD-2-Clause.txt`](licenses/OpenH264-BSD-2-Clause.txt)。
- **H.264 專利**：Cisco 只替「從 Cisco 官方下載的 OpenH264 二進位檔」支付 MPEG LA 的 H.264 專利授權費。PadBridge 是從原始碼編譯，**不在這個授權範圍內**。散布 PadBridge 的二進位檔時，請自行評估所在地區的專利授權問題。硬體編解碼（Media Foundation）使用 Windows 和顯示卡驅動本身提供的功能。

### egui 內建字型（`epaint_default_fonts`）

PadBridge.exe 內含 egui 的預設字型：

| 字型 | 授權 | 全文 |
|---|---|---|
| Ubuntu Light | Ubuntu Font Licence 1.0 | [`licenses/fonts/Ubuntu-Font-License-1.0.txt`](licenses/fonts/Ubuntu-Font-License-1.0.txt) |
| Hack Regular | MIT（Hack）與 Bitstream Vera 授權（DejaVu 部分屬公有領域） | [`licenses/fonts/Hack-Regular.txt`](licenses/fonts/Hack-Regular.txt) |
| Noto Emoji | SIL Open Font License 1.1 | [`licenses/fonts/OFL-1.1-NotoEmoji.txt`](licenses/fonts/OFL-1.1-NotoEmoji.txt) |
| emoji-icon-font | MIT | [`licenses/fonts/emoji-icon-font-MIT.txt`](licenses/fonts/emoji-icon-font-MIT.txt) |

中文字型（微軟正黑體 `msjh.ttc`）是在執行時從 Windows 讀取，不隨 PadBridge 散布。

### Rust crates

下表是 Windows 程式在 `x86_64-pc-windows-msvc` 上實際編譯進去的所有 crate（由 `cargo tree -e normal,build` 產生）。有多個授權可選的 crate，PadBridge 選用其中的 MIT 或 Apache-2.0。各 crate 的著作權聲明可在其來源儲存庫中找到。

授權全文：
[MIT](LICENSE)（條款相同，著作權人依各 crate）·
[Apache-2.0](licenses/Apache-2.0.txt) ·
[Unicode-3.0](licenses/Unicode-3.0.txt) ·
[BSL-1.0](licenses/BSL-1.0-error-code.txt) ·
[Zlib](licenses/Zlib-foldhash.txt) ·
[ISC](licenses/ISC-libloading.txt) ·
[BSD-2-Clause](licenses/OpenH264-BSD-2-Clause.txt)（條款相同，著作權人依各 crate）。
`hexf-parse` 為 CC0-1.0（公有領域貢獻），不需要附上聲明。

| Crate | 版本 | 授權 | 來源 |
|---|---|---|---|
| accesskit | 0.24.1 | MIT OR Apache-2.0 | <https://github.com/AccessKit/accesskit> |
| accesskit_consumer | 0.35.0 | MIT OR Apache-2.0 | <https://github.com/AccessKit/accesskit> |
| accesskit_windows | 0.32.1 | MIT OR Apache-2.0 | <https://github.com/AccessKit/accesskit> |
| accesskit_winit | 0.32.2 | Apache-2.0 | <https://github.com/AccessKit/accesskit> |
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | <https://github.com/oyvindln/adler2> |
| ahash | 0.8.12 | MIT OR Apache-2.0 | <https://github.com/tkaitchuck/ahash> |
| allocator-api2 | 0.2.21 | MIT OR Apache-2.0 | <https://github.com/zakarumych/allocator-api2> |
| arboard | 3.7.0 | MIT OR Apache-2.0 | <https://github.com/1Password/arboard> |
| arrayvec | 0.7.8 | MIT OR Apache-2.0 | <https://github.com/bluss/arrayvec> |
| ash | 0.38.0+1.3.281 | MIT OR Apache-2.0 | <https://github.com/ash-rs/ash> |
| autocfg | 1.5.1 | Apache-2.0 OR MIT | <https://github.com/cuviper/autocfg> |
| base64 | 0.23.1 | MIT OR Apache-2.0 | <https://github.com/marshallpierce/rust-base64> |
| bit-set | 0.9.1 | Apache-2.0 OR MIT | <https://github.com/contain-rs/bit-set> |
| bit-vec | 0.9.1 | Apache-2.0 OR MIT | <https://github.com/contain-rs/bit-vec> |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | <https://github.com/bitflags/bitflags> |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT | <https://github.com/Lokathor/bytemuck> |
| bytemuck_derive | 1.12.1 | Zlib OR Apache-2.0 OR MIT | <https://github.com/Lokathor/bytemuck> |
| byteorder-lite | 0.1.0 | Unlicense OR MIT | <https://github.com/image-rs/byteorder-lite> |
| cc | 1.11.0 | MIT OR Apache-2.0 | <https://github.com/rust-lang/cc-rs> |
| cfg_aliases | 0.2.2 | MIT | <https://github.com/katharostech/cfg_aliases> |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | <https://github.com/rust-lang/cfg-if> |
| clipboard-win | 5.4.3 | BSL-1.0 | <https://github.com/DoumanAsh/clipboard-win> |
| codespan-reporting | 0.13.1 | Apache-2.0 | <https://github.com/brendanzab/codespan> |
| color | 0.3.3 | Apache-2.0 OR MIT | <https://github.com/linebender/color> |
| crc32fast | 1.5.4 | MIT OR Apache-2.0 | <https://github.com/srijs/rust-crc32fast> |
| cursor-icon | 1.2.0 | MIT OR Apache-2.0 OR Zlib | <https://github.com/rust-windowing/cursor-icon> |
| deranged | 0.5.9 | MIT OR Apache-2.0 | <https://github.com/jhpratt/deranged> |
| displaydoc | 0.2.7 | MIT OR Apache-2.0 | <https://github.com/yaahc/displaydoc> |
| document-features | 0.2.12 | MIT OR Apache-2.0 | <https://github.com/slint-ui/document-features> |
| dpi | 0.1.2 | Apache-2.0 AND MIT | <https://github.com/rust-windowing/winit> |
| ecolor | 0.35.0 | MIT OR Apache-2.0 | <https://github.com/emilk/egui> |
| eframe | 0.35.0 | MIT OR Apache-2.0 | <https://github.com/emilk/egui/tree/main/crates/eframe> |
| egui | 0.35.0 | MIT OR Apache-2.0 | <https://github.com/emilk/egui> |
| egui-wgpu | 0.35.0 | MIT OR Apache-2.0 | <https://github.com/emilk/egui/tree/main/crates/egui-wgpu> |
| egui-winit | 0.35.0 | MIT OR Apache-2.0 | <https://github.com/emilk/egui/tree/main/crates/egui-winit> |
| either | 1.19.0 | MIT OR Apache-2.0 | <https://github.com/rayon-rs/either> |
| emath | 0.35.0 | MIT OR Apache-2.0 | <https://github.com/emilk/egui/tree/main/crates/emath> |
| epaint | 0.35.0 | MIT OR Apache-2.0 | <https://github.com/emilk/egui/tree/main/crates/epaint> |
| epaint_default_fonts | 0.35.0 | (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 | <https://github.com/emilk/egui/tree/main/crates/epaint_default_fonts> |
| equivalent | 1.0.3 | Apache-2.0 OR MIT | <https://github.com/indexmap-rs/equivalent> |
| error-code | 3.4.0 | BSL-1.0 | <https://github.com/DoumanAsh/error-code> |
| euclid | 0.22.14 | MIT OR Apache-2.0 | <https://github.com/servo/euclid> |
| fastrand | 2.5.0 | Apache-2.0 OR MIT | <https://github.com/smol-rs/fastrand> |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 | <https://github.com/image-rs/fdeflate> |
| fearless_simd | 0.4.1 | Apache-2.0 OR MIT | <https://github.com/linebender/fearless_simd> |
| find-msvc-tools | 0.1.15 | MIT OR Apache-2.0 | <https://github.com/rust-lang/cc-rs> |
| flate2 | 1.1.10 | MIT OR Apache-2.0 | <https://github.com/rust-lang/flate2-rs> |
| flume | 0.12.0 | Apache-2.0/MIT | <https://github.com/zesterer/flume> |
| foldhash | 0.1.5 | Zlib | <https://github.com/orlp/foldhash> |
| foldhash | 0.2.0 | Zlib | <https://github.com/orlp/foldhash> |
| font-types | 0.11.3 | MIT OR Apache-2.0 | <https://github.com/googlefonts/fontations> |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 | <https://github.com/servo/rust-url> |
| futures-core | 0.3.34 | MIT OR Apache-2.0 | <https://github.com/rust-lang/futures-rs> |
| futures-sink | 0.3.34 | MIT OR Apache-2.0 | <https://github.com/rust-lang/futures-rs> |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | <https://github.com/rust-random/getrandom> |
| gl_generator | 0.14.0 | Apache-2.0 | <https://github.com/brendanzab/gl-rs/> |
| glow | 0.17.0 | MIT OR Apache-2.0 OR Zlib | <https://github.com/grovesNL/glow> |
| glutin_wgl_sys | 0.6.1 | Apache-2.0 | <https://github.com/rust-windowing/glutin> |
| gpu-allocator | 0.28.0 | MIT OR Apache-2.0 | <https://github.com/Traverse-Research/gpu-allocator> |
| gpu-descriptor | 0.3.2 | MIT OR Apache-2.0 | <https://github.com/zakarumych/gpu-descriptor> |
| gpu-descriptor-types | 0.2.0 | MIT OR Apache-2.0 | <https://github.com/zakarumych/gpu-descriptor> |
| guillotiere | 0.7.0 | MIT/Apache-2.0 | <https://github.com/nical/guillotiere> |
| half | 2.7.1 | MIT OR Apache-2.0 | <https://github.com/VoidStarKat/half-rs> |
| harfrust | 0.7.0 | MIT | <https://github.com/harfbuzz/harfrust> |
| hashbrown | 0.15.5 | MIT OR Apache-2.0 | <https://github.com/rust-lang/hashbrown> |
| hashbrown | 0.16.1 | MIT OR Apache-2.0 | <https://github.com/rust-lang/hashbrown> |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 | <https://github.com/rust-lang/hashbrown> |
| hexf-parse | 0.2.1 | CC0-1.0 | <https://github.com/lifthrasiir/hexf> |
| icu_collections | 2.4.0 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| icu_locale_core | 2.4.0 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| icu_normalizer | 2.4.0 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| icu_normalizer_data | 2.4.0 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| icu_properties | 2.4.0 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| icu_properties_data | 2.4.0 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| icu_provider | 2.4.0 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| idna | 1.1.0 | MIT OR Apache-2.0 | <https://github.com/servo/rust-url/> |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT | <https://github.com/hsivonen/idna_adapter> |
| if-addrs | 0.15.0 | MIT OR BSD-3-Clause | <https://github.com/messense/if-addrs> |
| image | 0.25.10 | MIT OR Apache-2.0 | <https://github.com/image-rs/image> |
| indexmap | 2.14.2 | Apache-2.0 OR MIT | <https://github.com/indexmap-rs/indexmap> |
| itertools | 0.14.0 | MIT OR Apache-2.0 | <https://github.com/rust-itertools/itertools> |
| jobserver | 0.1.35 | MIT OR Apache-2.0 | <https://github.com/rust-lang/jobserver-rs> |
| khronos_api | 3.1.0 | Apache-2.0 | <https://github.com/brendanzab/gl-rs/> |
| khronos-egl | 6.0.0 | MIT/Apache-2.0 | <https://github.com/timothee-haudebourg/khronos-egl> |
| kurbo | 0.13.1 | Apache-2.0 OR MIT | <https://github.com/linebender/kurbo> |
| libc | 0.2.191 | MIT OR Apache-2.0 | <https://github.com/rust-lang/libc> |
| libloading | 0.8.9 | ISC | <https://github.com/nagisa/rust_libloading/> |
| libm | 0.2.16 | MIT | <https://github.com/rust-lang/compiler-builtins> |
| linebender_resource_handle | 0.1.1 | Apache-2.0 OR MIT | <https://github.com/linebender/raw_resource_handle> |
| litemap | 0.8.3 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| litrs | 1.0.1 | MIT OR Apache-2.0 | <https://github.com/LukasKalbertodt/litrs> |
| lock_api | 0.4.14 | MIT OR Apache-2.0 | <https://github.com/Amanieu/parking_lot> |
| log | 0.4.35 | MIT OR Apache-2.0 | <https://github.com/rust-lang/log> |
| mdns-sd | 0.21.7 | Apache-2.0 OR MIT | <https://github.com/keepsimple1/mdns-sd> |
| memchr | 2.8.3 | Unlicense OR MIT | <https://github.com/BurntSushi/memchr> |
| minifb | 0.29.0 | MIT OR Apache-2.0 | <https://github.com/emoon/rust_minifb> |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 | <https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide> |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 | <https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide> |
| mio | 1.2.4 | MIT | <https://github.com/tokio-rs/mio> |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 | <https://github.com/awxkee/moxcms.git> |
| naga | 29.0.4 | MIT OR Apache-2.0 | <https://github.com/gfx-rs/wgpu> |
| nasm-rs | 0.3.2 | MIT OR Apache-2.0 | <https://github.com/medek/nasm-rs> |
| nohash-hasher | 0.2.0 | Apache-2.0 OR MIT | <https://github.com/paritytech/nohash-hasher> |
| num-conv | 0.2.2 | MIT OR Apache-2.0 | <https://github.com/jhpratt/num-conv> |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | <https://github.com/rust-num/num-traits> |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | <https://github.com/matklad/once_cell> |
| openh264 | 0.9.9 | BSD-2-Clause | <https://github.com/ralfbiedert/openh264-rs> |
| openh264-sys2 | 0.9.9 | BSD-2-Clause | <https://github.com/ralfbiedert/openh264-rs> |
| ordered-float | 5.5.1 | MIT | <https://github.com/reem/rust-ordered-float> |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 | <https://github.com/Amanieu/parking_lot> |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 | <https://github.com/Amanieu/parking_lot> |
| peniko | 0.6.1 | Apache-2.0 OR MIT | <https://github.com/linebender/peniko> |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 | <https://github.com/servo/rust-url/> |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT | <https://github.com/taiki-e/pin-project-lite> |
| plist | 1.10.3 | MIT | <https://github.com/ebarnard/rust-plist/> |
| png | 0.18.1 | MIT OR Apache-2.0 | <https://github.com/image-rs/image-png> |
| pollster | 0.4.0 | Apache-2.0/MIT | <https://github.com/zesterer/pollster> |
| polycool | 0.4.0 | MIT OR Apache-2.0 | <https://github.com/linebender/kurbo> |
| potential_utf | 0.1.6 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| powerfmt | 0.2.1 | MIT OR Apache-2.0 | <https://github.com/jhpratt/powerfmt> |
| presser | 0.3.1 | MIT OR Apache-2.0 | <https://github.com/EmbarkStudios/presser> |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | <https://github.com/dtolnay/proc-macro2> |
| profiling | 1.0.18 | MIT OR Apache-2.0 | <https://github.com/aclysma/profiling> |
| pxfm | 0.1.30 | BSD-3-Clause OR Apache-2.0 | <https://github.com/awxkee/pxfm> |
| quick-xml | 0.42.0 | MIT | <https://github.com/tafia/quick-xml> |
| quote | 1.0.47 | MIT OR Apache-2.0 | <https://github.com/dtolnay/quote> |
| range-alloc | 0.1.5 | MIT OR Apache-2.0 | <https://github.com/gfx-rs/range-alloc> |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib | <https://github.com/rust-windowing/raw-window-handle> |
| read-fonts | 0.39.2 | MIT OR Apache-2.0 | <https://github.com/googlefonts/fontations> |
| renderdoc-sys | 1.1.0 | MIT OR Apache-2.0 | <https://github.com/ebkalderon/renderdoc-rs> |
| rustc-hash | 1.1.0 | Apache-2.0/MIT | <https://github.com/rust-lang-nursery/rustc-hash> |
| rustc-hash | 2.1.3 | Apache-2.0 OR MIT | <https://github.com/rust-lang/rustc-hash> |
| safe_arch | 1.2.0 | Zlib OR Apache-2.0 OR MIT | <https://github.com/Lokathor/safe_arch> |
| same-file | 1.0.6 | Unlicense/MIT | <https://github.com/BurntSushi/same-file> |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 | <https://github.com/bluss/scopeguard> |
| self_cell | 1.3.0 | Apache-2.0 OR GPL-2.0-only | <https://github.com/Voultapher/self_cell> |
| serde | 1.0.229 | MIT OR Apache-2.0 | <https://github.com/serde-rs/serde> |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | <https://github.com/serde-rs/serde> |
| shlex | 2.0.1 | MIT OR Apache-2.0 | <https://github.com/comex/rust-shlex> |
| simd-adler32 | 0.3.10 | MIT | <https://github.com/mcountryman/simd-adler32> |
| skrifa | 0.42.1 | MIT OR Apache-2.0 | <https://github.com/googlefonts/fontations> |
| smallvec | 1.16.4 | MIT OR Apache-2.0 | <https://github.com/servo/rust-smallvec> |
| smol_str | 0.2.2 | MIT OR Apache-2.0 | <https://github.com/rust-analyzer/smol_str> |
| socket-pktinfo | 0.4.1 | MIT | <https://github.com/pixsper/socket-pktinfo> |
| socket2 | 0.6.5 | MIT OR Apache-2.0 | <https://github.com/rust-lang/socket2> |
| spin | 0.9.9 | MIT | <https://github.com/mvdnes/spin-rs.git> |
| spirv | 0.4.0+sdk-1.4.341.0 | Apache-2.0 | <https://github.com/gfx-rs/rspirv> |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 | <https://github.com/storyyeller/stable_deref_trait> |
| static_assertions | 1.1.0 | MIT OR Apache-2.0 | <https://github.com/nvzqz/static-assertions-rs> |
| syn | 2.0.119 | MIT OR Apache-2.0 | <https://github.com/dtolnay/syn> |
| syn | 3.0.9 | MIT OR Apache-2.0 | <https://github.com/dtolnay/syn> |
| synstructure | 0.14.0 | MIT | <https://github.com/mystor/synstructure> |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | <https://github.com/dtolnay/thiserror> |
| thiserror-impl | 2.0.21 | MIT OR Apache-2.0 | <https://github.com/dtolnay/thiserror> |
| time | 0.3.55 | MIT OR Apache-2.0 | <https://github.com/time-rs/time> |
| time-core | 0.1.9 | MIT OR Apache-2.0 | <https://github.com/time-rs/time> |
| tinystr | 0.8.5 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| tracing | 0.1.44 | MIT | <https://github.com/tokio-rs/tracing> |
| tracing-core | 0.1.36 | MIT | <https://github.com/tokio-rs/tracing> |
| type-map | 0.5.1 | MIT/Apache-2.0 | <https://github.com/kardeiz/type-map> |
| unicode-general-category | 1.1.0 | Apache-2.0 | <https://github.com/yeslogic/unicode-general-category> |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 | <https://github.com/dtolnay/unicode-ident> |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 | <https://github.com/unicode-rs/unicode-segmentation> |
| unicode-width | 0.2.3 | MIT OR Apache-2.0 | <https://github.com/unicode-rs/unicode-width> |
| url | 2.5.8 | MIT OR Apache-2.0 | <https://github.com/servo/rust-url> |
| utf_types | 1.0.0 | Apache-2.0 OR MIT | <https://github.com/hsivonen/utf_types> |
| utf8_iter | 1.1.0 | Apache-2.0 OR MIT | <https://github.com/hsivonen/utf8_iter> |
| uuid | 1.29.0 | Apache-2.0 OR MIT | <https://github.com/uuid-rs/uuid> |
| vello_common | 0.0.9 | Apache-2.0 OR MIT | <https://github.com/linebender/vello> |
| vello_cpu | 0.0.9 | Apache-2.0 OR MIT | <https://github.com/linebender/vello> |
| version_check | 0.9.5 | MIT/Apache-2.0 | <https://github.com/SergioBenitez/version_check> |
| walkdir | 2.5.0 | Unlicense/MIT | <https://github.com/BurntSushi/walkdir> |
| web-time | 1.1.0 | MIT OR Apache-2.0 | <https://github.com/daxpedda/web-time> |
| webbrowser | 1.2.6 | MIT OR Apache-2.0 | <https://github.com/amodm/webbrowser-rs> |
| wgpu | 29.0.4 | MIT OR Apache-2.0 | <https://github.com/gfx-rs/wgpu> |
| wgpu-core | 29.0.4 | MIT OR Apache-2.0 | <https://github.com/gfx-rs/wgpu> |
| wgpu-core-deps-windows-linux-android | 29.0.4 | MIT OR Apache-2.0 | <https://github.com/gfx-rs/wgpu> |
| wgpu-hal | 29.0.4 | MIT OR Apache-2.0 | <https://github.com/gfx-rs/wgpu> |
| wgpu-naga-bridge | 29.0.4 | MIT OR Apache-2.0 | <https://github.com/gfx-rs/wgpu> |
| wgpu-types | 29.0.4 | MIT OR Apache-2.0 | <https://github.com/gfx-rs/wgpu> |
| wide | 1.7.1 | Zlib OR Apache-2.0 OR MIT | <https://github.com/Lokathor/wide> |
| winapi | 0.3.9 | MIT/Apache-2.0 | <https://github.com/retep998/winapi-rs> |
| winapi-util | 0.1.11 | Unlicense OR MIT | <https://github.com/BurntSushi/winapi-util> |
| windows | 0.62.2 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows_x86_64_msvc | 0.52.6 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-collections | 0.3.2 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-core | 0.62.2 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-future | 0.3.2 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-numerics | 0.3.1 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-result | 0.4.1 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-sys | 0.52.0 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| windows-threading | 0.2.1 | MIT OR Apache-2.0 | <https://github.com/microsoft/windows-rs> |
| winit | 0.30.13 | Apache-2.0 | <https://github.com/rust-windowing/winit> |
| writeable | 0.6.4 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| xml-rs | 0.8.29 | MIT | <https://github.com/kornelski/xml-rs> |
| yoke | 0.8.4 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| yoke-derive | 0.8.4 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| zerocopy | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT | <https://github.com/google/zerocopy> |
| zerocopy-derive | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT | <https://github.com/google/zerocopy> |
| zerofrom | 0.1.8 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| zerofrom-derive | 0.1.8 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| zerotrie | 0.2.5 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| zerovec | 0.11.9 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |
| zerovec-derive | 0.11.7 | Unicode-3.0 | <https://github.com/unicode-org/icu4x> |

## 執行時使用，但不隨 PadBridge 散布

| 元件 | 授權 | 說明 |
|---|---|---|
| [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) | MIT | 讓 iPad 成為真正的延伸螢幕。PadBridge 只在使用者按下「安裝虛擬螢幕」時，透過 winget 下載它的官方控制程式；驅動由上游作者簽章與安裝。PadBridge 透過它的設定檔與 named pipe 控制解析度與螢幕數量。 |
| Apple Mobile Device Service | Apple 專有 | 隨 iTunes 或「Apple 裝置」App 安裝，提供 USB 用的 usbmuxd。 |
| Windows Media Foundation、Direct3D 11、DXGI | 作業系統元件 | 硬體編解碼、螢幕擷取、色彩轉換。 |
| iPadOS 架構（Network、VideoToolbox、ReplayKit、AVFoundation） | 作業系統元件 | iPad App 只使用系統架構，沒有第三方相依。 |

## 僅供參考，未使用其程式碼

- [libimobiledevice / usbmuxd](https://github.com/libimobiledevice/usbmuxd)（LGPL-2.1）：usbmux 協定格式的參考。PadBridge 的 `crates/usbmux` 是獨立實作，沒有連結或複製其程式碼。
- [peertalk](https://github.com/rsms/peertalk)（MIT）：透過 usbmux 讓 iOS 當伺服器的做法參考。

## 開發與發布工具（不包含在產品中）

- [XcodeGen](https://github.com/yonaskolb/XcodeGen)（MIT）：在 CI 產生 Xcode 專案。
- [Sideloadly](https://sideloadly.io/)：使用者自行用來簽章並安裝 IPA 的第三方工具，與本專案無關聯。
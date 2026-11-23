---
title: 建置
layout: default
parent: 首頁
nav_order: 3
---

# 從原始碼建置
{: .no_toc }

## 目錄
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## 需求 {#requirements}

- 透過 [rustup](https://rustup.rs/) 安裝的 **Rust**（stable，1.90 以上）
- **Visual Studio 2022 以上**（或其 Build Tools），並安裝「使用 C++ 的桌面開發」工作負載，因為 C 程式庫（BTstack、各編解碼器）需以 MSVC 編譯
- 建置 UI 所需的 **Node.js** 22 與 **pnpm**（執行 `corepack enable` 即可取得 pnpm）
- **Git**（用於子模組）
- 供 `build.ps1` / `build.bat` 使用的 **PowerShell 7**（`pwsh`，`winget install Microsoft.PowerShell`）
- 供檢查（`tools/check_lang.py`）與 [模擬器測試](#emu) 使用的 **Python 3.11 以上**
- 建置 ARM64 版本時：Visual Studio 元件「MSVC v143 - VS 2022 C++ ARM64 build tools」；在 x64 電腦上還需要「適用於 Windows 的 C++ Clang 編譯器」（rustls 的 `ring` 以 clang 組譯 ARM64 程式碼；`installer/build-release.ps1` 會自動找到 Visual Studio 的 clang），以及 `rustup target add aarch64-pc-windows-msvc`

## 快速建置 {#quick-build}

```bash
git clone --recursive https://github.com/SeiyaFunaokaJP/BlueCaster.git
cd BlueCaster
pwsh ./build.ps1
```

也可以在檔案總管中按兩下 `build.bat`：它會執行 `build.ps1`，並在結束時等待按鍵（`build.bat -Test` 會把參數一併傳入）。`build.ps1` 會先檢查 Rust、Node.js、pnpm 與子模組是否齊全，`node_modules` 不存在時執行 `pnpm install`，接著建置視窗的頁面，再把三個程式輸出到 `target/release/`：`bluecaster-core.exe`（常駐核心：藍牙、音訊、系統匣）、`bluecaster.exe`（視窗），以及 `bluecaster-driver.exe`（以提高的權限指派 / 還原 WinUSB 驅動程式的輔助程式）。請將這三個程式放在同一個資料夾；`bluecaster-core.exe --cli` 可以不開視窗直接串流。

參數：`-Debug`（dev 設定檔，輸出到 `target/debug/`）、`-Test`（同時執行「測試」中的檢查）、`-Decoder`（同時建置解碼工具）、`-Emu`（同時執行 [模擬器測試](#emu)；`-Emu -Codecs sbc,ldac` 只測試這些編解碼器）、`-Release -Arch x64|arm64`（只執行發行指令碼，請參閱「發行套件」）。

**逐步建置**（也就是 `build.ps1` 執行的步驟；`bluecaster.exe` 會內嵌視窗的頁面，因此必須先建置頁面）：

```bash
cargo build --release -p bluecaster-core-exe -p bluecaster-driver
cd app/src/ui
pnpm install --frozen-lockfile
pnpm build
cd ../../..
cargo build --release -p bluecaster-ui
```

## 若複製時未使用 `--recursive` {#if-you-already-cloned-without---recursive}

```bash
git submodule update --init --recursive
```

拉取更新了程式庫的提交後，也請再執行一次。如果 `extern/` 下的程式庫不在 `extern/versions.txt` 所列的提交上，建置會停止，錯誤訊息會說明如何仍以其他版本建置。

## 專案結構 {#project-structure}

```
BlueCaster/
├── app/
│   ├── src/
│   │   ├── core/           bluecaster-core.exe: resident core, tray, IPC server, --cli
│   │   ├── ui/             bluecaster.exe: Tauri 2 + SolidJS (pnpm dev runs it in a browser with a mock core)
│   │   ├── driver/         bluecaster-driver.exe: WinUSB assign / restore (elevated)
│   │   └── crates/         Libraries: btstack-sys, hci-transport, bt-host, codec, the codec -sys crates,
│   │                       pipeline, audio-capture, win-sys, core (bluecaster-core), ipc, le-audio, lc3-sys,
│   │                       aptx-adaptive (aptX Adaptive encoder)
│   ├── lang/               Localization (en.json is the source; tools/check_lang.py checks the others)
│   └── resources/          Icon and manifest
├── Cargo.toml              Cargo workspace
├── installer/              Release script, Inno Setup script, updater notes (installer/README.md)
├── compat/                 MSVC compatibility headers for the AOSP code
├── docs/                   Documentation (this site)
├── extern/                 Third-party C libraries (git submodules)
│   ├── btstack/            BTstack Bluetooth stack
│   ├── libldac/            AOSP LDAC encoder
│   ├── libopenaptx/        aptX / aptX HD / aptX LL encoder
│   ├── fdk-aac/            Fraunhofer AAC encoder
│   ├── lhdcv5-enc/         LHDC V5 encoder
│   └── opus/               libopus (Opus encoder)
└── tools/
    ├── a2dp_decode/        bluecaster_decode: checks / decodes the media stream in an HCI capture (.pklg)
    ├── emu/                End-to-end test against a virtual Bluetooth sink (Python)
    └── linux_sink/         bluecaster_sink: measuring A2DP receiver on Linux (Python, Bumble)
```

## 測試 {#tests}

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd app/src/ui && pnpm exec tsc --noEmit && pnpm exec vitest run
python tools/check_lang.py
```

## 發行套件 {#release-packages}

```powershell
pwsh installer/build-release.ps1 -Arch x64
```

這個指令碼會把安裝程式（`BlueCaster-<version>-win64-setup.exe`）、免安裝版 zip、符號 zip（`.pdb` 檔案，供分析當機傾印使用）與更新程式資訊清單輸出到 `target/dist`。需要 Inno Setup 7.1.0（`winget install JRSoftware.InnoSetup.7 --version 7.1.0`）。應用程式內更新的簽署與其他細節，請參閱 `installer/README.md`。

## 不使用硬體進行測試 (tools/emu) {#emu}

`tools/emu` 會在同一台電腦上，把每種編解碼器從 BlueCaster 串流到虛擬藍牙接收端，不需要介面卡或耳機。它只能在 Windows 上執行，需要 **Python 3.11 以上**，以及建置解碼工具用的 **CMake**（隨附於 Visual Studio）。

```bash
python tools/emu/setup_env.py                       # once: creates tools/emu/.venv with pinned packages
cmake -S tools/a2dp_decode -B build/a2dp_decode     # once: the decoder tool
cmake --build build/a2dp_decode --config Release
cargo build --release -p bluecaster-core-exe
python tools/emu/run_test.py                        # every codec and negotiation case
```

運作方式：

- `emu_sink.py` 會在同一處理程序內的連結上執行兩個 [Bumble](https://github.com/google/bumble) 虛擬控制器。BlueCaster 透過 H4 over TCP（`127.0.0.1`）連線到其中一個。另一個由 Bumble 的 A2DP 接收端使用，提供 SBC（支援 SBC XQ）、AAC、aptX、aptX HD、aptX LL、LDAC、LHDC V5、FastStream、Opus、Opus 05 與 aptX Adaptive，並記錄自己的 HCI 通訊。
- `run_test.py` 會以開發用選項，對每種編解碼器執行 `bluecaster-core.exe --cli`：`--hci-tcp`（以虛擬控制器取代 WinUSB）、`--test-tone`（以左聲道 1 kHz / 右聲道 1.5 kHz 取代系統音訊）、`--duration`、`--hci-capture`。環境變數 `BLUECASTER_CONFIG_DIR` 讓 BlueCaster 改用另一個設定資料夾，因此與虛擬接收端配對時，絕不會動到您實際的連結金鑰。
- 接著以 `bluecaster_decode` 檢查兩份 HCI 擷取（接收端那份加上 `--received`）。

符合下列所有條件時，該編解碼器即通過測試：

- 協商出所要求的編解碼器
- `bluecaster_decode` 在傳送端與接收端都沒有發現問題
- 接收端收到了 BlueCaster 傳送的每個媒體封包與訊框，且實際傳送的音訊至少達到要求長度的 90%
- 兩端解碼後的音訊完全相同，且與測試音一致（LDAC、LHDC V5 與 aptX Adaptive 除外，因為沒有開放原始碼的解碼器）

| 選項 | 說明 |
|:-------|:------------|
| `--codecs sbc aac ...` | 只測試這些編解碼器（`sbc sbcxq aac aptx aptxhd aptxll ldac lhdcv5 faststream opus opus05`）或協商案例（取樣率、低 bitpool、SBC 訊框設定、aptX Adaptive、...；`-h` 會列出全部）；預設：全部 |
| `--le-audio [case ...]` | 改為執行 LE Audio 案例（預設：全部） |
| `--auracast [case ...]` | 改為執行 Auracast 案例（預設：全部） |
| `--scan [case ...]` | 改為執行裝置掃描案例（`eir`、`name-request`） |
| `--link [case ...]` | 改為執行連結案例（`disconnect`、`device-link`、`device-a2dp`、`device-stream`、`restart-on-link`） |
| `--duration <s>` | 每種編解碼器的串流秒數（預設 5） |
| `--max-packet <bytes>` | 直接傳給 BlueCaster（媒體封包大小上限，請參閱 [使用方式](usage)） |
| `--bluecaster <exe>` | 要測試的核心（預設 `target/release/bluecaster-core.exe`） |
| `--decoder <exe>` | 解碼工具（預設 `<build-dir>/a2dp_decode/<config>/bluecaster_decode.exe`） |
| `--build-dir <dir>`、`--config <cfg>` | 解碼工具的建置位置（預設 `build`、`Release`） |
| `--base-port <port>` | 第一個 TCP 連接埠（預設 9101；LE Audio 案例使用 +100）；變更此值即可同時執行多個測試 |
| `--out <dir>` | 輸出資料夾（預設 `tools/emu/out`） |

每種編解碼器的 `tools/emu/out/<codec>/` 中存放擷取檔（`bluecaster.pklg`、`sink.pklg`）、記錄、`bluecaster_decode` 的報告，以及解碼後的 WAV 檔案。所有編解碼器都通過時，結束代碼為 0。

{: .note }
虛擬連結沒有無線電：它會檢查編碼、封包化、AVDTP / L2CAP 信令，以及與另一套獨立藍牙堆疊（Bumble）的互通性，但不會檢查無線電環境、實際硬體上的時序，或實際耳機如何播放音訊。Bumble 套件只是測試工具，不屬於 BlueCaster（請參閱 THIRD_PARTY_LICENSES.md）。

`emu_vhci.py` 則針對 Linux 藍牙堆疊做同樣的事：在 Linux 上，它透過 `/dev/vhci` 把虛擬介面卡接到 BlueZ，測量用接收器 `tools/linux_sink` 就能像使用實際介面卡一樣使用它。如此不需要硬體即可測試接收器，也可以選擇讓媒體封包遺失或延後送達（`--drop`、`--stall`）。請參閱 `tools/linux_sink/README.md`。

## 相依項目 {#dependencies}

C 程式庫是 git 子模組；Rust crate 與 UI 的 npm 套件由 Cargo 與 pnpm 取得（版本固定在 `Cargo.lock` 與 `app/src/ui/pnpm-lock.yaml`；`cargo deny check` 會檢查它們的授權）。

| 程式庫 | 方式 | 固定版本 | 授權 |
|:--------|:-------|:---------------|:--------|
| BTstack | git 子模組 | v1.8.3 | BTstack License（類似 BSD-3-Clause，附非商業條款） |
| Bluedroid SBC codec | 編碼器移植為 Rust；解碼器（測試、工具）取自 BTstack 隨附的副本 | 同 BTstack | Apache-2.0 |
| rijndael | 隨附於 BTstack（`3rd-party/rijndael`） | 同 BTstack | 公眾領域 |
| liblc3 (lc3-google) | 隨附於 BTstack（`3rd-party/lc3-google`） | 同 BTstack | Apache-2.0 |
| micro-ecc | 隨附於 BTstack（`3rd-party/micro-ecc`） | 同 BTstack | BSD-2-Clause |
| libldac (AOSP) | git 子模組 | android-15.0.0_r36-4-geeee1a3 | Apache-2.0 |
| libopenaptx | git 子模組 | 0.2.0（請勿更新） | LGPL-2.1+ |
| fdk-aac | git 子模組 | v3.0.0 | FDK AAC License |
| LHDC V5 編碼器 | git 子模組 | 依固定的版本 | Apache-2.0 |
| libopus | git 子模組 | v1.6.1 | BSD-3-Clause |
| Tauri、SolidJS、Rust crate | Cargo / pnpm | `Cargo.lock`、`pnpm-lock.yaml` | MIT / Apache-2.0 等（發行版本中的 `licenses/third-party/rust-crates.txt`） |

{: .warning }
由於連結了 BTstack，即使 BlueCaster 原始碼本身採用 Apache-2.0 或 MIT 雙重授權，您自行建置的 BlueCaster 也只能為個人、非商業目的使用與再散布。

完整的授權細節請參閱 [THIRD_PARTY_LICENSES.md](https://github.com/SeiyaFunaokaJP/BlueCaster/blob/main/THIRD_PARTY_LICENSES.md)。

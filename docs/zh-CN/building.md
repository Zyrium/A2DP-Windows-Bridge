---
title: 构建
layout: default
parent: 首页
nav_order: 3
---

# 从源代码构建
{: .no_toc }

## 目录
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## 环境要求 {#requirements}

- **Rust**（stable，1.90 或更高版本），通过 [rustup](https://rustup.rs/) 安装
- **Visual Studio 2022 或更高版本**（或其生成工具），并安装“使用 C++ 的桌面开发”工作负载，因为 C 库（BTstack、各编解码器）由 MSVC 编译
- 用于 UI 的 **Node.js** 22 和 **pnpm**（`corepack enable` 即可提供 pnpm）
- **Git**（用于子模块）
- 用于 `build.ps1` / `build.bat` 的 **PowerShell 7**（`pwsh`，`winget install Microsoft.PowerShell`）
- 用于检查（`tools/check_lang.py`）和[模拟器测试](#emu)的 **Python 3.11 或更高版本**
- ARM64 构建还需要：Visual Studio 组件“MSVC v143 - VS 2022 C++ ARM64 生成工具”，在 x64 电脑上还需要“适用于 Windows 的 C++ Clang 编译器”（rustls 的 `ring` 用 clang 汇编其 ARM64 代码；`installer/build-release.ps1` 会自动找到 Visual Studio 的 clang），以及 `rustup target add aarch64-pc-windows-msvc`

## 快速构建 {#quick-build}

```bash
git clone --recursive https://github.com/SeiyaFunaokaJP/BlueCaster.git
cd BlueCaster
pwsh ./build.ps1
```

也可以在资源管理器中双击 `build.bat`：它会运行 `build.ps1`，并在结束时等待按键（`build.bat -Test` 会把开关传给脚本）。`build.ps1` 先检查 Rust、Node.js、pnpm 和子模块是否齐全，在缺少 `node_modules` 时运行 `pnpm install`，然后构建窗口的页面，并把三个程序输出到 `target/release/`：`bluecaster-core.exe`（常驻核心：蓝牙、音频、托盘）、`bluecaster.exe`（窗口）和 `bluecaster-driver.exe`（以提升权限运行、负责分配 / 恢复 WinUSB 驱动程序的辅助程序）。请将这三个程序放在同一个文件夹中；`bluecaster-core.exe --cli` 可在不打开窗口的情况下串流。

开关：`-Debug`（开发配置，`target/debug/`）、`-Test`（同时运行“测试”中的检查）、`-Decoder`（同时构建解码工具）、`-Emu`（同时运行[模拟器测试](#emu)；`-Emu -Codecs sbc,ldac` 只测试这些编解码器）、`-Release -Arch x64|arm64`（仅运行发布脚本，见“发布包”）。

**分步构建**（即 `build.ps1` 所做的事；`bluecaster.exe` 会嵌入窗口的页面，因此必须先构建页面）：

```bash
cargo build --release -p bluecaster-core-exe -p bluecaster-driver
cd app/src/ui
pnpm install --frozen-lockfile
pnpm build
cd ../../..
cargo build --release -p bluecaster-ui
```

## 如果克隆时没有使用 `--recursive`

```bash
git submodule update --init --recursive
```

如果拉取的提交更新了某个库，请再运行一次。如果 `extern/` 下的某个库不在 `extern/versions.txt` 所列的提交上，构建会停止，错误消息会说明如何仍用其他版本构建。

## 项目结构 {#project-structure}

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

## 测试 {#tests}

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd app/src/ui && pnpm exec tsc --noEmit && pnpm exec vitest run
python tools/check_lang.py
```

## 发布包 {#release-packages}

```powershell
pwsh installer/build-release.ps1 -Arch x64
```

该命令会将安装程序（`BlueCaster-<version>-win64-setup.exe`）、便携版 zip、符号 zip（`.pdb` 文件，用于崩溃转储）和更新程序清单写入 `target/dist`。需要 Inno Setup 7.1.0（`winget install JRSoftware.InnoSetup.7 --version 7.1.0`）。应用内更新的签名及其他细节请参阅 `installer/README.md`。

## 无硬件测试（tools/emu） {#emu}

`tools/emu` 在同一台 PC 上把 BlueCaster 的每种编解码器串流到虚拟蓝牙接收端，无需适配器或耳机。它只能在 Windows 上运行，需要 **Python 3.11 或更高版本**；构建解码工具还需要 **CMake**（随 Visual Studio 附带）。

```bash
python tools/emu/setup_env.py                       # once: creates tools/emu/.venv with pinned packages
cmake -S tools/a2dp_decode -B build/a2dp_decode     # once: the decoder tool
cmake --build build/a2dp_decode --config Release
cargo build --release -p bluecaster-core-exe
python tools/emu/run_test.py                        # every codec and negotiation case
```

工作原理：

- `emu_sink.py` 在进程内链路上运行两个 [Bumble](https://github.com/google/bumble) 虚拟控制器。BlueCaster 通过 H4 over TCP（`127.0.0.1`）连接其中一个。另一个供 Bumble A2DP 接收端使用；该接收端提供 SBC（支持 SBC XQ）、AAC、aptX、aptX HD、aptX LL、LDAC、LHDC V5、FastStream、Opus、Opus 05 和 aptX Adaptive，并记录自身的 HCI 通信。
- `run_test.py` 针对每种编解码器，使用开发选项运行 `bluecaster-core.exe --cli`：`--hci-tcp`（使用虚拟控制器代替 WinUSB）、`--test-tone`（以左 1 kHz / 右 1.5 kHz 的测试音代替系统音频）、`--duration`、`--hci-capture`。环境变量 `BLUECASTER_CONFIG_DIR` 让 BlueCaster 使用单独的配置文件夹，因此与虚拟接收端配对不会影响你真实的链接密钥。
- 随后用 `bluecaster_decode` 检查两份 HCI 捕获（接收端一侧使用 `--received`）。

满足以下所有条件，该编解码器才算通过测试：

- 协商结果是所请求的编解码器
- `bluecaster_decode` 在发送端和接收端都没有发现问题
- 接收端收到了 BlueCaster 发送的每个媒体数据包和帧，并且至少发送了所请求音频时长的 90%
- 两端解码后的音频完全相同，并且与测试音一致（LDAC、LHDC V5 和 aptX Adaptive 除外，因为它们没有开源解码器）

| 选项 | 说明 |
|:-------|:------------|
| `--codecs sbc aac ...` | 只测试这些编解码器（`sbc sbcxq aac aptx aptxhd aptxll ldac lhdcv5 faststream opus opus05`）或协商用例（采样率、低 bitpool、SBC 帧配置、aptX Adaptive 等；`-h` 会列出它们）；默认：全部 |
| `--le-audio [case ...]` | 改为运行 LE Audio 用例（默认：全部） |
| `--auracast [case ...]` | 改为运行 Auracast 用例（默认：全部） |
| `--scan [case ...]` | 改为运行设备扫描用例（`eir`、`name-request`） |
| `--link [case ...]` | 改为运行链路用例（`disconnect`、`device-link`、`device-a2dp`、`device-stream`、`restart-on-link`） |
| `--duration <s>` | 每种编解码器的串流秒数（默认 5） |
| `--max-packet <bytes>` | 传递给 BlueCaster（最大媒体数据包大小，请参阅[使用方法](usage)） |
| `--bluecaster <exe>` | 要测试的核心（默认 `target/release/bluecaster-core.exe`） |
| `--decoder <exe>` | 解码工具（默认 `<build-dir>/a2dp_decode/<config>/bluecaster_decode.exe`） |
| `--build-dir <dir>`、`--config <cfg>` | 解码工具的构建位置（默认 `build`、`Release`） |
| `--base-port <port>` | 第一个 TCP 端口（默认 9101；LE Audio 用例使用 +100）；同时运行多个测试时请更改它 |
| `--out <dir>` | 输出文件夹（默认 `tools/emu/out`） |

每种编解码器的捕获文件（`bluecaster.pklg`、`sink.pklg`）、日志、`bluecaster_decode` 报告和解码后的 WAV 文件都保存在 `tools/emu/out/<codec>/` 中。所有编解码器都通过时，退出代码为 0。

{: .note }
虚拟链路不经过无线电：它检查编码、分包、AVDTP / L2CAP 信令以及与独立蓝牙堆栈（Bumble）的互操作性，但不检查无线环境、真实硬件上的时序，以及真实耳机如何播放音频。Bumble 软件包仅为测试工具，不属于 BlueCaster（请参阅 THIRD_PARTY_LICENSES.md）。

`emu_vhci.py` 为 Linux 蓝牙堆栈做同样的事：在 Linux 上，它通过 `/dev/vhci` 向 BlueZ 挂接一个虚拟适配器，测量用接收器 `tools/linux_sink` 会像使用真实适配器一样使用它。这样无需硬件即可测试接收器，还可以选择模拟丢失或滞留的媒体数据包（`--drop`、`--stall`）。请参阅 `tools/linux_sink/README.md`。

## 依赖项 {#dependencies}

C 库以 git 子模块形式引入；Rust crate 和 UI 的 npm 软件包由 Cargo 和 pnpm 获取（版本固定在 `Cargo.lock` 和 `app/src/ui/pnpm-lock.yaml` 中；`cargo deny check` 会检查它们的许可证）。

| 库 | 方式 | 固定版本 | 许可证 |
|:--------|:-------|:---------------|:--------|
| BTstack | git 子模块 | v1.8.3 | BTstack License（BSD-3-Clause 风格，附带非商业条款） |
| Bluedroid SBC 编解码器 | 编码器已移植为 Rust；解码器（测试、工具用）取自 BTstack 附带的副本 | 与 BTstack 相同 | Apache-2.0 |
| rijndael | 随 BTstack 附带（`3rd-party/rijndael`） | 与 BTstack 相同 | 公有领域 |
| liblc3 (lc3-google) | 随 BTstack 附带（`3rd-party/lc3-google`） | 与 BTstack 相同 | Apache-2.0 |
| micro-ecc | 随 BTstack 附带（`3rd-party/micro-ecc`） | 与 BTstack 相同 | BSD-2-Clause |
| libldac (AOSP) | git 子模块 | android-15.0.0_r36-4-geeee1a3 | Apache-2.0 |
| libopenaptx | git 子模块 | 0.2.0（请勿更新） | LGPL-2.1+ |
| fdk-aac | git 子模块 | v3.0.0 | FDK AAC License |
| LHDC V5 编码器 | git 子模块 | 以子模块固定的版本为准 | Apache-2.0 |
| libopus | git 子模块 | v1.6.1 | BSD-3-Clause |
| Tauri、SolidJS、Rust crate | Cargo / pnpm | `Cargo.lock`、`pnpm-lock.yaml` | MIT / Apache-2.0 等（见发布包中的 `licenses/third-party/rust-crates.txt`） |

{: .warning }
由于链接了 BTstack，你自行构建的任何 BlueCaster 都只能出于个人、非商业目的使用和再分发，即使 BlueCaster 源代码本身采用 Apache-2.0 或 MIT 双重许可。

完整的许可证详情请参阅 [THIRD_PARTY_LICENSES.md](https://github.com/SeiyaFunaokaJP/BlueCaster/blob/main/THIRD_PARTY_LICENSES.md)。

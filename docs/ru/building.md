---
title: Сборка
layout: default
parent: Главная
nav_order: 3
---

# Сборка из исходного кода
{: .no_toc }

## Содержание
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Требования {#requirements}

- **Rust** (stable, 1.90 или новее) через [rustup](https://rustup.rs/)
- **Visual Studio 2022 или новее** (или её Build Tools) с рабочей нагрузкой «Разработка классических приложений на C++»: MSVC компилирует библиотеки на C (BTstack, кодеки)
- **Node.js** 22 и **pnpm** для интерфейса (`corepack enable` устанавливает pnpm)
- **Git** (для подмодулей)
- **PowerShell 7** (`pwsh`) для `build.ps1` / `build.bat` (`winget install Microsoft.PowerShell`)
- **Python 3.11 или новее** для проверок (`tools/check_lang.py`) и [теста с эмулятором](#emu)
- Для сборки под ARM64: компоненты Visual Studio «MSVC v143 - VS 2022 C++ ARM64 build tools» и, на компьютере x64, «C++ Clang Compiler for Windows» (`ring` из rustls собирает свой код ARM64 с помощью clang, а `installer/build-release.ps1` сам находит clang из Visual Studio), а также `rustup target add aarch64-pc-windows-msvc`

## Быстрая сборка {#quick-build}

```bash
git clone --recursive https://github.com/SeiyaFunaokaJP/BlueCaster.git
cd BlueCaster
pwsh ./build.ps1
```

Или дважды щёлкните `build.bat` в проводнике: он запустит `build.ps1` и в конце будет ждать нажатия клавиши (параметры передаются дальше, например `build.bat -Test`). Сначала `build.ps1` проверяет, что Rust, Node.js, pnpm и подмодули на месте, и запускает `pnpm install`, если нет `node_modules`. Затем он собирает страницу окна и три программы в `target/release/`: `bluecaster-core.exe` (фоновое ядро: Bluetooth, звук, область уведомлений), `bluecaster.exe` (окно) и `bluecaster-driver.exe` (вспомогательная программа с правами администратора, которая назначает и восстанавливает драйвер WinUSB). Все три программы должны лежать в одной папке. `bluecaster-core.exe --cli` передаёт звук без окна.

Параметры: `-Debug` (профиль dev, `target/debug/`), `-Test` (дополнительно запускает проверки из раздела «Тесты»), `-Decoder` (дополнительно собирает инструмент декодирования), `-Emu` (дополнительно запускает [тест с эмулятором](#emu); `-Emu -Codecs sbc,ldac` проверяет только эти кодеки), `-Release -Arch x64|arm64` (запускает только скрипт выпуска, см. «Пакеты выпуска»).

**Пошагово** (то же, что делает `build.ps1`; страницу окна нужно собрать раньше `bluecaster.exe`, потому что она встраивается в него):

```bash
cargo build --release -p bluecaster-core-exe -p bluecaster-driver
cd app/src/ui
pnpm install --frozen-lockfile
pnpm build
cd ../../..
cargo build --release -p bluecaster-ui
```

## Если вы уже клонировали без `--recursive`

```bash
git submodule update --init --recursive
```

Запускайте эту команду и после каждого pull, который обновляет библиотеку. Если библиотека в `extern/` не на том коммите, что указан в `extern/versions.txt`, сборка останавливается. В сообщении об ошибке сказано, как всё же собрать с другой версией.

## Структура проекта {#project-structure}

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

## Тесты {#tests}

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd app/src/ui && pnpm exec tsc --noEmit && pnpm exec vitest run
python tools/check_lang.py
```

## Пакеты выпуска {#release-packages}

```powershell
pwsh installer/build-release.ps1 -Arch x64
```

создаёт в `target/dist` установщик (`BlueCaster-<version>-win64-setup.exe`), портативный ZIP-архив, ZIP-архив символов (файлы `.pdb`, для дампов сбоев) и манифест обновлений. Для этого нужен Inno Setup 7.1.0 (`winget install JRSoftware.InnoSetup.7 --version 7.1.0`). Подпись обновлений внутри приложения и остальные подробности описаны в `installer/README.md`.

## Тестирование без оборудования (tools/emu) {#emu}

`tools/emu` передаёт звук из BlueCaster всеми кодеками на виртуальный приёмник Bluetooth на том же компьютере, без адаптера и наушников. Он работает только в Windows. Нужны **Python 3.11 или новее** и **CMake** для инструмента декодирования (входит в Visual Studio).

```bash
python tools/emu/setup_env.py                       # once: creates tools/emu/.venv with pinned packages
cmake -S tools/a2dp_decode -B build/a2dp_decode     # once: the decoder tool
cmake --build build/a2dp_decode --config Release
cargo build --release -p bluecaster-core-exe
python tools/emu/run_test.py                        # every codec and negotiation case
```

Как это работает:

- `emu_sink.py` запускает два виртуальных контроллера [Bumble](https://github.com/google/bumble), связанных внутри одного процесса. BlueCaster подключается к одному из них как H4 через TCP (`127.0.0.1`). Другой использует приёмник A2DP на Bumble. Приёмник предлагает SBC (с поддержкой SBC XQ), AAC, aptX, aptX HD, aptX LL, LDAC, LHDC V5, FastStream, Opus, Opus 05 и aptX Adaptive и записывает свой трафик HCI.
- `run_test.py` запускает `bluecaster-core.exe --cli` для каждого кодека с параметрами для разработки: `--hci-tcp` (виртуальный контроллер вместо WinUSB), `--test-tone` (1 kHz слева / 1,5 kHz справа вместо системного звука), `--duration`, `--hci-capture`. Переменная среды `BLUECASTER_CONFIG_DIR` указывает BlueCaster отдельную папку настроек, поэтому сопряжение с виртуальным приёмником не затрагивает ваши настоящие ключи связи.
- Затем обе записи HCI проверяются с помощью `bluecaster_decode` (сторона приёмника — с `--received`).

Кодек проходит тест, если выполнены все условия:

- согласован запрошенный кодек
- `bluecaster_decode` не находит проблем ни на передающей, ни на принимающей стороне
- приёмник получил все медиапакеты и кадры, отправленные BlueCaster, и отправлено не менее 90% запрошенной длительности звука
- декодированный звук одинаков на обеих сторонах и совпадает с тестовым сигналом (кроме LDAC, LHDC V5 и aptX Adaptive, для которых нет декодера с открытым исходным кодом)

| Параметр | Описание |
|:-------|:------------|
| `--codecs sbc aac ...` | Проверять только эти кодеки (`sbc sbcxq aac aptx aptxhd aptxll ldac lhdcv5 faststream opus opus05`) или случаи согласования (частоты дискретизации, низкие bitpool, конфигурации кадров SBC, aptX Adaptive, ...; `-h` выводит их список); по умолчанию: все |
| `--le-audio [case ...]` | Вместо этого выполнить случаи LE Audio (по умолчанию: все) |
| `--auracast [case ...]` | Вместо этого выполнить случаи Auracast (по умолчанию: все) |
| `--scan [case ...]` | Вместо этого выполнить случаи поиска устройств (`eir`, `name-request`) |
| `--link [case ...]` | Вместо этого выполнить случаи соединения (`disconnect`, `device-link`, `device-a2dp`, `device-stream`, `restart-on-link`) |
| `--duration <s>` | Длительность передачи на кодек в секундах (по умолчанию 5) |
| `--max-packet <bytes>` | Передаётся в BlueCaster (максимальный размер медиапакета, см. [Использование](usage)) |
| `--bluecaster <exe>` | Проверяемое ядро (по умолчанию `target/release/bluecaster-core.exe`) |
| `--decoder <exe>` | Инструмент декодирования (по умолчанию `<build-dir>/a2dp_decode/<config>/bluecaster_decode.exe`) |
| `--build-dir <dir>`, `--config <cfg>` | Где собран инструмент декодирования (по умолчанию `build`, `Release`) |
| `--base-port <port>` | Первый порт TCP (по умолчанию 9101; случаи LE Audio используют +100). Измените его, чтобы запускать несколько тестов одновременно |
| `--out <dir>` | Папка результатов (по умолчанию `tools/emu/out`) |

Для каждого кодека в `tools/emu/out/<codec>/` лежат записи (`bluecaster.pklg`, `sink.pklg`), журналы, отчёты `bluecaster_decode` и декодированные файлы WAV. Код выхода равен 0, если все кодеки прошли тест.

{: .note }
В виртуальном соединении нет радиоканала. Тест проверяет кодирование, упаковку в пакеты, сигнализацию AVDTP / L2CAP и совместимость с независимым стеком Bluetooth (Bumble). Условия радиосвязи, тайминги на реальном оборудовании и воспроизведение на настоящих наушниках он не проверяет. Пакеты Bumble нужны только для тестов и не входят в BlueCaster (см. THIRD_PARTY_LICENSES.md).

`emu_vhci.py` делает то же самое для стека Bluetooth в Linux. Он подключает к BlueZ виртуальный адаптер через `/dev/vhci`, и измерительный приёмник `tools/linux_sink` использует его как настоящий адаптер. Так приёмник можно проверить без оборудования, при желании с потерянными или задержанными медиапакетами (`--drop`, `--stall`). См. `tools/linux_sink/README.md`.

## Зависимости {#dependencies}

Библиотеки на C подключены как подмодули git. Крейты Rust и пакеты npm для интерфейса загружают Cargo и pnpm (версии зафиксированы в `Cargo.lock` и `app/src/ui/pnpm-lock.yaml`, а `cargo deny check` проверяет их лицензии).

| Библиотека | Способ | Зафиксированная версия | Лицензия |
|:--------|:-------|:---------------|:--------|
| BTstack | подмодуль git | v1.8.3 | BTstack License (в духе BSD-3-Clause с условием о некоммерческом использовании) |
| Кодек SBC из Bluedroid | кодировщик перенесён на Rust; декодер (тесты, инструменты) — из копии в BTstack | как в BTstack | Apache-2.0 |
| rijndael | входит в BTstack (`3rd-party/rijndael`) | как в BTstack | Общественное достояние (Public Domain) |
| liblc3 (lc3-google) | входит в BTstack (`3rd-party/lc3-google`) | как в BTstack | Apache-2.0 |
| micro-ecc | входит в BTstack (`3rd-party/micro-ecc`) | как в BTstack | BSD-2-Clause |
| libldac (AOSP) | подмодуль git | android-15.0.0_r36-4-geeee1a3 | Apache-2.0 |
| libopenaptx | подмодуль git | 0.2.0 (не обновлять) | LGPL-2.1+ |
| fdk-aac | подмодуль git | v3.0.0 | FDK AAC License |
| Кодировщик LHDC V5 | подмодуль git | как зафиксировано | Apache-2.0 |
| libopus | подмодуль git | v1.6.1 | BSD-3-Clause |
| Tauri, SolidJS, крейты Rust | Cargo / pnpm | `Cargo.lock`, `pnpm-lock.yaml` | MIT / Apache-2.0 и другие (`licenses/third-party/rust-crates.txt` в выпуске) |

{: .warning }
BTstack компонуется в программу, поэтому любую собранную вами копию BlueCaster можно использовать и распространять только в личных некоммерческих целях. Это так, хотя сам исходный код BlueCaster распространяется по двойной лицензии Apache-2.0 или MIT.

Полные сведения о лицензиях см. в [THIRD_PARTY_LICENSES.md](https://github.com/SeiyaFunaokaJP/BlueCaster/blob/main/THIRD_PARTY_LICENSES.md).

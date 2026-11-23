---
title: Compilación
layout: default
parent: Inicio
nav_order: 3
---

# Compilación desde el código fuente
{: .no_toc }

## Contenido
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Requisitos {#requirements}

- **Rust** (stable, 1.90 o posterior) mediante [rustup](https://rustup.rs/)
- **Visual Studio 2022 o posterior** (o sus Build Tools) con la carga de trabajo "Desarrollo para el escritorio con C++", porque MSVC compila las bibliotecas en C (BTstack, los códecs)
- **Node.js** 22 y **pnpm** para la interfaz (`corepack enable` proporciona pnpm)
- **Git** (para los submódulos)
- **PowerShell 7** (`pwsh`) para `build.ps1` / `build.bat` (`winget install Microsoft.PowerShell`)
- **Python 3.11 o posterior** para las comprobaciones (`tools/check_lang.py`) y la [prueba con el emulador](#emu)
- Para compilaciones ARM64: los componentes de Visual Studio "MSVC v143 - VS 2022 C++ ARM64 build tools" y, en un PC x64, "C++ Clang Compiler for Windows" (`ring`, de rustls, ensambla su código ARM64 con clang; `installer/build-release.ps1` encuentra automáticamente el clang de Visual Studio), y `rustup target add aarch64-pc-windows-msvc`

## Compilación rápida {#quick-build}

```bash
git clone --recursive https://github.com/SeiyaFunaokaJP/BlueCaster.git
cd BlueCaster
pwsh ./build.ps1
```

O haga doble clic en `build.bat` en el Explorador de archivos: ejecuta `build.ps1` y espera a que pulse una tecla al final (`build.bat -Test` le pasa las opciones). `build.ps1` comprueba primero que Rust, Node.js, pnpm y los submódulos estén presentes y ejecuta `pnpm install` si falta `node_modules`. Después compila la página de la ventana y los tres programas en `target/release/`: `bluecaster-core.exe` (el núcleo residente: Bluetooth, audio, bandeja), `bluecaster.exe` (la ventana) y `bluecaster-driver.exe` (el auxiliar elevado que asigna / restaura el controlador WinUSB). Mantenga los tres en la misma carpeta; `bluecaster-core.exe --cli` transmite sin la ventana.

Opciones: `-Debug` (perfil de desarrollo, `target/debug/`), `-Test` (ejecuta también las comprobaciones de «Pruebas»), `-Decoder` (compila también la herramienta de decodificación), `-Emu` (ejecuta también la [prueba con el emulador](#emu); `-Emu -Codecs sbc,ldac` prueba solo esos códecs), `-Release -Arch x64|arm64` (ejecuta solo el script de versión; consulte «Paquetes de versión»).

**Paso a paso** (lo que hace `build.ps1`; la página de la ventana debe compilarse antes que `bluecaster.exe`, que la incrusta):

```bash
cargo build --release -p bluecaster-core-exe -p bluecaster-driver
cd app/src/ui
pnpm install --frozen-lockfile
pnpm build
cd ../../..
cargo build --release -p bluecaster-ui
```

## Si ya clonó sin `--recursive`

```bash
git submodule update --init --recursive
```

Vuelva a ejecutarlo después de un pull que actualice una biblioteca. La compilación se detiene si una biblioteca de `extern/` no está en el commit indicado en `extern/versions.txt`, y el mensaje de error explica cómo compilar otra versión de todos modos.

## Estructura del proyecto {#project-structure}

```
BlueCaster/
├── app/
│   ├── src/
│   │   ├── core/           bluecaster-core.exe: núcleo residente, bandeja, servidor IPC, --cli
│   │   ├── ui/             bluecaster.exe: Tauri 2 + SolidJS (pnpm dev la ejecuta en un navegador con un núcleo simulado)
│   │   ├── driver/         bluecaster-driver.exe: asignar / restaurar WinUSB (elevado)
│   │   └── crates/         Bibliotecas: btstack-sys, hci-transport, bt-host, codec, los crates -sys de los códecs,
│   │                       pipeline, audio-capture, win-sys, core (bluecaster-core), ipc, le-audio, lc3-sys,
│   │                       aptx-adaptive (codificador aptX Adaptive)
│   ├── lang/               Localización (en.json es el original; tools/check_lang.py comprueba los demás)
│   └── resources/          Icono y manifiesto
├── Cargo.toml              Workspace de Cargo
├── installer/              Script de versión, script de Inno Setup, notas del actualizador (installer/README.md)
├── compat/                 Encabezados de compatibilidad MSVC para el código de AOSP
├── docs/                   Documentación (este sitio)
├── extern/                 Bibliotecas de terceros en C (submódulos de git)
│   ├── btstack/            Pila Bluetooth BTstack
│   ├── libldac/            Codificador LDAC de AOSP
│   ├── libopenaptx/        Codificador aptX / aptX HD / aptX LL
│   ├── fdk-aac/            Codificador AAC de Fraunhofer
│   ├── lhdcv5-enc/         Codificador LHDC V5
│   └── opus/               libopus (codificador Opus)
└── tools/
    ├── a2dp_decode/        bluecaster_decode: comprueba / decodifica el flujo multimedia de una captura HCI (.pklg)
    ├── emu/                Prueba de extremo a extremo frente a un receptor Bluetooth virtual (Python)
    └── linux_sink/         bluecaster_sink: receptor A2DP de medición en Linux (Python, Bumble)
```

## Pruebas {#tests}

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd app/src/ui && pnpm exec tsc --noEmit && pnpm exec vitest run
python tools/check_lang.py
```

## Paquetes de versión {#release-packages}

```powershell
pwsh installer/build-release.ps1 -Arch x64
```

genera en `target/dist` el instalador (`BlueCaster-<version>-win64-setup.exe`), el zip portable, el zip de símbolos (archivos `.pdb`, para los volcados de bloqueo) y el manifiesto del actualizador. Necesita Inno Setup 7.1.0 (`winget install JRSoftware.InnoSetup.7 --version 7.1.0`). `installer/README.md` explica la firma de las actualizaciones desde la aplicación y otros detalles.

## Pruebas sin hardware (tools/emu) {#emu}

`tools/emu` transmite todos los códecs desde BlueCaster a un receptor Bluetooth virtual en el mismo PC, sin adaptador ni auriculares. Solo funciona en Windows y necesita **Python 3.11 o posterior** y **CMake** (incluido con Visual Studio) para la herramienta de decodificación.

```bash
python tools/emu/setup_env.py                       # una vez: crea tools/emu/.venv con paquetes de versión fija
cmake -S tools/a2dp_decode -B build/a2dp_decode     # una vez: la herramienta de decodificación
cmake --build build/a2dp_decode --config Release
cargo build --release -p bluecaster-core-exe
python tools/emu/run_test.py                        # todos los códecs y casos de negociación
```

Cómo funciona:

- `emu_sink.py` ejecuta dos controladores virtuales de [Bumble](https://github.com/google/bumble) sobre un enlace dentro del proceso. BlueCaster se conecta a uno de ellos como H4 por TCP (`127.0.0.1`). Un receptor A2DP de Bumble usa el otro; ofrece SBC (compatible con SBC XQ), AAC, aptX, aptX HD, aptX LL, LDAC, LHDC V5, FastStream, Opus, Opus 05 y aptX Adaptive, y registra su tráfico HCI.
- `run_test.py` ejecuta `bluecaster-core.exe --cli` para cada códec con opciones de desarrollo: `--hci-tcp` (controlador virtual en lugar de WinUSB), `--test-tone` (1 kHz a la izquierda / 1.5 kHz a la derecha en lugar del audio del sistema), `--duration`, `--hci-capture`. La variable de entorno `BLUECASTER_CONFIG_DIR` hace que BlueCaster use una carpeta de configuración aparte, de modo que el emparejamiento con el receptor virtual nunca toca sus claves de enlace reales.
- Después, ambas capturas HCI se comprueban con `bluecaster_decode` (la del receptor con `--received`).

Un códec supera la prueba cuando se cumple todo esto:

- se negoció el códec solicitado
- `bluecaster_decode` no encuentra ningún problema ni en el lado emisor ni en el receptor
- el receptor recibió todos los paquetes multimedia y todas las tramas que envió BlueCaster, y se envió al menos el 90 % de la duración de audio solicitada
- el audio decodificado es idéntico en ambos lados y coincide con el tono de prueba (salvo en LDAC, LHDC V5 y aptX Adaptive, que no tienen decodificador de código abierto)

| Opción | Descripción |
|:-------|:------------|
| `--codecs sbc aac ...` | Probar solo estos códecs (`sbc sbcxq aac aptx aptxhd aptxll ldac lhdcv5 faststream opus opus05`) o casos de negociación (frecuencias de muestreo, bitpools bajos, configuraciones de tramas SBC, aptX Adaptive, ...; `-h` los enumera); predeterminado: todos |
| `--le-audio [case ...]` | Ejecutar en su lugar los casos de LE Audio (predeterminado: todos) |
| `--auracast [case ...]` | Ejecutar en su lugar los casos de Auracast (predeterminado: todos) |
| `--scan [case ...]` | Ejecutar en su lugar los casos de búsqueda de dispositivos (`eir`, `name-request`) |
| `--link [case ...]` | Ejecutar en su lugar los casos de enlace (`disconnect`, `device-link`, `device-a2dp`, `device-stream`, `restart-on-link`) |
| `--duration <s>` | Segundos de transmisión por códec (predeterminado 5) |
| `--max-packet <bytes>` | Se pasa a BlueCaster (tamaño máximo del paquete multimedia, consulte [Uso](usage)) |
| `--bluecaster <exe>` | El núcleo que se prueba (predeterminado `target/release/bluecaster-core.exe`) |
| `--decoder <exe>` | La herramienta de decodificación (predeterminado `<build-dir>/a2dp_decode/<config>/bluecaster_decode.exe`) |
| `--build-dir <dir>`, `--config <cfg>` | Dónde se compiló la herramienta de decodificación (predeterminado `build`, `Release`) |
| `--base-port <port>` | Primer puerto TCP (predeterminado 9101; los casos de LE Audio usan +100); cámbielo para ejecutar varias pruebas a la vez |
| `--out <dir>` | Carpeta de salida (predeterminado `tools/emu/out`) |

Para cada códec, `tools/emu/out/<codec>/` contiene las capturas (`bluecaster.pklg`, `sink.pklg`), los registros, los informes de `bluecaster_decode` y los archivos WAV decodificados. El código de salida es 0 cuando todos los códecs superan la prueba.

{: .note }
El enlace virtual no tiene radio: comprueba la codificación, la paquetización, la señalización AVDTP / L2CAP y la interoperabilidad con una pila Bluetooth independiente (Bumble), pero no las condiciones de radio, la temporización en hardware real ni cómo reproducen el audio unos auriculares reales. Los paquetes de Bumble son solo herramientas de prueba y no forman parte de BlueCaster (consulte THIRD_PARTY_LICENSES.md).

`emu_vhci.py` hace lo mismo con la pila Bluetooth de Linux: en Linux conecta un adaptador virtual a BlueZ a través de `/dev/vhci`, y el receptor de medición `tools/linux_sink` lo usa como un adaptador real. Así se puede probar el receptor sin hardware, opcionalmente con paquetes multimedia perdidos o retenidos (`--drop`, `--stall`). Consulte `tools/linux_sink/README.md`.

## Dependencias {#dependencies}

Las bibliotecas en C son submódulos de git; los crates de Rust y los paquetes npm de la interfaz los descargan Cargo y pnpm (versiones fijadas en `Cargo.lock` y `app/src/ui/pnpm-lock.yaml`; `cargo deny check` comprueba sus licencias).

| Biblioteca | Método | Versión fijada | Licencia |
|:--------|:-------|:---------------|:--------|
| BTstack | submódulo de git | v1.8.3 | Licencia de BTstack (tipo BSD-3-Clause con una cláusula no comercial) |
| Códec SBC de Bluedroid | codificador portado a Rust; el decodificador (pruebas, herramientas), de la copia incluida en BTstack | la de BTstack | Apache-2.0 |
| rijndael | incluido en BTstack (`3rd-party/rijndael`) | la de BTstack | Dominio público |
| liblc3 (lc3-google) | incluido en BTstack (`3rd-party/lc3-google`) | la de BTstack | Apache-2.0 |
| micro-ecc | incluido en BTstack (`3rd-party/micro-ecc`) | la de BTstack | BSD-2-Clause |
| libldac (AOSP) | submódulo de git | android-15.0.0_r36-4-geeee1a3 | Apache-2.0 |
| libopenaptx | submódulo de git | 0.2.0 (no actualizar) | LGPL-2.1+ |
| fdk-aac | submódulo de git | v3.0.0 | FDK AAC License |
| Codificador LHDC V5 | submódulo de git | la fijada | Apache-2.0 |
| libopus | submódulo de git | v1.6.1 | BSD-3-Clause |
| Tauri, SolidJS, crates de Rust | Cargo / pnpm | `Cargo.lock`, `pnpm-lock.yaml` | MIT / Apache-2.0 y otras (`licenses/third-party/rust-crates.txt` en la versión publicada) |

{: .warning }
Como BTstack se enlaza en el programa, cualquier BlueCaster que compile solo puede usarse y redistribuirse con fines personales y no comerciales, aunque el código fuente de BlueCaster en sí tenga licencia dual Apache-2.0 o MIT.

Encontrará todos los detalles de las licencias en [THIRD_PARTY_LICENSES.md](https://github.com/SeiyaFunaokaJP/BlueCaster/blob/main/THIRD_PARTY_LICENSES.md).

---
title: Arquitectura
layout: default
parent: Inicio
nav_order: 4
---

# Arquitectura
{: .no_toc }

## Contenido
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Descripción general {#overview}

BlueCaster habilita el audio Bluetooth LHDC V5, LDAC, aptX Adaptive, aptX HD,
aptX Low Latency, aptX, Opus, AAC, SBC XQ y SBC en Windows
(y LC3 sobre LE Audio, experimental). La pila Bluetooth propia de Windows solo admite
SBC, aptX clásico y (solo en Windows 11) AAC para A2DP; BlueCaster añade los
demás sin un controlador de kernel.

Usa **BTstack + WinUSB**: todo se ejecuta en modo de usuario, así que no hace falta firmar controladores.

BlueCaster 2.0 está escrito en Rust. Las bibliotecas en C que no se pueden sustituir
(BTstack y los codificadores de los códecs) se compilan desde el código fuente y se llaman a través
de pequeños crates FFI; todo lo demás, incluido el codificador SBC, es Rust.

## Procesos {#processes}

BlueCaster se ejecuta como hasta tres programas:

| Programa | Se ejecuta | Función |
|:--------|:-----|:-----|
| `bluecaster-core.exe` | Mientras BlueCaster está en ejecución (lo inician la ventana, el menú Inicio o **Iniciar con Windows**) | Bluetooth, captura y codificación del audio, perfiles y configuración, el icono de la bandeja y su menú, el servidor IPC. `--cli` ejecuta el mismo núcleo desde una consola |
| `bluecaster.exe` | Solo mientras hay una ventana abierta (se cierra 3 minutos después de cerrar la última) | La ventana principal y el panel desplegable de la bandeja (Tauri 2, SolidJS + TypeScript). No guarda ningún estado propio |
| `bluecaster-driver.exe` | Brevemente, con derechos de administrador (UAC), para un cambio de controlador o una importación de claves | Asigna WinUSB a un adaptador, restaura el controlador anterior del adaptador o lee las claves de enlace que el Bluetooth de Windows guarda para un adaptador (`windows-keys`), y se cierra |

```mermaid
---
config:
  flowchart:
    wrappingWidth: 480
    rankSpacing: 30
---
flowchart TB
    ui["bluecaster.exe:<br>ventana y panel desplegable<br>de la bandeja<br>(Tauri + SolidJS)"]
    src["Audio de Windows:<br>loopback del sistema /<br>dispositivo virtual /<br>una aplicación"]
    subgraph core["bluecaster-core.exe"]
        ctl["Menú de la bandeja /<br>servidor IPC / CLI (--cli)"] --> svc["Servicio del núcleo:<br>máquina de estados de la conexión,<br>selección de códec, perfiles,<br>reconexión"]
        svc --> cap["Captura WASAPI"]
        cap -- PCM --> enc["Canal de codificación:<br>LDAC / familia aptX / LHDC V5 /<br>Opus / AAC / SBC"]
        enc --> bt["BTstack:<br>HCI / L2CAP / AVDTP /<br>A2DP / AVRCP"]
    end
    ui <-->|"canalización con nombre<br>por usuario"| ctl
    src --> cap
    bt -->|"WinUSB (nusb)"| usb["Adaptador Bluetooth USB<br>(controlador WinUSB)"]
    usb -. Bluetooth .-> hp["Auriculares / altavoces"]
    usb -.-|SetupAPI| drv["bluecaster-driver.exe (elevado):<br>asignar / restaurar WinUSB,<br>leer las claves de enlace de Windows"]
```

- **El núcleo no es un servicio de Windows.** Es un proceso normal del
  usuario que ha iniciado sesión, porque el loopback de WASAPI captura el audio de la sesión de ese usuario.
  Un mutex por sesión permite una sola instancia; si se vuelve a iniciar el núcleo, se abre la
  ventana.
- **La bandeja pertenece al núcleo.** El menú contextual del icono de la bandeja (abrir, los 10 primeros
  dispositivos, desconectar, salir) se abre al instante, sin iniciar la ventana. Un
  clic izquierdo abre el panel desplegable (o la ventana
  principal, según la Configuración).
- **La ventana solo muestra el estado.** Recibe una instantánea del estado del núcleo
  y después cada cambio. Cerrar la ventana, o que la ventana se bloquee, no detiene
  el audio. Cerrar la ventana solo la oculta, así que volver a abrirla desde la bandeja
  es instantáneo.
- **La línea de comandos usa el mismo núcleo.** `bluecaster-core.exe --cli` ejecuta el
  servicio del núcleo sin bandeja ni IPC; la prueba con el emulador usa este modo.

## Transporte: BTstack + WinUSB

BlueCaster prescinde por completo de la pila Bluetooth de Windows y se comunica directamente con un
adaptador Bluetooth USB mediante WinUSB (el controlador USB genérico firmado por Microsoft). BTstack,
una pila Bluetooth de código disponible (licencia no comercial), implementa HCI, L2CAP, AVDTP y A2DP en
modo de usuario.

**Ventajas**:
- Sin coste de firma de controladores
- No requiere el modo de firma de prueba
- No hay que desactivar el Arranque seguro
- Todo el código se ejecuta en modo de usuario (depuración más sencilla)

**Inconvenientes**:
- Requiere un adaptador Bluetooth USB dedicado (aparte del Bluetooth integrado
  de Windows)
- Mientras WinUSB lo controla, el adaptador no está disponible para el Bluetooth
  normal de Windows
- Los dispositivos se encuentran con una búsqueda (GAP inquiry) con el adaptador USB, o
  se escribe su dirección a mano

**Cómo funciona**:
1. El usuario asigna WinUSB a un adaptador Bluetooth USB, desde la aplicación (**Adaptador >
   Reemplazar por el controlador WinUSB**, consulte [Asignación del controlador](#driver-assignment)) o con Zadig
2. El crate `hci-transport` abre el adaptador a través de WinUSB con `nusb`
   y lo conecta a BTstack como su transporte HCI
3. BTstack envía comandos HCI para inicializar el controlador Bluetooth
4. En los adaptadores Realtek, Intel y Broadcom, BlueCaster carga el firmware si es necesario
5. BTstack establece una conexión ACL y abre canales L2CAP (PSM 0x0019)
6. La señalización AVDTP descubre los SEP remotos y negocia los códecs
7. El audio codificado se envía como paquetes multimedia AVDTP sobre L2CAP

```mermaid
sequenceDiagram
    participant App as BlueCaster (BTstack)
    participant Ad as Adaptador USB
    participant Sink as Auriculares
    App->>Ad: Apertura mediante WinUSB,<br>reinicio / inicialización HCI
    opt Realtek, Intel, Broadcom
        App->>Ad: Carga del firmware
    end
    App->>Sink: Conexión ACL
    Note over App,Sink: SSP Just Works en la primera conexión<br>(se guarda la clave de enlace)
    App->>Sink: Canal L2CAP, PSM 0x0019<br>(señalización AVDTP)
    App->>Sink: AVDTP DISCOVER / GET_CAPABILITIES
    Sink-->>App: Extremos de transmisión y códecs
    App->>Sink: SET_CONFIGURATION<br>(códec seleccionado), OPEN, START
    loop Durante la transmisión
        App->>Sink: Paquetes multimedia<br>AVDTP sobre L2CAP
    end
```

El transporte USB conserva los pasos de recuperación que necesitaron los adaptadores reales en
la v1: restablece las canalizaciones de eventos y ACL al abrir el adaptador, reintenta una lectura
de eventos fallida, aplica un tiempo de espera a los comandos HCI y envía HCI Reset al cerrar el adaptador
(salvo a un controlador que sigue en su bootloader), de modo que la siguiente
sesión encuentra un controlador inactivo.

## Estructura de crates {#crate-structure}

El código es un único workspace de Cargo (`Cargo.toml` en la raíz del repositorio) en cinco capas;
cada capa depende solo de las capas inferiores. Las bibliotecas están en
`app/src/crates/`, y los programas, en `app/src/core`, `app/src/ui` y
`app/src/driver`. El crate `aptx-adaptive`, incluido en el repositorio, es una dependencia por ruta de
`codec` y queda fuera del workspace. Solo los crates `*-sys` y `win-sys` pueden contener código `unsafe`;
todos los demás crates tienen `#![forbid(unsafe_code)]`.

| Capa | Crate | Función |
|:------|:------|:-----|
| 1. FFI | `btstack-sys` | Compila BTstack con `cc` (MSVC), declaraciones FFI escritas a mano y envoltorios seguros, `btstack_config.h`. BTstack solo puede llamarse en el hilo de su bucle de ejecución, algo que garantiza el token `!Send` `Stack` |
| 1. FFI | `ldac-sys`, `openaptx-sys`, `fdkaac-sys`, `lhdcv5-sys`, `opus-sys`, `lc3-sys` | Compilan los codificadores en C y los exponen; un crate por biblioteca, para que cada licencia quede separada |
| 1. FFI | `win-sys` | API de Win32 sin biblioteca de Rust: MMCSS, el dispositivo de audio predeterminado (IPolicyConfig), la API WLAN (AFH), las notificaciones de energía, la clave Run, las teclas multimedia, la DACL de la canalización, la elevación, la asignación del controlador WinUSB (SetupAPI) y el Bluetooth propio de Windows (sus radios, dispositivos emparejados y claves de enlace) |
| 2. Componentes | `hci-transport` | HCI sobre USB («H2») con `nusb`, y H4 sobre TCP hacia un controlador virtual (`tools/emu`) |
| 2. Componentes | `aptx-adaptive` | El codificador aptX Adaptive en Rust (incluido en el repositorio desde AKS-Logic/aptx-adaptive-linux, MIT) |
| 2. Componentes | `codec` | El trait `Encoder`, el codificador SBC (port en Rust del de Bluedroid), envoltorios seguros de los codificadores en C, el análisis de Codec Information y la elección del códec |
| 2. Componentes | `audio-capture` | Captura WASAPI (crate `wasapi`): loopback del sistema, dispositivo virtual, una aplicación (loopback de proceso); listas de dispositivos y aplicaciones, tono de prueba, volumen por aplicación (Mezclador de volumen) |
| 2. Componentes | `voice-link` | El audio del micrófono del auricular (modo de desarrollador): PCM mono reproducido en el dispositivo de salida elegido (WASAPI), la captura mezclada a mono para el sentido de subida y los códecs de voz CVSD / mSBC / LC3-SWB sobre cargas SCO |
| 3. Motor | `bt-host` | Ejecuta BTstack en su propio hilo, controlado mediante comandos y eventos tipados: GAP, A2DP Source, AVRCP, SDP, canales RFCOMM para el paso directo (`rfcomm.*`), claves de enlace, los servicios SDP de un dispositivo, AFH, carga de firmware y cargadores de chips (`bringup`), la lectura de datos del chip y los scripts de chip, captura HCI (`.pklg`), LE Audio (`le/`) |
| 2. Componentes | `le-audio` | LE Audio: la lógica sin BTstack ni Windows (números asignados, LTV, PDU de PACS / ASCS, preajustes, los modelos CSIP / VCP / MCP / CCP) |
| 3. Motor | `pipeline` | Búferes circulares de PCM, conversión de formato, mezcla descendente, codificación, cargas útiles multimedia, ritmo de envío, ABR |
| 4. Núcleo | `bluecaster-core` (directorio `core`) | La máquina de estados, las reconexiones, suspensión / reanudación, los perfiles, la configuración y la migración desde la v1, la sincronización de volumen, el silencio de la salida, la búsqueda de actualizaciones, la descarga del firmware de Realtek, el cliente de la prueba con receptor, las estadísticas, los protocolos de fabricante (`vendor/`) |
| 4. Núcleo | `ipc` | Los tipos del protocolo compartidos por el núcleo y la ventana (serde; tipos de TypeScript generados con `ts-rs`) y el entramado de la canalización |
| 5. Programas | `bluecaster-core-exe` (`app/src/core`) | `bluecaster-core.exe`: núcleo, bandeja (`tray-icon` sobre un bucle de eventos `tao`), servidor IPC, `--cli` |
| 5. Programas | `bluecaster-ui` (`app/src/ui`) | `bluecaster.exe`: Tauri 2; la parte de Rust es un cliente IPC, la página es SolidJS + TypeScript + Vite |
| 5. Programas | `bluecaster-driver` (`app/src/driver`) | `bluecaster-driver.exe`: el auxiliar elevado |

## Ruta del audio e hilos {#audio-path-and-threads}

```mermaid
---
config:
  flowchart:
    wrappingWidth: 480
    rankSpacing: 30
---
flowchart TD
    out["Salida de audio del sistema<br>(loopback / dispositivo virtual / una aplicación)"]
    out --> cap["hilo capture:<br>WASAPI, controlado por eventos<br>(normalmente float32, 44.1–96 kHz)"]
    cap -->|"búfer circular<br>sin bloqueos (PCM)"| enc["hilo encode:<br>conversión, mezcla descendente, codificador,<br>carga útil multimedia + encabezado RTP, ABR"]
    enc -->|"búfer circular sin<br>bloqueos (paquetes)"| stack["hilo bt-runloop: BTstack → AVDTP → L2CAP → HCI"]
    stack --> usb["hci-transport → WinUSB → adaptador Bluetooth USB → radio"]
    usb -.-> hp["Auriculares / altavoces"]
```

| Hilo | Prioridad | Trabajo |
|:-------|:---------|:-----|
| capture | MMCSS "Audio" | Lee WASAPI; rellena con silencio los tramos sin sonido |
| encode | MMCSS "Pro Audio" | Convierte el PCM al formato de entrada del codificador, codifica bloques completos del codificador (el resto espera a la siguiente ronda) y empaqueta las tramas en cargas útiles multimedia, en rondas de unos 10 ms para que los paquetes salgan de forma regular |
| bt-runloop | -- | El bucle de ejecución de BTstack; envía un paquete en cola cada vez que L2CAP puede enviar. Cada vez que se pone un paquete en cola, la cola de envío (64 paquetes) descarta los paquetes que llevan 350 ms (100 ms con baja latencia de LHDC V5) esperando, empezando por el más antiguo |
| USB | -- | Transferencias WinUSB: tres lecturas en curso en cada canalización IN (eventos, datos ACL), un escritor para los comandos y otro para los datos ACL (hasta 8 transferencias en curso) |
| core | Normal | Máquina de estados, IPC (tokio; las colas del paso directo RFCOMM, por conexión), configuración, estadísticas (4 Hz), búsqueda de actualizaciones |

Una vez que la transmisión está en marcha, la ruta del audio no asigna memoria ni toma ningún bloqueo.
Las estadísticas son contadores atómicos que el núcleo lee cuatro veces por segundo.
BTstack no es seguro entre hilos, así que los demás hilos le envían comandos que se ejecutan en
el hilo bt-runloop.

## Núcleo e IPC {#core-and-ipc}

La ventana se comunica con el núcleo a través de la canalización con nombre del usuario
`\\.\pipe\bluecaster-<user SID>`, cuya DACL solo admite a ese usuario. Los mensajes son
tramas JSON precedidas de su longitud:

1. La ventana envía `hello { protocol, client }`. Un núcleo con otra
   versión de protocolo responde `reject`, y la ventana indica que los dos
   programas son de versiones distintas. Si no, el núcleo responde
   `welcome { protocol, server, features }`; `features` nombra las ampliaciones
   del protocolo (`rfcomm`).
2. El núcleo envía un `snapshot`: todo su estado (adaptador, dispositivos,
   transmisión, perfiles, configuración, actualización) y un número de revisión.
3. A partir de ahí, el núcleo envía `event`s con los cambios. Si a la ventana le falta una revisión,
   pide una instantánea nueva.
4. Las acciones son `request { id, method, params }` con una `response { id,
   result | error }`. Los grupos de métodos son `state.*`, `stream.*`,
   `device.*`, `profile.*`, `settings.*`, `adapter.*`, `audio.*`, `diag.*`,
   `receiver.*`, `rfcomm.*` y `app.*`.

Otros programas usan la misma canalización; consulte
[Integración (IPC)](integration). Las versiones anteriores a la 3.0.0 llamaban a la canalización
`\\.\pipe\a2dpwb-<user SID>`; el núcleo también atiende ese nombre durante al menos 6
meses después de la 3.0.0.

Los errores son códigos con parámetros (por ejemplo `adapter.driver_missing`), no
textos; la ventana los traduce y muestra la solución junto al mensaje.
El núcleo solo envía los datos de alta frecuencia (calidad del enlace, el registro) mientras una pantalla
está suscrita a ellos.

La máquina de estados de la conexión:

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Connecting: Iniciar
    Connecting --> Streaming: transmisión iniciada
    Connecting --> Idle: detenido
    Connecting --> Error: fallo de inicialización /<br>conexión / códec
    Streaming --> Reconnecting: conexión perdida
    Reconnecting --> Streaming: reconectado
    Reconnecting --> Error: fallo de reconexión
    Streaming --> Idle: Detener
    Error --> Connecting: Iniciar de nuevo
```

El núcleo intenta una conexión fallida hasta 3 veces. Un enlace perdido lo reconecta hasta
3 veces, o hasta 10 veces después de reiniciar la pila (suspensión, un adaptador
que dejó de funcionar). Cuando el PC entra en suspensión, el núcleo detiene BTstack sin contar
el enlace perdido como un fallo y vuelve a conectar cuando el PC se reanuda.

## Asignación del controlador {#driver-assignment}

Windows 8 y posteriores incluyen el controlador WinUSB firmado por Microsoft
(`winusb.inf`). `bluecaster-driver.exe` hace lo mismo que «Actualizar
controlador > Elegir en una lista > WinUsb Device» del Administrador de dispositivos, mediante SetupAPI:

1. Abre el adaptador por su ID de instancia de dispositivo.
2. Escribe un valor `DeviceInterfaceGUIDs` en sus `Device Parameters`, para que
   WinUSB registre una interfaz de dispositivo para él.
3. Selecciona el modelo «WinUsb Device» de `%SystemRoot%\INF\winusb.inf` y lo
   instala con `DiInstallDevice`. El controlador incluido en Windows está firmado, así que no se
   instala ningún certificado (a diferencia de Zadig / libwdi, que firman un INF generado
   con un certificado propio).
4. **Restaurar**: quita el dispositivo (`DiUninstallDevice`) y lo vuelve a enumerar;
   Windows elige entonces de nuevo su controlador mejor clasificado (normalmente `BthUSB` o el
   del fabricante). El controlador que tenía antes el adaptador se registra en la
   carpeta de configuración (`driver\`) y se compara con el resultado tras la restauración.
   `restore --records <folder>` restaura todos los adaptadores registrados en una sola
   ejecución elevada; el desinstalador lo usa después de preguntar al usuario.

El núcleo nunca se ejecuta como administrador: para una asignación o una restauración, inicia
el auxiliar con una solicitud de UAC (`ShellExecuteEx` "runas") y lee el resultado
JSON del auxiliar. Enumerar los adaptadores y sus controladores no requiere elevación.

```mermaid
sequenceDiagram
    participant Core as bluecaster-core
    participant Drv as bluecaster-driver (elevado)
    participant Win as Windows (SetupAPI)
    Core->>Drv: inicio con un aviso de UAC
    alt Asignar
        Drv->>Win: DeviceInterfaceGUIDs<br>en Device Parameters
        Drv->>Win: instalar winusb.inf<br>(WinUsb Device)
    else Restaurar
        Drv->>Win: quitar el dispositivo<br>y volver a enumerarlo
        Win->>Win: vuelve a elegir su<br>mejor controlador
    end
    Drv-->>Core: resultado (JSON)
```

## Módulos clave {#key-modules}

### Elección del códec (`codec::caps`)

- Analiza las capacidades del dispositivo remoto: los elementos estándar de SBC y AAC y los
  códecs de fabricante, incluido aptX LL con ambos Vendor ID y aptX Adaptive
  (receptores R1 y R2; solo utilizable con 48 kHz estéreo)
- Elige el códec: Automático toma el códec de mayor calidad de sonido que ofrece el
  remoto. Si el remoto no ofrece un códec solicitado, se sustituye por el códec
  que tomaría Automático (siempre para un perfil, desde el issue #86; en la
  línea de comandos solo con `--fallback`); si no, es un error. Un códec
  elegido en su tarjeta cuando el remoto no lo ofrecía (el `codec_forced`
  del perfil) se pide igualmente, sin alternativa (`RemoteCaps::forcing`).
  Un códec cuya tarjeta ofrece más de lo que anuncia el remoto
  (`unlisted_options`) se configura a partir de `RemoteCaps::unlisted`
- Mientras el ajuste `auto_codec` está activado (Códecs > Seleccionar automáticamente el códec compatible de mayor
  calidad; un único ajuste para todos los dispositivos desde el esquema 5), transmite cada perfil A2DP
  como Automático, con los ajustes predeterminados de Automático. El códec
  elegido para el dispositivo se conserva en su perfil para cuando el ajuste esté desactivado
- Sigue el orden de códecs que el usuario fijó arrastrando las tarjetas del dispositivo
  (el `codec_order` del perfil; `codec::AutoOrder`, vacío
  para mayor calidad de sonido primero), tanto para la elección de Automático como para la alternativa de
  un códec elegido, también con la selección automática. FastStream y Opus
  05 nunca son la elección de Automático ni una alternativa. (Las betas de 2.0.2 tenían un orden para
  todos los dispositivos, el `auto_order` de settings.json, que ahora se ignora.)
- Elige la frecuencia de muestreo de la familia aptX (44.1 / 48 kHz entre las que
  anuncia el remoto; WASAPI remuestrea la captura); aptX Adaptive usa 48 kHz, o
  96 kHz cuando se eligen 96 kHz, la calidad es Alta y el receptor es R2 y
  anuncia 96 kHz. Una frecuencia en Automático (también con la selección automática) pide 48 kHz; si el
  receptor no la anuncia, el selector de cada códec toma la mejor que anuncia el receptor (issue #86: no
  la frecuencia más alta que anuncia ni la frecuencia de mezcla de Windows)
- Asigna la calidad del perfil a las que ofrece el códec
  (`Quality::for_codec`, issue #86): LDAC y LHDC V5 Alta / Media
  (`sq`) / Baja (`mq`), LHDC V5 también Media-alta (`mhq`), aptX Adaptive
  Alta / Baja latencia, todos los demás códecs Alta (la tasa de bits más alta que acepta el
  dispositivo). Una calidad que el códec ya no tiene (Estándar /
  Móvil de aptX Adaptive, los niveles inferiores de AAC) funciona como Alta
- Registra lo que anuncia cada códec que ofreció el dispositivo (`KnownCodecs.caps`:
  frecuencias de muestreo, profundidades de bits, tasa de bits máxima; `RemoteCaps::listed_*`), para que
  los ajustes del códec solo ofrezcan esos valores; el rango de bitpool de SBC ya se registraba

### Host Bluetooth (`bt-host`)

**Responsabilidades**:
- Ejecutar BTstack con el transporte HCI USB (o TCP) en su propio hilo
- Registrar los extremos de transmisión: códecs de fabricante (LDAC, aptX HD, aptX, aptX LL,
  LHDC V5, FastStream, Opus, Opus 05, aptX Adaptive) además de SBC (que también usa
  SBC XQ) y AAC: 11 en total (`MAX_NR_AVDTP_STREAM_ENDPOINTS` de BTstack)
- Registrar los registros SDP (A2DP Source, AVRCP Controller, AVRCP Target; HFP
  Audio Gateway, solo para el nivel de batería, mientras ese ajuste
  está activado)
- Gestionar el emparejamiento SSP (Just Works, General Bonding; claves de enlace persistentes)
- Cerrar una conexión semiabierta tras un tiempo de espera de conexión, para que los reintentos funcionen
- Reintentar una conexión fallida (hasta 3 intentos) y distinguir las causas (sin
  respuesta, enlace sin datos, el remoto cerró el enlace, autenticación); descartar una clave de enlace
  que el dispositivo ya no reconoce y volver a emparejar
- GAP inquiry para la búsqueda de dispositivos
- Volumen absoluto AVRCP y los botones de los auriculares
- Clasificación de canales AFH del host y lecturas de RSSI / AFH para la vista
  de calidad del enlace
- Carga de firmware: Realtek y Broadcom mediante los controladores de chipset de BTstack,
  Intel (bootloaders heredado y TLV) en Rust sobre la interfaz común de cargadores de chips
  (`bringup`); la pantalla Adaptador muestra el progreso de la carga mientras se inicia el
  adaptador
- Modo de desarrollador: la lectura de datos del chip (lo que un adaptador responde a comandos de lectura
  estándar, un informe JSON en `logs\probe\`), los scripts de chip (solicitudes escritas
  como texto, ejecutadas en un adaptador con WinUSB) y los servicios SDP que anuncia un
  dispositivo conectado
- Captura HCI (`.pklg`), incluido el establecimiento de la conexión cuando la captura empieza con la conexión ya en curso

**API clave de BTstack utilizadas**:
- `a2dp_source_create_stream_endpoint()` — Registrar los extremos de códec
- `a2dp_source_establish_stream()` — Conectar con el receptor A2DP
- `a2dp_source_set_config_other()` — Configurar un códec específico del fabricante
- `a2dp_source_stream_send_media_payload_rtp()` — Enviar audio codificado con
  encabezado RTP (LDAC, aptX HD, AAC, SBC, SBC XQ, LHDC V5, Opus)
- `a2dp_source_stream_send_media_packet()` — Enviar paquetes multimedia sin procesar, sin
  encabezado RTP (aptX, aptX LL, FastStream), o con el encabezado RTP propio de BlueCaster
  (aptX Adaptive)

### Captura de audio (`audio-capture`)

Captura el audio en tiempo real mediante Windows Audio Session API.
- Loopback del sistema del dispositivo de salida predeterminado, loopback de un dispositivo
  (virtual) elegido, o loopback de proceso de una aplicación y sus procesos
  secundarios (Windows 10 compilación 19041 o posterior)
- Entrega el formato de mezcla del modo compartido del dispositivo (normalmente float32; el
  pipeline lo convierte para el codificador)
- En el modo Aplicación, espera a que se inicie la aplicación y vuelve a capturarla
  tras un reinicio
- Ajusta el volumen y el silencio de una aplicación en la salida predeterminada, como hace el mezclador
  de volumen de Windows (`mixer`): el volumen de una sesión se aplica a la captura loopback, incluso
  mientras el núcleo silencia esa salida durante una transmisión con Loopback del sistema
  (`bluecaster-core::mute_output`)

### Protocolos de fabricante (`bluecaster-core::vendor`)

Algunos auriculares solo ofrecen un códec o una tasa de bits después de que la app del fabricante
cambie un modo a través del servicio RFCOMM propio de los auriculares. Un módulo por fabricante lo hace a través del
enlace de BlueCaster y reconoce los auriculares por su nombre:

- **Huawei / Honor**: cuando un perfil pide LDAC y los auriculares no lo
  anuncian, el módulo activa su modo «prioridad a la calidad de sonido» y se reconecta;
  si así no se obtiene LDAC, vuelve a desactivar el modo en los auriculares que lo tenían desactivado. Nunca
  con Automático
- **OPPO**: cuando LHDC V5 transmite (elegido o por Automático), el módulo pide Hi-Res a los
  auriculares una vez y se reconecta una vez para leer lo que anuncian entonces; esto
  se registra con los códecs del dispositivo (`device_codecs.json`) hasta que
  `device.forget_codecs` lo borra

La pestaña Fabricantes del modo de desarrollador fija cuándo se ejecuta cada módulo (Automático por nombre,
Siempre, Nunca) y ofrece las opciones propias del fabricante.

### Emparejamientos (`link_keys`, `win-sys::bt_keys`)

Las claves de enlace se guardan en `link_keys.txt`. Al añadir dispositivos, el núcleo puede
importar las claves que el Bluetooth de Windows guardó para el adaptador que BlueCaster usa ahora:
`bluecaster-driver.exe windows-keys` las lee (una solicitud de UAC). Las claves que BlueCaster
ya tiene se conservan, y los dispositivos de audio entre las importadas se guardan con sus
nombres de Windows. El modo de desarrollador muestra la clave de enlace de un dispositivo y los servicios
SDP que anuncia.

Los archivos de claves de `pairing\` (claves de enlace, vínculos LE, conjuntos
coordinados, claves LE tomadas de Windows, claves de Windows reemplazadas) se
cifran con DPAPI para el usuario de Windows (`win-sys::dpapi`); otros
usuarios no pueden leerlos, ni tampoco otro PC al que se copien. Los
archivos en texto plano de versiones anteriores se leen y se reescriben
cifrados. Un archivo que no se puede descifrar se conserva como
`<nombre>.undecryptable` y sus dispositivos se emparejan de nuevo. BlueCaster
guarda por su cuenta el TLV LE de BTstack (el `TlvStore` de `btstack-sys`, con el
contenido que escribía `btstack_tlv_windows`). `keys_for_windows.json`, que el
auxiliar elevado lee durante una restauración, sigue en texto plano
(el auxiliar puede ejecutarse como otro administrador) y se borra cuando
el auxiliar termina.

### Codificadores de audio {#audio-encoders}

| Codificador | Biblioteca | Tasa de bits | Características |
|---------|---------|---------|----------|
| LHDC V5 | lhdcv5-enc (port en C del codificador de AOSP) | 400/500/900/1000 kbps (Baja/Media/Media-alta/Alta), como máximo el máximo que anuncia el receptor, ABR 160–400 kbps | 16/24 bits, tramas de 5 ms, encabezado de carga útil multimedia de 2 bytes, encabezado RTP; baja latencia si el receptor la anuncia (codifica cada 10 ms en lugar de cada 20; con ABR desactivado, añade el control de la cola descrito abajo) |
| LDAC | libldac (AOSP) | 330/660/990 kbps | Modos Alta/Media/Baja (HQ/SQ/MQ), ABR |
| aptX Adaptive | aptx-adaptive (codificador en Rust puro de AKS-Logic/aptx-adaptive-linux, MIT) | 427 kbps en Alta, 265 kbps en Baja latencia (receptores R2: 270); ABR baja a 364/318 (R2: 270/204) | 48 kHz estéreo (96 kHz en Alta cuando se eligen, a un receptor R2 que los anuncia: 646/340/255 kbps), 24 bits; un paquete del códec por paquete multimedia tras el encabezado RTP propio de BlueCaster; ABR en Alta; Automático lo toma después de LHDC V5 y LDAC |
| Opus | libopus 1.6.1 | 256 kbps (estéreo; mono la mitad) | Opus A2DP de Android: 48 kHz, tramas de 20 ms (10 ms si el receptor solo admite eso), VBR restringida limitada a la carga útil multimedia, una trama por paquete, encabezado de carga útil multimedia de 1 byte, encabezado RTP |
| aptX HD | libopenaptx | 576 kbps | Entrada de 24 bits, tasa fija, encabezado RTP |
| aptX | libopenaptx | 352/384 kbps (44.1/48 kHz) | Estéreo de 16 bits, sin encabezado RTP |
| aptX LL | libopenaptx | 352 kbps | Latencia de ~32 ms, sin encabezado RTP, paquetes ≤ ~7.5 ms |
| AAC | fdk-aac | hasta 256 kbps (como máximo la tasa de bits que anuncia el receptor) | AAC-LC, transporte LATM |
| SBC XQ | el mismo codificador SBC | ~385/505/552 kbps | Canal dual, bitpool 47 / 43 a 44.1 / 48 kHz (Alta); solo cuando el SBC del receptor admite canal dual y un bitpool de al menos 29 (el bitpool se limita entonces al máximo del receptor, como PipeWire); comparte con SBC el extremo de transmisión, el encabezado de carga útil multimedia y el encabezado RTP |
| SBC | Port en Rust del codificador de Bluedroid | hasta ~345 kbps | Base obligatoria de A2DP; idéntico bit a bit al codificador en C (probado en todas las configuraciones) |
| FastStream | el mismo codificador SBC | 198/216 kbps (44.1/48 kHz) | Códec de fabricante de CSR, solo en la dirección de música (sin canal de retorno de voz): SBC joint stereo con bitpool 29, tramas rellenadas hasta 72 bytes, 3 por paquete, sin encabezado de carga útil multimedia ni encabezado RTP (como PipeWire); Automático nunca lo elige |

**Control de la cola de LHDC V5** (baja latencia activada, ABR desactivado):
mientras la cola de envío tiene de media 12 paquetes o más durante un segundo,
el codificador baja un nivel (1000, 900 y después 500 kbps), con al menos 2 s
entre cambios. Tras un minuto sin problemas, vuelve a subir un nivel. Si una
subida no se mantiene 30 s, la espera se duplica, hasta un máximo de 10 minutos.

Todos los codificadores implementan el trait `codec::Encoder`: indican el formato PCM
y cuántas tramas consume una llamada, y devuelven tramas del códec.

libopenaptx espera grupos de 4 muestras estéreo como PCM de 24 bits empaquetado
little-endian; los envoltorios de aptX convierten las muestras de 16 o 32 bits a ese formato,
y la entrada mono se duplica en ambos canales.

## Elementos de información de códecs de fabricante {#vendor-codec-information-elements}

Los códecs no estándar se registran como Vendor Specific en AVDTP:

| Códec | Vendor ID | Codec ID |
|-------|-----------|----------|
| LHDC V5 | Savitech (0x0000053A) | 0x4C35 |
| LDAC | Sony (0x0000012D) | 0x00AA |
| aptX Adaptive | Qualcomm (0x000000D7) | 0x00AD |
| Opus | Google (0x000000E0) | 0x0001 |
| aptX HD | Qualcomm (0x000000D7) | 0x0024 |
| aptX | APT (0x0000004F) | 0x0001 |
| aptX Low Latency | CSR (0x0000000A) o Qualcomm (0x000000D7) | 0x0002 |
| FastStream | CSR (0x0000000A) | 0x0001 |
| Opus 05 | The Linux Foundation (0x000005F1, PipeWire) | 0x1005 |

AAC y SBC usan los Codec ID estándar de A2DP definidos en la especificación de A2DP.

Tamaños de los elementos de información de códec (incluidos los 6 bytes de Vendor ID/Codec ID):

- **aptX**: 7 bytes (byte 6 = frecuencia de muestreo / modo de canal)
- **aptX HD**: 11 bytes (byte 6 como en aptX, más 4 bytes reservados); se requiere
  estéreo
- **aptX LL**: 8 bytes, o 17 bytes cuando el receptor activa el indicador extendido
  («new caps»). BlueCaster registra un único extremo aptX LL y configura el
  flujo con el Vendor ID que usó el receptor
- **LHDC V5**: 11 bytes (`a2dp_vendor_lhdcv5_constants.h` de Android): byte 6
  frecuencias de muestreo (0x20 44.1k, 0x10 48k, 0x04 96k, 0x01 192k), byte 7 profundidad de bits
  (0x04 16 bits, 0x02 24 bits) y los límites mínimo/máximo de tasa de bits del receptor, byte 8
  subversión del códec (0x01 = V5 ver.1) y 0x10 = tramas de 5 ms, bytes 9-10
  funciones opcionales (baja latencia, sin pérdida, JAS, AR, META). BlueCaster pide
  baja latencia (byte 9, 0x40) cuando la baja latencia está activada en los ajustes
  de LHDC V5 y el receptor la anuncia. BlueCaster no ofrece ninguna de las demás
  funciones. Solo estéreo. Los límites de tasa de bits del receptor se devuelven en la
  configuración y se aplican al codificador, como hace Android. Los paquetes multimedia
  llevan un encabezado de carga útil de 2 bytes: número de tramas << 2 (bits de latencia 0) y un
  número de secuencia
- **FastStream**: 8 bytes (`a2dp_faststream_t` de PipeWire): byte 6 dirección
  (0x01 sink = música, 0x02 source = voz), byte 7 en el nibble bajo las frecuencias del receptor
  (0x2 44.1k, 0x1 48k), en el nibble alto la frecuencia de la fuente (0x2 16k). BlueCaster ofrece
  y configura solo la dirección de música
- **Opus**: 7 bytes (`a2dp_vendor_opus_constants.h` de Android): byte 6 bits
  2..0 modo de canal (0x01 mono, 0x02 estéreo, 0x04 mono dual), bits 4..3
  duración de trama (0x08 10 ms, 0x10 20 ms), bit 7 48 kHz. BlueCaster ofrece mono
  y estéreo, 10 y 20 ms, y configura estéreo a 20 ms (mono solo para una entrada
  mono hacia un receptor que lo admita). Los paquetes multimedia llevan un paquete Opus tras
  un encabezado de carga útil de 1 byte (número de tramas 1 en los 4 bits bajos)
- **aptX Adaptive**: 40 bytes. El evento de capacidades de BTstack recorta la
  información del códec del receptor a 30 bytes, que siguen conteniendo todo lo que usa BlueCaster.
  SET_CONFIGURATION: 48 kHz con tipo de fuente 2 (codificador en el host: el receptor
  temporiza la reproducción por sí mismo), estéreo; un receptor R1 recibe de vuelta sus propios
  bytes de latencia objetivo, con LL0 elevada a al menos 80 ms y una latencia HQ de cero
  sustituida por 200 ms; un receptor R2 recibe el bloque R2 propio de la fuente

**Con o sin RTP**: los paquetes multimedia de LDAC, aptX HD, AAC, SBC, SBC XQ, LHDC V5 y Opus llevan el
encabezado RTP de 12 bytes. aptX, aptX LL y FastStream se envían **sin** encabezado RTP (como hacen Android y
PipeWire), de modo que toda la MTU de L2CAP queda disponible para las tramas aptX.

**aptX Adaptive** envía un paquete del códec por paquete multimedia
tras el encabezado RTP de 12 bytes propio de BlueCaster (BTstack no añade ninguno): tipo de carga útil 96,
número de secuencia +1 por paquete, marca de tiempo = las muestras por canal del
paquete (no un reloj), SSRC = el modo de latencia del receptor (0x000000A3 HQ,
0x000000A1 LL). La carga útil lleva los bytes intercambiados en palabras de 16 bits. Los paquetes HQ tienen
656 bytes (12 ms) y los LL 348 bytes (10 ms). Automático lo coloca por encima de
aptX clásico, Opus y AAC y por debajo de LHDC V5 y LDAC (el orden del issue #86).

## Configuración y archivos {#settings-and-files}

Todo se guarda en la carpeta de configuración, `%APPDATA%\BlueCaster` (o la
carpeta indicada por la variable de entorno `BLUECASTER_CONFIG_DIR`). Los archivos de
la v1 y la 2.0.x conservan sus formatos; la 2.0.x usaba `%APPDATA%\A2DPWB`; en su primer inicio, la 3.0.0
cambia el nombre de esa carpeta y ordena los archivos en estas carpetas:

| Ruta | Contenido |
|:-----|:---------|
| `settings.json` | Configuración de la aplicación, con un `schema_version`; las claves desconocidas se conservan |
| `profiles.json` | Perfiles |
| `devices\devices.txt` | Dispositivos conocidos |
| `devices\le_audio_devices.txt` | LE Audio: las entradas LE Audio de la lista de dispositivos |
| `devices\le_audio_sets.txt` | LE Audio: los conjuntos coordinados de los dispositivos guardados (solo las direcciones de sus miembros; los dos auriculares de un par son un solo dispositivo) |
| `devices\device_codecs.json` | Lo que ofreció cada dispositivo clásico en su última conexión (y si se pidió Hi-Res a unos auriculares OPPO) |
| `pairing\link_keys.txt` | Claves de enlace: los auriculares emparejados con la v1 o la 2.0.x se reconectan sin volver a emparejar |
| `pairing\le_audio_bonds.tlv`, `pairing\le_audio_csip.txt` | LE Audio: los vínculos LE (con los valores CCC persistentes del servidor GATT) y los conjuntos coordinados |
| `pairing\windows_le_keys.txt`, `pairing\replaced_windows_keys.json` | Claves LE tomadas de Windows hasta que una pila con LE las añade; las claves de Windows que reemplazó **Restaurar el controlador original** |
| `adapters\adapter_caps.json` | Lo que una pila averiguó de cada adaptador (compatibilidad con LE Audio, dirección, versión HCI, el nombre que le da Windows) |
| `adapters\driver\<instance id>.json` | El controlador anterior de cada adaptador al que se asignó WinUSB |
| `firmware\` | Archivos de firmware (`*.bin`, `*.hcd`, `ibt-*`; los de Intel también en `firmware\intel\`, como los organiza linux-firmware); los archivos que versiones anteriores dejaban sueltos en la carpeta de configuración se mueven aquí |
| `logs\<YYYY-MM-DD>\core.log`, `ui.log`, `driver.log` | Los registros del núcleo, de la ventana y del auxiliar del controlador, una carpeta por día (UTC); sin enmascarar |
| `logs\hci\*.pklg` | Capturas HCI: las del registrador de vuelo, escritas cuando falla una transmisión o se pierde una conexión (las 10 más recientes), y las que inicia usted mismo (`hci_<date>_<time>.pklg`) |
| `logs\probe\` | Informes de la lectura de datos del chip y de los scripts de chip (los 10 más recientes) |
| `logs\crash\` | Informes de bloqueo: un minivolcado (`*.dmp`) y un archivo de texto por bloqueo (los 5 más recientes por programa) |
| `logs\bundles\*.zip` | Paquetes de diagnóstico (los 3 más recientes) |
| `backup\v1\` | Una copia de la configuración de la v1, creada una sola vez en el primer inicio de la v2 |
| `backup\v2\` | Una copia de la configuración y los emparejamientos de la 2.0.x, creada una sola vez en el primer inicio de la 3.0.0 |
| `backup\profiles-before-codecs.json` | `profiles.json` tal como estaba antes de que la pantalla Códecs integrara varios perfiles de un dispositivo en uno (se escribe una vez) |

La carpeta del programa es `%LOCALAPPDATA%\BlueCaster\`: los tres programas, el
desinstalador y `licenses\` (`licenses\README.txt`, `licenses\bluecaster\LICENSE...`,
`licenses\third-party\...`). La caché de descarga del firmware es `%TEMP%\bluecaster-cache`.

## Sistema de compilación {#build-system}

- **Cargo** compila los programas; no necesitan CMake. Cada crate `-sys`
  compila su biblioteca en C desde el submódulo git de `extern/` con MSVC en
  `build.rs`. Solo la herramienta para desarrolladores `tools/a2dp_decode` se compila con CMake
- La página de la ventana se compila con **Vite** (`pnpm build` en `app/src/ui`) y
  Tauri la incrusta en `bluecaster.exe`
- Los tipos de TypeScript del protocolo IPC se generan a partir de los tipos de Rust
  (`cargo test -p ipc export_bindings`)
- La ventana y el menú de la bandeja del núcleo usan los mismos archivos de idioma
  (`app/lang/*.json`)
- `installer/build-release.ps1` genera el instalador (Inno Setup, `installer/BlueCaster.iss`),
  el zip portable y el manifiesto del actualizador. Consulte [Compilación](building)

## Consideraciones importantes {#important-considerations}

- **Compatibilidad de adaptadores**: Realtek (los más probados), CSR, Intel, Broadcom, MediaTek
  y otros ([Hardware verificado](hardware)). Los adaptadores Realtek, y los Intel en modo bootloader,
  necesitan cargar el firmware al iniciar
- **Emparejamiento**: BlueCaster usa SSP Just Works con General Bonding. Las claves de enlace se guardan
  en un archivo local o se importan del Bluetooth de Windows para el adaptador
- **Se recomienda un segundo adaptador**: mantenga el Bluetooth integrado para Windows y use
  un adaptador USB dedicado para BlueCaster
- El firmware de algunos adaptadores Bluetooth limita la tasa de bits alcanzable
- Por lo general, los adaptadores USB Bluetooth 5.0+ funcionan bien con LDAC
- Prioridad de la selección automática de códec (por calidad de sonido, issue #86): LHDC V5 > LDAC > aptX Adaptive > Opus > aptX HD > aptX > aptX LL > AAC > SBC XQ > SBC (FastStream y Opus 05 solo si se eligen); un códec elegido que el dispositivo ya no ofrece se sustituye por el primer códec del orden del dispositivo que el dispositivo ofrezca; uno elegido en su tarjeta cuando el dispositivo no lo ofrecía se pide igualmente (`codec_forced`, sin alternativa)

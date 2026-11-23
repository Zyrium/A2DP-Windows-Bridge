---
title: Instalación
layout: default
parent: Inicio
nav_order: 1
---

# Instalación
{: .no_toc }

## Contenido
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Requisitos previos

- **Windows 10 (22H2) / 11** (x64 o ARM64)
- **Un adaptador Bluetooth USB para BlueCaster**: se recomienda un adaptador aparte; un adaptador USB integrado también sirve, pero entonces Windows se queda sin Bluetooth propio (consulte el [Paso 2](#winusb))

BlueCaster asigna automáticamente el controlador WinUSB al adaptador, así que ya no necesita [Zadig](https://zadig.akeo.ie/) (sigue funcionando; consulte [Usar Zadig en su lugar](#zadig)).

## Instalar BlueCaster

Cada [versión](https://github.com/SeiyaFunaokaJP/BlueCaster/releases) se publica en dos formatos con los mismos archivos:

- **Instalador** (`BlueCaster-<version>-win64-setup.exe`, en ARM64 `-arm64-setup.exe`): instala para el usuario actual sin derechos de administrador, añade una entrada en el menú Inicio (**BlueCaster**) y un desinstalador, e instala Microsoft Edge WebView2 Runtime si falta (Windows 11 ya lo incluye). Las actualizaciones se instalan desde la propia aplicación (**Configuración > Actualizaciones > Actualizar ahora**). Los programas se instalan en `%LOCALAPPDATA%\BlueCaster\` (los tres programas, el desinstalador y una carpeta `licenses\` con los textos de licencia)
- **Portable** (`BlueCaster-<version>-win64-portable.zip`, en ARM64 `-arm64-portable.zip`): extráigalo en cualquier carpeta y ejecute `bluecaster.exe`. Mantenga juntos los tres programas (`bluecaster.exe`, `bluecaster-core.exe`, `bluecaster-driver.exe`). La copia portable solo avisa de las actualizaciones y necesita WebView2 Runtime

Ambos guardan la configuración, los perfiles, las claves de enlace y el firmware en `%APPDATA%\BlueCaster`, así que puede cambiar de uno a otro. La desinstalación elimina esa carpeta, salvo que desactive **Eliminar los datos de usuario** (una desinstalación silenciosa la conserva).

### Actualizar desde la v1 {#from-v1}

Ejecute el instalador sobre la v1. Su página **Listo para Instalar** indica qué versiones anteriores desinstalará primero (la v1, o cualquier 2.0.x). Si la v1 está en ejecución, el instalador le pide que la cierre; después desinstala la v1, pero conserva la carpeta de configuración. Los perfiles, los emparejamientos con los auriculares y los archivos de firmware se mantienen, así que no hace falta volver a emparejar los auriculares. Un adaptador que ya tiene WinUSB (instalado con Zadig) lo conserva. Si **Iniciar con Windows** estaba activado, Windows inicia ahora la v2.

En su primer inicio, la v2 copia la configuración de la v1 en `%APPDATA%\BlueCaster\backup\v1\` antes de leerla.

### Actualizar desde la 2.0.x (A2DP Windows Bridge) {#from-2x}

BlueCaster 3.0.0 es el nuevo nombre de A2DP Windows Bridge. Ejecute el instalador de la 3.0.0 sobre la 2.0.x instalada: el instalador quita automáticamente la versión anterior y conserva la configuración. La entrada del menú Inicio y la de **Iniciar con Windows** pasan a llamarse «BlueCaster».

En el primer inicio, la 3.0.0 cambia el nombre de la carpeta `%APPDATA%\A2DPWB` a `%APPDATA%\BlueCaster` y ordena los archivos en subcarpetas (consulte [Configuración y archivos](architecture#settings-and-files)). `backup\v2\` guarda una copia de la configuración y los emparejamientos de la 2.0.x por si quiere volver a la 2.0.2: copie esos archivos de nuevo en `%APPDATA%\A2DPWB`.

Los auriculares pueden seguir mostrando el antiguo nombre Bluetooth «A2DPWB» hasta que los vuelva a emparejar.

## Paso 1: Configuración inicial {#first-run}

Conecte el adaptador Bluetooth USB e inicie BlueCaster. En el primer inicio (aún sin dispositivo de audio, o sin ningún adaptador con WinUSB), BlueCaster abre el **Asistente de configuración**, de cuatro pasos. Una vez que lo termina u omite, el asistente no vuelve a abrirse automáticamente; añada los dispositivos en **Inicio** o en **Dispositivos de audio**.

1. **Elegir el adaptador**: muestra los adaptadores Bluetooth USB encontrados. Si falta el suyo, conéctelo y pulse **Actualizar**. Una etiqueta verde **Compatible** o roja **No compatible** indica si BlueCaster puede usar cada adaptador; los no compatibles aparecen en la lista, pero no se pueden elegir
2. **Controlador y firmware**: el paso indica lo que va a hacer, y **Preparar este adaptador** lo lleva a cabo: BlueCaster reemplaza el controlador del adaptador por WinUSB (Windows solicita permiso de administrador; consulte el [Paso 2](#winusb)) y descarga los archivos de firmware de Realtek que necesite el adaptador (consulte [Firmware](#firmware)). El firmware que BlueCaster no puede descargar debe copiarse a mano en la carpeta de firmware. Si Windows tiene dispositivos emparejados a través de este adaptador (por ejemplo, un teclado o un ratón en un adaptador integrado), el botón los muestra primero: dejan de funcionar en Windows mientras BlueCaster usa el adaptador, y **Cancelar** los mantiene en funcionamiento. Los dispositivos con su propio receptor USB no se ven afectados; para conservar los demás, use BlueCaster con un adaptador USB aparte. **Reemplazar por el controlador WinUSB**, en la pantalla **Adaptador**, también los muestra
3. **Encontrar los dispositivos de audio**: la búsqueda empieza automáticamente. Ponga el dispositivo de audio en modo de emparejamiento y pulse **Conectar** junto a él (o escriba su dirección y pulse **Usar**): BlueCaster guarda el dispositivo con el mejor códec que admite (Automático) y se conecta. Después, el dispositivo aparece en **Dispositivos emparejados** con un punto de estado, y la búsqueda se detiene. **Añadir un dispositivo** lo desconecta y reanuda la búsqueda de otro dispositivo; el primero sigue guardado, y allí puede olvidar uno equivocado. **Importar emparejamientos de Windows** aparece cuando Windows ha emparejado dispositivos a través de este adaptador: su botón **Importar** los muestra, todos marcados (consulte el [Paso 3](#headphones)). **Siguiente** funciona también sin dispositivo; puede añadir dispositivos de audio más tarde en **Dispositivos de audio**. Los dispositivos nuevos capturan el **Hi-Fi Cable** si está instalado y, si no, VB-CABLE. El Hi-Fi Cable añade el menor retardo, así que se recomienda para el sonido, y VB-CABLE para el micrófono de los auriculares. Si falta un cable, el paso muestra un aviso, y su **Más información** muestra ambos cables: **Instalar** de VB-CABLE descarga el paquete de VB-CABLE de VB-Audio (1,3 MB) y lo instala (con una solicitud de permisos de administrador; si no aparece, reinicie Windows), y **Descargar** del Hi-Fi Cable abre la página de VB-Audio. Un cable instalado muestra **Desinstalar** en su lugar. Ambos cables son donationware de VB-Audio ([vb-cable.com](https://vb-cable.com/)), como indica **Acerca de**: BlueCaster no incluye VB-CABLE, y las donaciones van a VB-Audio. VB-Audio pide a empresas y otras organizaciones una licencia por instalación ([licencias](https://vb-audio.com/Services/licensing.htm)). Cuando BlueCaster encuentra un cable por primera vez más adelante, los dispositivos con **Dispositivo predeterminado** pasan a capturarlo (VB-CABLE solo mientras el Hi-Fi Cable no esté instalado), y los dispositivos cuyo micrófono no tiene una salida que funcione envían el micrófono allí. Puede cambiar el códec más tarde en **Códecs**
4. **Configuración general**: el tema, **Inicio**, **Notificaciones** y la comprobación de actualizaciones, los mismos controles que en Configuración, que se guardan al instante. La primera configuración activa **Iniciar con Windows** e **Iniciar en la bandeja**

**Configurar más tarde** omite el resto; **Configuración > Ejecutar la configuración inicial** vuelve a abrir el asistente.

## Paso 2: Asignar WinUSB al adaptador {#winusb}

BlueCaster se comunica directamente con un adaptador Bluetooth USB mediante WinUSB, el controlador USB genérico incluido en Windows. Se recomienda un **adaptador dedicado**: así el Bluetooth integrado sigue funcionando con normalidad en Windows.

1. Abra la pantalla **Adaptador** (el paso **Controlador y firmware** de la configuración inicial hace los pasos 2 a 4 con **Preparar este adaptador**)
2. El **Controlador** del adaptador indica **Controlador original** (o **Sin controlador**): el controlador que instaló Windows o el fabricante, que WinUSB reemplaza. Haga clic en **Reemplazar por el controlador WinUSB**
3. Lea la confirmación y continúe: Windows solicita permiso de administrador (UAC)
4. El controlador indica ahora **Controlador WinUSB**

Si Windows no puede cargar WinUSB en ese momento (porque el adaptador está en uso), BlueCaster se lo indica y la línea del controlador muestra **Reconecte o reinicie Windows**. Desconecte el adaptador y vuelva a conectarlo; si es un adaptador integrado, reinicie Windows. Hasta entonces, la conexión falla con `failed to initialize WinUSB (error 50)` en el registro.

{: .warning }
Mientras WinUSB esté asignado, el Bluetooth de Windows ya no ve el adaptador: los dispositivos emparejados con Windows a través de él dejan de funcionar hasta que se restaure el controlador. Use el Bluetooth integrado para los periféricos habituales (teclado, ratón, etc.). También puede ceder a BlueCaster un adaptador integrado que sea un dispositivo USB (uno de Realtek en muchos portátiles), pero entonces **el PC se queda sin Bluetooth de Windows** (configuración de Bluetooth, ratones, teclados, otros dispositivos de audio) hasta que restaure su controlador.

**Restaurar el controlador original**, en la misma pantalla, devuelve el adaptador a Windows: Windows vuelve a elegir su propio controlador (normalmente su controlador Bluetooth) y el adaptador funciona con el Bluetooth de Windows. BlueCaster no podrá transmitir a través de él hasta que vuelva a asignar WinUSB. Windows usa un solo adaptador Bluetooth a la vez. Si hay otro adaptador activo, BlueCaster se lo indica tras la restauración; para usar este adaptador en Windows, deshabilite el otro en el Administrador de dispositivos y vuelva a conectar este. Los auriculares que BlueCaster volvió a emparejar tienen una clave nueva. Para los auriculares que Windows tenía emparejados a través de este adaptador, la restauración pasa la clave nueva a Windows, así que se conectan allí sin volver a emparejarlos. **Limitación:** solo se transfieren los emparejamientos clásicos, no las claves LE. Un dispositivo que Windows usa por LE (LE Audio) debe volver a emparejarse en Windows (el modo de desarrollador puede intentar transferir las claves LE: **Desarrollador > Depuración**). El desinstalador ofrece lo mismo: una página del asistente de desinstalación tiene **Restaurar el controlador Bluetooth original** (cuando BlueCaster asignó WinUSB a algún adaptador; una sola solicitud de permisos de administrador) y **Eliminar los datos de usuario** (la configuración, los códecs, los emparejamientos y el firmware de `%APPDATA%\BlueCaster`, y lo que recuerda la ventana, como **Mostrar solo los códecs compatibles**), ambas activadas de forma predeterminada. Si los únicos adaptadores con WinUSB se configuraron con Zadig o con la v1 (así que BlueCaster no tiene registro de ellos), la primera opción aparece desactivada, y si la activa, Windows elige su controlador. Las actualizaciones mantienen WinUSB, igual que una desinstalación silenciosa (`/S`). La misma página ofrece **Quitar VB-CABLE** y **Quitar Hi-Fi Cable** cuando están instalados, desactivadas de forma predeterminada porque otras aplicaciones pueden usarlos. Se quitan sin la ventana del instalador de VB-Audio, tras una solicitud de permisos de administrador de Windows. Después, Windows busca cambios de hardware para que el cable desaparezca de la lista de Sonido; solo se ofrece reiniciar si sigue en la lista.

### Usar Zadig en su lugar {#zadig}

Un controlador WinUSB instalado con [Zadig](https://zadig.akeo.ie/) funciona igual de bien:

1. Abra Zadig y vaya a **Options > List All Devices**
2. Seleccione su adaptador Bluetooth USB en la lista desplegable
3. Seleccione **WinUSB** como controlador de destino y haga clic en **Replace Driver**

### A mano en el Administrador de dispositivos {#device-manager}

Si el adaptador no aparece en la lista de BlueCaster, falla un botón de la pantalla **Adaptador** o su adaptador se comporta de forma distinta a otros de su tipo, puede hacer lo mismo a mano en el Administrador de dispositivos de Windows (clic derecho en **Inicio** > **Administrador de dispositivos**). Ambas tareas requieren permisos de administrador. Los nombres de los menús pueden variar un poco según la versión de Windows.

**Reemplazar por el controlador WinUSB**

1. Busque el adaptador: normalmente en **Bluetooth** con su propio nombre (por ejemplo «TP-Link Bluetooth USB Adapter»), o con una marca amarilla cuando su controlador no pudo iniciarse. **Ver > Dispositivos por conexión** ayuda a distinguirlo del Bluetooth integrado del equipo; no toque ese salvo que quiera cederlo a BlueCaster
2. Haga clic derecho en él > **Actualizar controlador** > **Buscar controladores en mi equipo** > **Elegir en una lista de controladores disponibles en el equipo**
3. Si Windows pide un tipo de dispositivo, elija **Dispositivos de bus serie universal**. Desmarque **Mostrar hardware compatible**, elija el fabricante **Dispositivo WinUsb** y el modelo **Dispositivo WinUsb**, haga clic en **Siguiente** y confirme la advertencia
4. El adaptador aparece ahora como **Dispositivo WinUsb** en **Dispositivos de bus serie universal**. Reinicie BlueCaster (o vuelva a abrir la pantalla **Adaptador**): usará el adaptador igual que con Zadig

**Restaurar el controlador original**

1. En **Dispositivos de bus serie universal**, haga clic derecho en el adaptador (**Dispositivo WinUsb**) > **Desinstalar el dispositivo**. Si el controlador WinUSB vino de Zadig, marque también **Intentar quitar el controlador de este dispositivo**; si no, Windows vuelve a elegir el controlador de Zadig
2. Desconecte el adaptador y vuelva a conectarlo (o **Acción > Buscar cambios de hardware**). Windows instala su propio controlador y el adaptador aparece en **Bluetooth**
3. Si en cambio aparece ahí con una marca amarilla (su controlador no pudo iniciarse, por ejemplo con el código 31), el controlador del fabricante no funciona con él: haga clic derecho > **Actualizar controlador** > **Buscar controladores en mi equipo** > **Elegir en una lista de controladores disponibles en el equipo** > **Adaptador Bluetooth genérico**, o asigne de nuevo WinUSB como arriba para usarlo con BlueCaster

## Paso 3: Encontrar los auriculares {#headphones}

**Buscar dispositivos** (en **Dispositivos de audio**) busca dispositivos cercanos con el adaptador USB durante 30 segundos y los muestra a medida que responden. En la configuración inicial, la búsqueda empieza automáticamente y continúa hasta que se conecta un dispositivo (**Añadir un dispositivo** lo desconecta y reanuda la búsqueda), pulsa **Detener búsqueda** o cierra la configuración. Ponga antes los auriculares en modo de emparejamiento. Los dispositivos con los que BlueCaster ya se ha emparejado aparecen como **Emparejado**.

Si la búsqueda no los encuentra, escriba la dirección Bluetooth en **Manual:** y haga clic en **Usar**. BlueCaster da formato a las direcciones escritas o pegadas sobre la marcha: `AA:BB:CC:DD:EE:FF`, `aa-bb-cc-dd-ee-ff`, `AABBCCDDEEFF` y `AA BB CC DD EE FF` se convierten todas en `AA:BB:CC:DD:EE:FF`.

Los auriculares que Windows emparejó a través de este adaptador antes de que tuviera WinUSB pueden conservar ese emparejamiento. **Importar emparejamientos de Windows** (en el paso de dispositivos de la configuración inicial, visible cuando hay dispositivos así) los muestra, todos marcados, e importa los marcados para que se conecten sin modo de emparejamiento. Windows solicita permiso de administrador. Un dispositivo así no suele estar en modo de emparejamiento, por lo que no aparece en la búsqueda. Si aparece, o si escribe su dirección, **Conectar** o **Añadir** avisa primero de que Windows lo emparejó; **Añadir de todos modos** lo empareja de nuevo como un dispositivo nuevo.

**Desde la Configuración de Windows** (si los auriculares se emparejaron con el Bluetooth integrado del PC):
1. Abra **Configuración > Bluetooth y dispositivos**
2. Haga clic en su dispositivo de audio
3. Haga clic en **Propiedades**
4. Se muestra la dirección Bluetooth (formato: `AA:BB:CC:DD:EE:FF`)

**Desde la línea de comandos** (cierre antes BlueCaster, bandeja > **Salir**):
```
bluecaster-core.exe --cli --scan
```

Los auriculares empiezan con **Automático** como códec (**Códecs**) y el audio del sistema como origen de salida (**Dispositivos de audio**); los valores predeterminados dan el mejor sonido. Consulte [Uso](usage) para todo lo demás.

---

## Firmware (adaptadores Realtek) {#firmware}

Los adaptadores Bluetooth USB basados en Realtek (p. ej., TP-Link UB500 o adaptadores RTL8761BU) necesitan firmware propietario para funcionar. Los adaptadores CSR no necesitan firmware. Los adaptadores Intel, MediaTek MT79xx, Qualcomm y Broadcom pueden necesitar sus propios archivos de firmware: consulte [Adaptadores recomendados](#adapters).

### En la aplicación (descarga guiada)

La pantalla **Adaptador** (y el paso **Controlador y firmware** de la configuración inicial) muestra el estado del firmware del adaptador: **Firmware correcto**, **No necesita archivos de Realtek** (un adaptador que BlueCaster no reconoce, con el chip en **No Realtek**) o **Firmware no encontrado** con los nombres de los archivos necesarios. Use el botón **Abrir página de descarga** para abrir la página [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) en el navegador y, después, **Abrir carpeta de firmware** para abrir la carpeta de destino. Descargue los archivos `.bin` necesarios y colóquelos en la carpeta de firmware.

Los adaptadores de otros fabricantes no tienen la opción **Chip Bluetooth**; en su lugar, una etiqueta indica de dónde viene el firmware: **Del controlador de MediaTek** o **Del controlador de Qualcomm** (leído del controlador de Windows del fabricante, que sigue instalado tras WinUSB), **Del controlador Wi-Fi** (Marvell; mantenga el Wi-Fi activado), **De la carpeta de firmware** (Intel, con **Abrir página de descarga**, **Abrir carpeta de firmware** y **Guía de configuración**), **Opcional** (el `.hcd` de Broadcom) o **No necesario** (CSR, Barrot, Actions). Cuando BlueCaster ha cargado el firmware con WinUSB, la etiqueta indica **Cargado**, o **No cargado** si la carga falló, junto con **Abrir página de descarga**, **Abrir carpeta de firmware** y una línea que dice de dónde viene el firmware.

Un adaptador RTL8761B (como el TP-Link UB500) no necesita ninguna descarga si el controlador de Windows de Realtek está instalado en el PC. BlueCaster usa entonces el firmware de ese controlador, que sigue instalado después de pasar el adaptador a WinUSB. Para cargar otro firmware, como el de Linux, seleccione **Usar firmware personalizado** en el recuadro de firmware del adaptador, en la pantalla **Adaptador**. BlueCaster carga entonces los archivos de la carpeta de firmware. Para volver atrás, seleccione **Usar el firmware del controlador**.

Para los chips de la lista **Chip Bluetooth**, **Descargar firmware** lo hace por usted: tras confirmar, BlueCaster descarga los archivos de linux-firmware (kernel.org), comprueba cada uno con el SHA-256 esperado y lo guarda en la carpeta de firmware. Si linux-firmware no tiene archivo config para un chip, BlueCaster guarda un `_config.bin` vacío y el chip usa su propia configuración. Los archivos no forman parte de BlueCaster; solo se descargan cuando usted lo pide.

BlueCaster detecta el chipset del adaptador y muestra qué archivos de firmware se necesitan.

Sin el firmware, un adaptador Realtek se abre igualmente y responde con su código de ROM, así que parece estar bien, pero la conexión o la transmisión fallan, y BlueCaster indica que el firmware no está cargado. Un adaptador conserva el firmware cargado en una sesión anterior hasta que pierde la alimentación, así que, aunque se quiten los archivos, puede seguir funcionando hasta que se reinicie el PC o se desconecte el adaptador.

### Opción Chip Bluetooth

La lista **Chip Bluetooth** de la pantalla **Adaptador** decide si BlueCaster ejecuta la inicialización de Realtek (carga del firmware) en el adaptador:

| Opción | Significado |
|:-------|:--------|
| **Automático** (predeterminado) | Se decide según el adaptador detectado cada vez que se inicia BlueCaster. Un adaptador Realtek conocido recibe su chip; los demás adaptadores no reciben la inicialización de Realtek |
| **No Realtek** | No ejecuta nunca la inicialización de Realtek. Úsela para adaptadores Intel, Broadcom, CSR y otros si Automático no reconoce el suyo |
| **RTL8761BU**, **RTL8822CU**, ... | Ese chip Realtek. Necesario para un adaptador basado en Realtek que Automático no reconoce (un producto OEM con su propio VID:PID). La línea de estado indica si los archivos `.bin` del chip están en la carpeta de firmware |
| **Custom** | Usa `custom_fw.bin` y `custom_config.bin` de la carpeta de firmware |

No elija un chip Realtek ni Custom para un adaptador que no sea Realtek: BlueCaster intentaría cargarle firmware de Realtek, y es posible que el adaptador no llegue a iniciarse.

### Descarga manual

1. Descargue los archivos `.bin` de firmware y de configuración de su chipset desde [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) (p. ej., `rtl8761bu_fw.bin` y `rtl8761bu_config.bin`)
2. Coloque ambos archivos en la carpeta de firmware de BlueCaster (`%APPDATA%\BlueCaster\firmware`, que también abre **Abrir carpeta de firmware**)

{: .note }
Estos archivos de firmware son binarios propietarios de Realtek distribuidos a través del proyecto linux-firmware. No se incluyen en este repositorio. Consulte [WHENCE](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/WHENCE) para conocer las condiciones de redistribución.

---

## Adaptadores recomendados {#adapters}

Los adaptadores y auriculares que se han usado realmente con BlueCaster, con sus códecs e informes, figuran en [Hardware verificado](hardware).

| Chipset | Productos de ejemplo | Notas |
|:--------|:-----------------|:------|
| Realtek | TP-Link UB500, RTL8761BU | Usa el firmware del controlador de Windows de Realtek si está instalado; si no, descargue el firmware (ver arriba). El más probado |
| Realtek | RTL8852BD (`0BDA:B853`), RTL8852BT, RTL8922A | BlueCaster lee el firmware (p. ej. `rtl8852bd_mp_chip_new.dat`) del controlador Bluetooth de Windows de Realtek, así que no desinstale ese controlador, o coloque en la carpeta de firmware el archivo que indica el registro. Aún no se ha probado en el hardware |
| CSR | Adaptadores CSR8510 genéricos | Funcionan sin más, sin firmware |
| Barrot | Adaptadores Bluetooth 5.4 / 6.0 (`33FA:0010`, `33FA:0012`) | Funcionan con el firmware de su ROM, sin firmware adicional. Por ahora, un solo informe de usuario |
| Marvell | Integrado en Microsoft Surface Pro / Surface Book (88W8897) | Sin archivos de firmware: el controlador de Wi-Fi carga también el firmware de Bluetooth, así que mantenga el Wi-Fi activado. Aún no probado en el hardware |
| Actions | Adaptadores ATS2851 (`10D7:B012`) | Funciona con el firmware de su ROM, sin firmware adicional. El adaptador anuncia comandos que no admite; BlueCaster los oculta. Aún no probado en el hardware |
| Broadcom | ASUS BT400 (BCM20702) y otros adaptadores y módulos de portátil BCM20702 / BCM20703 / BCM43142 | También se detectan los adaptadores que indican la clase de fabricante `FF/01/01` en lugar de la clase Bluetooth `E0/01/01` (adaptadores OEM BCM20702). Funciona con el firmware de su ROM. Si está instalado el controlador Bluetooth de Windows de Broadcom, BlueCaster carga al iniciar el parche de ese controlador. Si hay un archivo PatchRAM `.hcd` (p. ej. `BCM20702A1-0b05-17cb.hcd`) en la carpeta de firmware, BlueCaster usa ese archivo en su lugar. Si falla el parche del controlador, BlueCaster le pide que desconecte el adaptador y lo vuelva a conectar, y deja de cargar el parche en ese adaptador. El parche del controlador aún no se ha probado en el hardware |
| Intel | Intel 7260 / 3160 / 7265 / 3165 / 3168 / 8260 / 8265 / 9260 / AX200 / AX201 / AX210 / AX211 | En modo bootloader (p. ej. tras un arranque en frío) necesita los archivos `ibt-*.sfi` + `ibt-*.ddc` correspondientes de `intel/` de linux-firmware en la carpeta de firmware; el registro indica cuáles (p. ej. `ibt-0180-0041.sfi`). Los 7260 / 3160 / 7265 / 3165 / 3168 funcionan desde la ROM y no necesitan archivos; si el parche `ibt-hw-*.bseq` correspondiente está en la misma carpeta, se aplica |
| MediaTek | MT7925, MT7927, MT7922, MT7921, MT7902 (tarjetas Wi-Fi como la AMD RZ717, RZ738, RZ616, RZ608) | Necesita el `BT_RAM_CODE_MT*_hdr.bin` del chip (p. ej. `BT_RAM_CODE_MT7925_1_1_hdr.bin`). BlueCaster lo lee del controlador Bluetooth de Windows de MediaTek si ese controlador está instalado. Si no, coloque en la carpeta de firmware el archivo de `mediatek/` de linux-firmware que indica el registro. El MT7927 aún no se ha probado en el hardware |
| Qualcomm | QCA61x4, QCA9377 (ROME), WCN6855 (FastConnect 6900), WCN785x (FastConnect 7800) | Necesita el rampatch y el NVM del chip (p. ej. `AthrBT_0x00000302.dfu` y `ramps_0x00000302_48.dfu`). BlueCaster los lee del controlador Bluetooth de Windows de Qualcomm si ese controlador está instalado. Si no, coloque en la carpeta de firmware los archivos de `qca/` de linux-firmware que indica el registro (`rampatch_usb_*.bin`, `nvm_usb_*.bin`). Aún no se ha probado en el hardware |
| MediaTek | MT7663 | Necesita el archivo de parche del chip (p. ej. `mt7663_patch_e2_hdr.bin`). BlueCaster lo lee del controlador Bluetooth de Windows de MediaTek si ese controlador está instalado. Si no, coloque en la carpeta de firmware el archivo que indica el registro. Aún no se ha probado en el hardware |
| Qualcomm Atheros | AR3012 y otros chips AR30xx (Bluetooth 4.0 en portátiles antiguos) | Necesita los archivos de parche y de configuración del chip (p. ej. `AthrBT_0x31010000.dfu` y `ramps_0x31010000_40.dfu`). BlueCaster los lee del controlador Bluetooth de Windows de Qualcomm Atheros si ese controlador está instalado. Si no, coloque en la carpeta de firmware los archivos que indica el registro. Aún no se ha probado en el hardware |
| MediaTek, Qualcomm | Chips MediaTek más antiguos (como el MT7630 y el MT7650), otros chips Qualcomm (QCA6290) | No compatibles: necesitan cargadores de firmware que BlueCaster no tiene ([ayude a añadir uno](chip-support)) |

Por lo general, los adaptadores USB Bluetooth 5.0+ funcionan mejor con códecs de alta tasa de bits como LDAC.

Si varios adaptadores tienen WinUSB, BlueCaster usa el primero que encuentra, salvo que pulse **Usar este adaptador** en la tarjeta de otro, en la pantalla **Adaptadores**. El adaptador que se usa aparece marcado como **En uso**.

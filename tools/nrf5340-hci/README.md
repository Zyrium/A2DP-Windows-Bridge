# nRF5340 DK as a BlueCaster adapter (Auracast, four BIGs)

Firmware that makes an nRF5340 DK a USB HCI controller for BlueCaster,
set up for Auracast with up to four broadcasts at once (the ASUS BT600
refuses a third BIG: see `docs/le-audio-dev.md`, "Broadcast").

- Application core: Zephyr's `samples/bluetooth/hci_usb` (nRF Connect SDK
  v3.4.1), USB 2FE3:000B ("Zephyr USBD BT HCI", known to BlueCaster), with
  the USB HCI class replaced by `src/bt_hci_iso.c`: the stock class takes
  every bulk OUT packet for ACL data and drops the HCI ISO data BlueCaster
  sends on the same pipe; the copy learns the BIS handles from the LE
  Create BIG Complete events and passes packets on them as ISO.
  `src/main.c` sets the device class to E0/01/01 (Bluetooth), as dongles
  have: with the sample's 00/00/00 Windows takes the board for a
  composite device and BlueCaster does not list it.
- Network core: `hci_ipc` with Nordic's SoftDevice Controller and
  `sysbuild/hci_ipc_sdc.conf`: four advertising sets with periodic
  advertising, four BIGs, four BIS, 10 ISO buffers.
- `sysbuild/hci_ipc_ll_sw.conf` is the earlier set-up with the open Zephyr
  LL (snippet `bt-ll-sw-split`, HCI handle mapping off: with it the LL
  reports its own index as the BIG handle). It ran two BIGs with audio,
  then never completed a third LE Create BIG (no assert on the console).

## Build

Requires nRF Connect SDK v3.4.1 and its toolchain. The commands assume the
default install folder `C:\ncs`. `<toolchain id>` is the folder that the
toolchain manager created under `C:\ncs\toolchains`. In PowerShell:

```powershell
$T = 'C:\ncs\toolchains\<toolchain id>'
$env:PATH = "$T;$T\mingw64\bin;$T\bin;$T\opt\bin;$T\opt\bin\Scripts;$T\nrfutil\bin;$T\opt\zephyr-sdk\gnu\arm-zephyr-eabi\bin;" + $env:PATH
$env:PYTHONPATH = "$T\opt\bin;$T\opt\bin\Lib;$T\opt\bin\Lib\site-packages"
$env:ZEPHYR_TOOLCHAIN_VARIANT = 'zephyr/gnu'; $env:ZEPHYR_SDK_INSTALL_DIR = "$T\opt\zephyr-sdk"
$env:ZEPHYR_BASE = 'C:\ncs\v3.4.1\zephyr'
cd C:\ncs\v3.4.1
west build -p always -b nrf5340dk/nrf5340/cpuapp --sysbuild -d <build dir> <repo>\tools\nrf5340-hci
```

(From Git Bash the toolchain's Python fails with a ctypes error; use
PowerShell.)

## Flash

With the DK on its J-Link USB port (the one marked "IMCU"/debugger):
`west flash -d <build dir>` (both cores). Then connect the nRF5340 USB
port (marked "nRF5340 USB"): it appears as 2FE3:000B and needs WinUSB
(Zadig) like any other BlueCaster adapter.

## Results (2026-11-21, two JBL GO 4 class receivers)

- One stereo broadcast (1 BIG, 2 BIS) at the presets: stable, no offset
  between the channels.
- Four BIGs (quad, front + rear, matrix 48 + 24 kHz) are created and
  BlueCaster sends every SDU without an error, but the air is nearly full
  at 48 kHz: with the presets' RTN 2 the sound drops out. With
  `BLUECASTER_AURACAST_RTN=1` and `BLUECASTER_AURACAST_LATENCY_MS=20`
  (test knobs, applied only when there is more than one BIG) all layouts
  connect and play fairly well, with occasional stutters.
- Shrinking SDC's reserved time (BIG 400 us, periodic advertising event
  1000 us) made it worse: receivers lost sync even at 24 kHz.
- The board keeps a bad state after a failed attempt (the controller
  stops answering): a power cycle clears it.

## Settings

The ISO console trace (`CONFIG_BLUECASTER_TRACE`, counters every 2 s on
the J-Link VCOM port) and the network core's error log are enabled in
`prj.conf` and `sysbuild/hci_ipc_sdc.conf`; turn them off for daily use.

## License

`src/bt_hci_iso.c`, `src/main.c` and the build files are modified copies of
Zephyr sources and stay under Apache-2.0 (see `THIRD_PARTY_LICENSES.md`,
§22).

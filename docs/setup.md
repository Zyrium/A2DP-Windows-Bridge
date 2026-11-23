---
title: Setup
layout: default
nav_order: 2
---

# Setup
{: .no_toc }

## Table of contents
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Prerequisites

- **Windows 10 (22H2) / 11** (x64 or ARM64)
- **A USB Bluetooth adapter for BlueCaster**: a separate dongle is recommended; a built-in USB adapter also works, but Windows then loses its own Bluetooth (see [Step 2](#winusb))

BlueCaster assigns the WinUSB driver to the adapter for you, so you no longer need [Zadig](https://zadig.akeo.ie/) (it still works; see [Using Zadig instead](#zadig)).

## Installing BlueCaster

Each [release](https://github.com/SeiyaFunaokaJP/BlueCaster/releases) comes in two forms with the same files:

- **Installer** (`BlueCaster-<version>-win64-setup.exe`, on ARM64 `-arm64-setup.exe`): installs for the current user without administrator rights, adds a Start menu entry (**BlueCaster**) and an uninstaller, and installs the Microsoft Edge WebView2 Runtime if it is missing (Windows 11 includes it). Updates install from within the app (**Settings > Updates > Update now**). The programs go to `%LOCALAPPDATA%\BlueCaster\` (the three programs, the uninstaller and a `licenses\` folder with the license texts)
- **Portable** (`BlueCaster-<version>-win64-portable.zip`, on ARM64 `-arm64-portable.zip`): extract anywhere and run `bluecaster.exe`. Keep the three programs (`bluecaster.exe`, `bluecaster-core.exe`, `bluecaster-driver.exe`) together. The portable copy only notifies you about updates and requires the WebView2 Runtime

Both keep settings, profiles, link keys and firmware in `%APPDATA%\BlueCaster`, so you can switch between them. Uninstalling deletes that folder unless you turn **Delete user data** off (a silent uninstall keeps it).

### Upgrading from v1 {#from-v1}

Run the installer over v1. Its **Ready** page lists the earlier versions it will uninstall first (v1, or any 2.0.x). The installer asks you to close v1 if it is running, then uninstalls v1 but keeps the settings folder: profiles, headphone pairings and firmware files carry over, so you don't need to pair the headphones again. An adapter that already has WinUSB (from Zadig) keeps it. If **Start with Windows** was on, Windows now starts v2.

On first start, v2 copies the v1 settings to `%APPDATA%\BlueCaster\backup\v1\` before reading them.

### Upgrading from 2.0.x (A2DP Windows Bridge) {#from-2x}

BlueCaster 3.0.0 is the new name of A2DP Windows Bridge. Run the 3.0.0 installer over the installed 2.0.x: setup removes the old version automatically and keeps your settings. The Start menu entry and the **Start with Windows** entry are renamed to "BlueCaster".

On first start, 3.0.0 renames the folder `%APPDATA%\A2DPWB` to `%APPDATA%\BlueCaster` and sorts the files into subfolders (see [Settings and Files](architecture#settings-and-files)). `backup\v2\` keeps a copy of the 2.0.x settings and pairings in case you want to go back to 2.0.2: copy those files back to `%APPDATA%\A2DPWB`.

The headphones may keep showing the old Bluetooth name "A2DPWB" until you pair them again.

## Step 1: First-Run Setup {#first-run}

Plug in the USB Bluetooth adapter and start BlueCaster. On first start (no audio device yet, or no adapter with WinUSB), BlueCaster opens the four-step **Setup Wizard**. Once you finish or skip it, the wizard does not open automatically again; add devices on **Home** or **Audio Devices** instead.

1. **Choose the adapter**: lists the USB Bluetooth adapters found. If yours is missing, plug it in and press **Refresh**. A green **Supported** or red **Unsupported** label shows whether BlueCaster can use each adapter; unsupported adapters are listed but cannot be selected
2. **Driver and firmware**: the step lists what it will do, and **Set up this adapter** carries it out: BlueCaster replaces the adapter's driver with WinUSB (Windows asks for administrator permission; see [Step 2](#winusb)) and downloads any Realtek firmware files the adapter needs (see [Firmware](#firmware)). Firmware that BlueCaster cannot download must be copied into the firmware folder by hand. If Windows has devices paired through this adapter (for example, a keyboard or mouse on a built-in adapter), the button lists them first: they stop working in Windows while BlueCaster uses the adapter, and **Cancel** keeps them working. Devices with their own USB receiver are not affected; to keep the others, use BlueCaster with a separate USB adapter. **Replace with WinUSB driver** on the **Adapters** screen lists them too
3. **Find your audio devices**: the scan starts automatically. Put the audio device into pairing mode and press **Connect** next to it (or type its address and press **Use**): BlueCaster saves the device with the best codec it supports (Auto) and connects. The device then appears under **Paired Devices** with a status dot, and the scan stops. **Add a device** disconnects it and restarts the scan for another device; the first device stays saved, and you can forget a wrong one there. **Import pairings from Windows** appears when Windows has paired devices through this adapter: its **Import** button lists them, all ticked (see [Step 3](#headphones)). **Next** also works without a device; you can add audio devices later on **Audio Devices**. New devices capture the **Hi-Fi Cable** if it is installed, otherwise VB-CABLE. The Hi-Fi Cable adds the least delay, so it is recommended for sound, and VB-CABLE for the headphones' microphone. If a cable is missing, the step shows a warning, and its **Learn more** lists both cables: **Install** for VB-CABLE downloads VB-Audio's VB-CABLE package (1.3 MB) and installs it (with an administrator prompt; if the prompt does not appear, restart Windows), and **Download** for the Hi-Fi Cable opens VB-Audio's page. An installed cable shows **Uninstall** instead. Both cables are donationware by VB-Audio ([vb-cable.com](https://vb-cable.com/)), as **About** notes: BlueCaster does not include VB-CABLE, and donations go to VB-Audio. VB-Audio asks companies and other organisations for a licence per installation ([licensing](https://vb-audio.com/Services/licensing.htm)). When BlueCaster later finds a cable for the first time, devices set to **Default device** switch to capturing it (VB-CABLE only while the Hi-Fi Cable is not installed), and devices whose microphone has no working output send the microphone there. You can change the codec later on **Codecs**
4. **General Settings**: the theme, **Startup**, **Notifications** and update checks, the same controls as in Settings, saved immediately. The first setup turns on **Start with Windows** and **Start in the tray**

**Set up later** skips the rest; **Settings > Run setup** opens it again.

## Step 2: Assign WinUSB to the Adapter {#winusb}

BlueCaster communicates directly with a USB Bluetooth adapter via WinUSB, the generic USB driver included in Windows. A **dedicated adapter** is recommended -- that way your built-in Bluetooth keeps working normally in Windows.

1. Open the **Adapters** screen (the setup's **Driver and firmware** step does steps 2 to 4 with **Set up this adapter**)
2. The adapter's **Driver** shows **Original driver** (or **No driver**): the driver installed by Windows or the manufacturer, which WinUSB replaces. Click **Replace with WinUSB driver**
3. Read the confirmation and continue: Windows asks for administrator permission (UAC)
4. The driver now shows **WinUSB driver**

If Windows cannot load WinUSB right away (because the adapter is in use), BlueCaster tells you, and the driver line shows **Replug or restart Windows**. Unplug the adapter and plug it back in; for a built-in adapter, restart Windows. Until then, connecting fails with `failed to initialize WinUSB (error 50)` in the log.

{: .warning }
While WinUSB is assigned, Windows' own Bluetooth no longer sees the adapter: devices paired with Windows through it stop working until the driver is restored. Use your built-in Bluetooth for normal peripherals (keyboard, mouse, etc.). You can also give BlueCaster a built-in adapter that is a USB device (a Realtek one on many laptops), but then **the PC has no Windows Bluetooth at all** (Bluetooth settings, mice, keyboards, other audio devices) until you restore its driver.

**Restore original driver** on the same screen returns the adapter to Windows: Windows picks its own driver again (normally its Bluetooth driver), and the adapter works with Windows Bluetooth. BlueCaster cannot stream through it until WinUSB is assigned again. Windows uses only one Bluetooth adapter at a time. If another adapter is active, BlueCaster tells you after the restore; to use this adapter in Windows, disable the other one in Device Manager and replug this one. Headphones that BlueCaster paired again have a new key. For headphones that Windows had paired through this adapter, the restore passes the new key to Windows, so they connect there without pairing again. **Limitation:** only classic pairings are transferred, not LE keys. A device that Windows uses over LE (LE Audio) must be paired again in Windows (developer mode can try transferring LE keys: **Developer > Debug**). The uninstaller offers the same: one page of the uninstall wizard has **Restore the original Bluetooth driver** (when BlueCaster put adapters on WinUSB; one administrator prompt) and **Delete user data** (settings, codecs, pairings and firmware in `%APPDATA%\BlueCaster`, and what the window remembers, such as **Show only supported codecs**), both on by default. If the only adapters on WinUSB were set up with Zadig or v1 (so BlueCaster has no record of them), the first option is shown but off, and Windows chooses their driver if you turn it on. Updates keep WinUSB, and so does a silent uninstall (`/S`). The same page offers **Remove VB-CABLE** and **Remove Hi-Fi Cable** when they are installed, off by default because other apps may use them. They are removed without VB-Audio's setup window, after an administrator prompt from Windows. Windows then scans for hardware changes so the cable disappears from the Sound list; a restart is offered only if it is still listed.

### Using Zadig instead {#zadig}

A WinUSB driver installed with [Zadig](https://zadig.akeo.ie/) works just as well:

1. Open Zadig, go to **Options > List All Devices**
2. Select your USB Bluetooth adapter from the dropdown
3. Select **WinUSB** as the target driver and click **Replace Driver**

### By hand in Device Manager {#device-manager}

If the adapter is missing from BlueCaster's list, a button on the **Adapters** screen fails, or your adapter behaves differently from others of its kind, you can do the same by hand in Windows' Device Manager (right-click **Start** > **Device Manager**). Both tasks need administrator rights. Menu names may differ slightly between Windows versions.

**Replace with WinUSB driver**

1. Find the adapter: normally under **Bluetooth** with its own name (for example "TP-Link Bluetooth USB Adapter"), or with a yellow mark when its driver failed to start. **View > Devices by connection** helps tell it apart from the PC's built-in Bluetooth; leave that one alone unless you want to give it to BlueCaster
2. Right-click it > **Update driver** > **Browse my computer for drivers** > **Let me pick from a list of available drivers on my computer**
3. If Windows asks for a device type, choose **Universal Serial Bus devices**. Clear **Show compatible hardware**, choose the manufacturer **WinUsb Device** and the model **WinUsb Device**, click **Next** and confirm the warning
4. The adapter now shows as **WinUsb Device** under **Universal Serial Bus devices**. Restart BlueCaster (or open the **Adapters** screen again): it uses the adapter just as it would after Zadig

**Restore the original driver**

1. Under **Universal Serial Bus devices**, right-click the adapter (**WinUsb Device**) > **Uninstall device**. If the WinUSB driver came from Zadig, also tick **Attempt to remove the driver for this device**, or Windows picks Zadig's driver again
2. Unplug the adapter and plug it in again (or **Action > Scan for hardware changes**). Windows installs its own driver and the adapter appears under **Bluetooth**
3. If it shows a yellow mark there instead (its driver failed to start, for example Code 31), the manufacturer's driver does not work with it: right-click > **Update driver** > **Browse my computer for drivers** > **Let me pick from a list of available drivers on my computer** > **Generic Bluetooth Adapter**, or assign WinUSB again as above to use it with BlueCaster

## Step 3: Find Your Headphones {#headphones}

**Scan Devices** (on **Audio Devices**) searches for nearby devices through the USB adapter for 30 seconds and lists them as they respond. In the setup, the scan starts automatically and continues until a device connects (**Add a device** disconnects it and resumes the scan), you press **Stop Scanning**, or you close the setup. Put the headphones into pairing mode first. Devices BlueCaster has paired with before are listed as **Paired**.

If the scan does not find them, enter the Bluetooth address under **Manual:** and click **Use**. BlueCaster formats typed or pasted addresses as you go: `AA:BB:CC:DD:EE:FF`, `aa-bb-cc-dd-ee-ff`, `AABBCCDDEEFF` and `AA BB CC DD EE FF` all become `AA:BB:CC:DD:EE:FF`.

Headphones that Windows paired through this adapter before it got WinUSB can keep that pairing. **Import pairings from Windows** (in the setup's device step, shown when such devices exist) lists them, all ticked, and imports the ticked ones so they connect without pairing mode. Windows asks for administrator permission. Such a device is usually not in pairing mode, so it does not appear in the scan. If it does appear, or you type its address, **Connect** or **Add** first warns that Windows paired it; **Add anyway** pairs it again as a new device.

**From Windows Settings** (if the headphones were paired with the PC's built-in Bluetooth):
1. Open **Settings > Bluetooth & devices**
2. Click on your audio device
3. Click **Properties**
4. The Bluetooth address is shown (format: `AA:BB:CC:DD:EE:FF`)

**From the command line** (quit BlueCaster first, tray > **Exit**):
```
bluecaster-core.exe --cli --scan
```

The headphones start with **Auto** as the codec (**Codecs**) and the system audio as the output source (**Audio Devices**); these defaults give the best sound. See [Usage](usage) for everything else.

---

## Firmware (Realtek Adapters) {#firmware}

Realtek-based USB Bluetooth adapters (e.g., TP-Link UB500, RTL8761BU dongles) require proprietary firmware to operate. CSR adapters need no firmware. Intel, MediaTek MT79xx, Qualcomm and Broadcom adapters may need firmware files of their own: see [Recommended Adapters](#adapters).

### In the App (Guided Download)

The **Adapters** screen (and the setup's **Driver and firmware** step) shows the adapter's firmware status: **Firmware OK**, **No Realtek files needed** (an adapter BlueCaster does not recognise, with the chip set to **Not Realtek**), or **Firmware not found** with the required file names. Use the **Open Download Page** button to open the [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) page in your browser, then use **Open Firmware Folder** to open the destination folder. Download the required `.bin` files and place them in the firmware folder.

Adapters from other vendors have no **Bluetooth Chip** choice; instead, a tag shows where the firmware comes from: **From MediaTek's driver** or **From Qualcomm's driver** (read from the vendor's Windows driver, which stays installed after WinUSB), **From the Wi-Fi driver** (Marvell; keep Wi-Fi on), **From the firmware folder** (Intel, with **Open Download Page**, **Open Firmware Folder** and **Setup guide**), **Optional** (Broadcom's `.hcd`) or **Not needed** (CSR, Barrot, Actions). Once BlueCaster has loaded the firmware over WinUSB, the tag reads **Loaded**, or **Not loaded** if loading failed, along with **Open Download Page**, **Open Firmware Folder** and a line saying where the firmware comes from.

An RTL8761B adapter (such as the TP-Link UB500) needs no download if Realtek's Windows driver is installed on the PC. BlueCaster then uses that driver's firmware. The driver stays installed after the adapter switches to WinUSB. To load other firmware, such as the Linux firmware, select **Use custom firmware** in the adapter's firmware box on the **Adapters** screen. BlueCaster then loads the files in the firmware folder. To switch back, select **Use the driver's firmware**.

For the chips in the **Bluetooth Chip** list, **Download Firmware** does this for you: after you confirm, BlueCaster downloads the files from linux-firmware (kernel.org), checks each one against the SHA-256 it expects and saves it in the firmware folder. If linux-firmware has no config file for a chip, BlueCaster saves an empty `_config.bin` and the chip uses its own settings. The files are not part of BlueCaster; they are downloaded only when you ask for them.

BlueCaster detects the adapter's chipset and shows which firmware files are needed.

Without the firmware, a Realtek adapter still opens and responds using its ROM code, so it looks fine, but connecting or streaming fails, and BlueCaster reports that the firmware is not loaded. An adapter keeps firmware uploaded in an earlier session until it loses power, so even after the files are removed it may keep working until the PC restarts or the adapter is unplugged.

### Bluetooth Chip setting

The **Bluetooth Chip** list on the **Adapters** screen decides whether BlueCaster runs Realtek initialization (firmware upload) on the adapter:

| Choice | Meaning |
|:-------|:--------|
| **Auto** (default) | Decided from the detected adapter each time BlueCaster starts. A known Realtek adapter gets its chip; any other adapter gets no Realtek initialization |
| **Not Realtek** | Never run Realtek initialization. Use it for Intel, Broadcom, CSR and other adapters if Auto does not recognize yours |
| **RTL8761BU**, **RTL8822CU**, ... | That Realtek chip. Needed for a Realtek-based adapter that Auto does not recognize (an OEM product under its own VID:PID). The status line shows whether its `.bin` files are in the firmware folder |
| **Custom** | Uses `custom_fw.bin` and `custom_config.bin` from the firmware folder |

Do not choose a Realtek chip or Custom for a non-Realtek adapter: BlueCaster would try to upload Realtek firmware to it, and the adapter may fail to start.

### Manual Download

1. Download the firmware and config `.bin` files for your chipset from [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) (e.g., `rtl8761bu_fw.bin` and `rtl8761bu_config.bin`)
2. Place both files in the BlueCaster firmware folder (`%APPDATA%\BlueCaster\firmware`, also opened by **Open Firmware Folder**)

{: .note }
These firmware files are proprietary Realtek binaries distributed via the linux-firmware project. They are not included in this repository. See [WHENCE](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/WHENCE) for redistribution terms.

---

## Recommended Adapters {#adapters}

For the adapters and headphones actually used with BlueCaster, with codecs and reports, see [Verified Hardware](hardware).

| Chipset | Example Products | Notes |
|:--------|:-----------------|:------|
| Realtek | TP-Link UB500, RTL8761BU | Uses the firmware from Realtek's Windows driver if it is installed; otherwise download the firmware (see above). Best tested |
| Realtek | RTL8852BD (`0BDA:B853`), RTL8852BT, RTL8922A | BlueCaster reads the firmware (e.g. `rtl8852bd_mp_chip_new.dat`) from Realtek's Windows Bluetooth driver, so keep that driver installed, or put the file named in the log in the firmware folder. Not yet tested on the hardware |
| CSR | Generic CSR8510 dongles | Works out of the box, no firmware needed |
| Barrot | Bluetooth 5.4 / 6.0 dongles (`33FA:0010`, `33FA:0012`) | Runs on ROM firmware, no firmware needed. One user report so far |
| Marvell | Built into Microsoft Surface Pro / Surface Book (88W8897) | No firmware files: the Wi-Fi driver loads the Bluetooth firmware too, so keep Wi-Fi on. Not yet tested on the hardware |
| Actions | ATS2851 dongles (`10D7:B012`) | Runs on ROM firmware, no firmware needed. The adapter lists commands it does not support; BlueCaster hides them. Not yet tested on the hardware |
| Broadcom | ASUS BT400 (BCM20702), other BCM20702 / BCM20703 / BCM43142 dongles and laptop modules | Adapters that report the vendor class `FF/01/01` instead of Bluetooth `E0/01/01` (BCM20702 OEM dongles) are detected too. Runs on ROM firmware. If Broadcom's Windows Bluetooth driver is installed, BlueCaster uploads the patch from that driver at startup. A PatchRAM `.hcd` (e.g. `BCM20702A1-0b05-17cb.hcd`) in the firmware folder takes precedence. If the driver's patch fails, BlueCaster asks you to unplug the adapter and plug it back in, and no longer uploads the driver's patch to that adapter. The driver's patch is not yet tested on the hardware |
| Intel | Intel 7260 / 3160 / 7265 / 3165 / 3168 / 8260 / 8265 / 9260 / AX200 / AX201 / AX210 / AX211 | In bootloader mode (e.g. after a cold boot) needs the matching `ibt-*.sfi` + `ibt-*.ddc` from linux-firmware `intel/` in the firmware folder; the log names them (e.g. `ibt-0180-0041.sfi`). The 7260 / 3160 / 7265 / 3165 / 3168 run from ROM and need no files; the matching patch `ibt-hw-*.bseq` from the same folder is applied when present |
| MediaTek | MT7925, MT7927, MT7922, MT7921, MT7902 (Wi-Fi cards such as the AMD RZ717, RZ738, RZ616, RZ608) | Needs the chip's `BT_RAM_CODE_MT*_hdr.bin` (e.g. `BT_RAM_CODE_MT7925_1_1_hdr.bin`). BlueCaster reads it from MediaTek's Windows Bluetooth driver when that driver is installed. Otherwise, take the file named in the log from linux-firmware `mediatek/` and put it in the firmware folder. The MT7927 is not yet tested on the hardware |
| Qualcomm | QCA61x4, QCA9377 (ROME), WCN6855 (FastConnect 6900), WCN785x (FastConnect 7800) | Needs the chip's rampatch and NVM (e.g. `AthrBT_0x00000302.dfu` and `ramps_0x00000302_48.dfu`). BlueCaster reads them from Qualcomm's Windows Bluetooth driver when that driver is installed. Otherwise, take the files named in the log from linux-firmware `qca/` (`rampatch_usb_*.bin`, `nvm_usb_*.bin`) and put them in the firmware folder. Not yet tested on the hardware |
| MediaTek | MT7663 | Needs the chip's patch file (e.g. `mt7663_patch_e2_hdr.bin`). BlueCaster reads it from MediaTek's Windows Bluetooth driver when that driver is installed. Otherwise, put the file named in the log in the firmware folder. Not yet tested on the hardware |
| Qualcomm Atheros | AR3012 and other AR30xx chips (Bluetooth 4.0 in older laptops) | Needs the chip's patch and configuration files (e.g. `AthrBT_0x31010000.dfu` and `ramps_0x31010000_40.dfu`). BlueCaster reads them from Qualcomm Atheros' Windows Bluetooth driver when that driver is installed. Otherwise, put the files named in the log in the firmware folder. Not yet tested on the hardware |
| MediaTek, Qualcomm | Older MediaTek chips (such as the MT7630 and MT7650), other Qualcomm chips (QCA6290) | Not supported: need firmware loaders BlueCaster does not have ([help add one](chip-support)) |

USB Bluetooth 5.0+ adapters generally work best for high-bitrate codecs like LDAC.

When several adapters have WinUSB, BlueCaster uses the first one it finds, unless you press **Use this adapter** on another one's card on the **Adapters** screen. The adapter in use is marked **In use**.

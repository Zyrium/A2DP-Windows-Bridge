# Changelog

All notable changes in this fork (**Zyrium/A2DP-Windows-Bridge**) are documented in this file.

## [v3.0.4] - 2026-11-26

### Upstream Sync
- Merged upstream **BlueCaster v3.0.4** (`SeiyaFunaokaJP/BlueCaster`), including adapter selection, CSR8510 SCO fixes, reliable settings saving, experimental LE Audio & Auracast updates, and HID prototype support.
- Added `QcyState`, `QcyRefresh`, `QcyReconnect`, and `QcySet` to IPC service access rules (`app/src/crates/ipc/src/access.rs`).

### QCY MeloBuds N70 (`Fones` / `Earbuds`)
- **Automatic BLE GATT without LE Audio / Developer mode**: enabled `LeCore` for plain BLE GATT (`le_connect_plain`, `le_scan_until`, `le_core_ready`) so QCY control works alongside Classic A2DP (`LDAC` / `SBC XQ`) without enabling LE Audio streaming or Developer mode.
- **Auto-refresh on connect**: automatically refreshes QCY state in the background when a QCY headset connects (`state.rs` + `qcy_live::should_auto_refresh`).
- **Complete QCY control mapping**: unlocked and wired end-to-end (`qcy-protocol` → `qcy_live` → `ipc` → `src-tauri` → `Fones.tsx`):
  - LDAC toggle (`0x23`)
  - Multipoint toggle (`0x24`)
  - Auto power-off (`0x14`: Never, 5, 15, 30, 60 min)
  - 10-band Equalizer (`0x22`: 8 built-in presets + custom curve)
  - Touch Gestures (`0x2B`: double tap & long press for left/right earbuds)
  - Prompt volume (`0x1D`) kept read-only (ignored by N70 firmware).
- **UI cleanup**: removed the "Teste de escrita" banner and "Verificado" / "Em teste" badges from `Fones.tsx`.

### Audio & Bitrate Controls
- **Home screen bitrate picker**: added `BitratePicker` on the **Home** (`Início`) device card (`Home.tsx` + `chooseQuality` in `Codecs.tsx`) to switch bitrate/quality directly when the active codec supports multiple levels.
- **Additional codec quality levels**: exposed `sq` (Medium) and `mq` (Low) levels in `codecs.ts` for:
  - **SBC XQ**: `~552 kbps` / `~512 kbps` / `~385 kbps`
  - **SBC**: `~345 kbps` / `~229 kbps` / `~133 kbps`
  - **AAC**: `256 kbps` / `192 kbps` / `128 kbps`

### Localization & Docs
- Updated Brazilian Portuguese (`app/lang/pt-BR.json`) with all 149 new strings from v3.0.3 / v3.0.4 (1421 keys total).
- Updated `README.md` and added `CHANGELOG.md`.

## [v3.0.2] - 2026-11-20

- Merged upstream **BlueCaster v3.0.2**.
- Added `qcy-protocol` crate and `Fones` screen with initial QCY MeloBuds N70 support (battery, noise control, game mode, sleep mode, wind reduction, wear detection).
- Added full Brazilian Portuguese (`pt-BR`) translation.
- Added GitHub Actions Windows x64 release workflow (`.github/workflows/release.yml`).

---
title: Architektur
layout: default
parent: Startseite
nav_order: 4
---

# Architektur
{: .no_toc }

## Inhalt
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Überblick

BlueCaster bringt Bluetooth-Audio mit LHDC V5, LDAC, aptX Adaptive, aptX HD,
aptX Low Latency, aptX, Opus, AAC, SBC XQ und SBC auf Windows
(dazu LC3 über LE Audio, experimentell). Der Bluetooth-Stack von Windows unterstützt für A2DP
nur SBC, klassisches aptX und (nur unter Windows 11) AAC. BlueCaster ergänzt die
übrigen Codecs, ohne einen Kerneltreiber zu benötigen.

BlueCaster verwendet **BTstack + WinUSB**: Alles läuft im Benutzermodus, daher ist keine Treibersignatur nötig.

BlueCaster 2.0 ist in Rust geschrieben. Die C-Bibliotheken, die sich nicht ersetzen lassen
(BTstack und die Codec-Encoder), werden aus dem Quellcode kompiliert und über
kleine FFI-Crates aufgerufen. Alles andere, auch der SBC-Encoder, ist Rust.

## Prozesse

BlueCaster läuft als bis zu drei Programme:

| Programm | Läuft | Aufgabe |
|:---------|:------|:--------|
| `bluecaster-core.exe` | Solange BlueCaster läuft (gestartet vom Fenster, vom Startmenü oder über **Mit Windows starten**) | Bluetooth, Audioaufnahme und Kodierung, Profile und Einstellungen, das Symbol im Infobereich und sein Menü, der IPC-Server. `--cli` führt denselben Kern stattdessen in einer Konsole aus |
| `bluecaster.exe` | Nur solange ein Fenster offen ist (beendet sich 3 Minuten nach dem Schließen des letzten Fensters) | Das Hauptfenster und das Flyout im Infobereich (Tauri 2, SolidJS + TypeScript). Hat keinen eigenen Zustand |
| `bluecaster-driver.exe` | Für eine einzelne Treiberänderung oder einen Schlüsselimport, mit Administratorrechten (UAC) | Weist einem Adapter WinUSB zu, stellt seinen vorherigen Treiber wieder her oder liest die Link Keys, die das Windows-Bluetooth für einen Adapter gespeichert hat (`windows-keys`). Beendet sich danach |

```mermaid
---
config:
  flowchart:
    wrappingWidth: 480
    rankSpacing: 30
---
flowchart TB
    ui["bluecaster.exe:<br>Fenster und Flyout<br>im Infobereich<br>(Tauri + SolidJS)"]
    src["Windows-Audio:<br>System-Loopback /<br>virtuelles Gerät / eine App"]
    subgraph core["bluecaster-core.exe"]
        ctl["Menü im Infobereich /<br>IPC-Server / CLI (--cli)"] --> svc["Kerndienst:<br>Verbindungszustandsautomat,<br>Codec-Auswahl, Profile,<br>Neuverbindung"]
        svc --> cap["WASAPI-Aufnahme"]
        cap -- PCM --> enc["Encoder-Pipeline:<br>LDAC / aptX-Familie / LHDC V5 /<br>Opus / AAC / SBC"]
        enc --> bt["BTstack:<br>HCI / L2CAP / AVDTP /<br>A2DP / AVRCP"]
    end
    ui <-->|"Named Pipe pro Benutzer"| ctl
    src --> cap
    bt -->|"WinUSB (nusb)"| usb["USB-Bluetooth-Adapter<br>(WinUSB-Treiber)"]
    usb -. Bluetooth .-> hp["Kopfhörer / Lautsprecher"]
    usb -.-|SetupAPI| drv["bluecaster-driver.exe<br>(mit Administratorrechten):<br>WinUSB zuweisen / wiederherstellen,<br>Link Keys von Windows lesen"]
```

- **Der Kern ist kein Windows-Dienst.** Er ist ein gewöhnlicher Prozess des
  angemeldeten Benutzers, weil der WASAPI-Loopback den Ton aus der Sitzung dieses Benutzers aufnimmt.
  Ein Mutex pro Sitzung lässt nur eine Instanz zu. Ein erneuter Start öffnet das
  Fenster.
- **Der Infobereich gehört zum Kern.** Sein Rechtsklickmenü (Öffnen, die ersten 10
  Geräte, Trennen, Beenden) öffnet sich sofort, ohne das Fenster zu starten. Ein
  Linksklick startet das Flyout des Fensters (oder das Hauptfenster, je nach
  Einstellungen).
- **Das Fenster zeigt nur an.** Es erhält einen Snapshot des Kernzustands
  und danach die Änderungen. Wird das Fenster geschlossen oder stürzt es ab, läuft
  der Ton weiter. Beim Schließen wird ein Fenster nur ausgeblendet, daher öffnet es sich
  über den Infobereich sofort wieder.
- **Die Befehlszeile verwendet denselben Kern.** `bluecaster-core.exe --cli` führt den
  Kerndienst ohne Infobereich und IPC aus; der Emulatortest läuft auf diese Weise.

## Transport: BTstack + WinUSB

BlueCaster umgeht den Bluetooth-Stack von Windows vollständig und kommuniziert über WinUSB (den von Microsoft
signierten generischen USB-Treiber) direkt mit einem USB-Bluetooth-Adapter. BTstack,
ein Bluetooth-Stack mit offenem Quellcode (nicht kommerzielle Lizenz), implementiert HCI, L2CAP, AVDTP und A2DP im
Benutzermodus.

**Vorteile**:
- Keine Kosten für eine Treibersignatur
- Kein Testsignaturmodus erforderlich
- Secure Boot muss nicht deaktiviert werden
- Der gesamte Code läuft im Benutzermodus (einfacheres Debugging)

**Nachteile**:
- Erfordert einen eigenen USB-Bluetooth-Adapter (getrennt vom integrierten
  Bluetooth von Windows)
- Solange WinUSB den Adapter belegt, steht er dem normalen Windows-Bluetooth
  nicht zur Verfügung
- Die Geräteadresse wird von Hand eingegeben oder per Suche (GAP Inquiry) über
  den USB-Adapter gefunden

**Funktionsweise**:
1. Einem USB-Bluetooth-Adapter wird WinUSB zugewiesen, in der App (**Adapter >
   Durch WinUSB-Treiber ersetzen**, siehe [Treiberzuweisung](#driver-assignment)) oder mit Zadig
2. Das Crate `hci-transport` öffnet den Adapter über WinUSB mit `nusb`
   und verbindet ihn als HCI-Transport mit BTstack
3. BTstack sendet HCI-Befehle, um den Bluetooth-Controller zu initialisieren
4. (Realtek-, Intel- und Broadcom-Adapter) Bei Bedarf wird Firmware hochgeladen
5. BTstack baut eine ACL-Verbindung auf und öffnet L2CAP-Kanäle (PSM 0x0019)
6. Die AVDTP-Signalisierung ermittelt die SEPs der Gegenseite und handelt die Codecs aus
7. Das kodierte Audio wird als AVDTP-Medienpakete über L2CAP gesendet

```mermaid
sequenceDiagram
    participant App as BlueCaster (BTstack)
    participant Ad as USB-Adapter
    participant Sink as Kopfhörer
    App->>Ad: Öffnen über WinUSB,<br>HCI Reset / Init
    opt Realtek, Intel, Broadcom
        App->>Ad: Firmware-Upload
    end
    App->>Sink: ACL-Verbindung
    Note over App,Sink: SSP Just Works bei der ersten Verbindung<br>(Link Key gespeichert)
    App->>Sink: L2CAP-Kanal, PSM 0x0019<br>(AVDTP-Signalisierung)
    App->>Sink: AVDTP DISCOVER / GET_CAPABILITIES
    Sink-->>App: Stream-Endpunkte und Codecs
    App->>Sink: SET_CONFIGURATION<br>(gewählter Codec), OPEN, START
    loop Während des Streamings
        App->>Sink: AVDTP-Medienpakete über L2CAP
    end
```

Der USB-Transport übernimmt die Wiederherstellungsschritte, die echte Adapter schon in
v1 brauchten: Beim Öffnen des Adapters werden die Event- und ACL-Pipes zurückgesetzt, ein fehlgeschlagener
Event-Lesevorgang wird wiederholt, und HCI-Befehle haben eine Zeitüberschreitung. Beim Schließen des Adapters wird
HCI Reset gesendet (außer an einen Controller, der noch in seinem Bootloader ist). So findet die nächste
Sitzung einen Controller im Ruhezustand vor.

## Crate-Struktur

Ein Cargo-Workspace (`Cargo.toml` im Wurzelverzeichnis des Repositorys) in fünf Schichten.
Jede Schicht hängt nur von den Schichten unter ihr ab. Die Bibliotheken liegen in
`app/src/crates/`, die Programme in `app/src/core`, `app/src/ui` und
`app/src/driver`. Das übernommene `aptx-adaptive` ist eine Pfadabhängigkeit von
`codec` und gehört nicht zum Workspace. Nur die `*-sys`-Crates und `win-sys` dürfen `unsafe`-Code
enthalten. Alle anderen Crates haben `#![forbid(unsafe_code)]`.

| Schicht | Crate | Aufgabe |
|:--------|:------|:--------|
| 1. FFI | `btstack-sys` | Baut BTstack mit `cc` (MSVC), handgeschriebene FFI-Deklarationen und sichere Wrapper, `btstack_config.h`. BTstack darf nur auf dem Thread seiner Run Loop aufgerufen werden. Das `!Send`-Token `Stack` stellt das sicher |
| 1. FFI | `ldac-sys`, `openaptx-sys`, `fdkaac-sys`, `lhdcv5-sys`, `opus-sys`, `lc3-sys` | Bauen die C-Encoder und stellen sie bereit. Ein Crate pro Bibliothek, damit die Lizenzen getrennt bleiben |
| 1. FFI | `win-sys` | Win32-APIs ohne Rust-Bibliothek: MMCSS, das Standard-Audiogerät (IPolicyConfig), die WLAN-API (AFH), Energiebenachrichtigungen, der Run-Schlüssel, Medientasten, die DACL der Pipe, Rechteerhöhung, die WinUSB-Treiberzuweisung (SetupAPI) und das Windows-Bluetooth (seine Funkmodule, gekoppelten Geräte und Link Keys) |
| 2. Bausteine | `hci-transport` | HCI über USB („H2“) mit `nusb` und H4 über TCP zu einem virtuellen Controller (`tools/emu`) |
| 2. Bausteine | `aptx-adaptive` | Der aptX-Adaptive-Encoder in Rust (übernommen aus AKS-Logic/aptx-adaptive-linux, MIT) |
| 2. Bausteine | `codec` | Der Trait `Encoder`, der SBC-Encoder (Rust-Portierung des Encoders aus Bluedroid), sichere Wrapper der C-Encoder, Auswertung der Codec Information und die Codec-Wahl |
| 2. Bausteine | `audio-capture` | WASAPI-Aufnahme (Crate `wasapi`): System-Loopback, virtuelles Gerät, eine Anwendung (Prozess-Loopback); Geräte- und App-Listen, Testton, Lautstärke pro App (Lautstärkemixer) |
| 2. Bausteine | `voice-link` | Der Ton des Headset-Mikrofons (Entwicklermodus): Mono-PCM an das gewählte Ausgabegerät (WASAPI), die heruntergemischte Aufnahme für die Gegenrichtung und die Sprachcodecs CVSD / mSBC / LC3-SWB über SCO-Nutzdaten |
| 3. Engine | `bt-host` | Führt BTstack auf einem eigenen Thread aus, gesteuert durch Befehle und typisierte Ereignisse: GAP, A2DP Source, AVRCP, SDP, RFCOMM-Kanäle für das Passthrough (`rfcomm.*`), Link Keys, die SDP-Dienste eines Geräts, AFH, Laden der Firmware und Chip-Loader (`bringup`), die Chip-Abfrage und Chip-Skripte, HCI-Aufzeichnung (`.pklg`), LE Audio (`le/`) |
| 2. Bausteine | `le-audio` | LE Audio: die Logik ohne BTstack oder Windows (Assigned Numbers, LTV, PDUs von PACS / ASCS, Presets, die Modelle für CSIP / VCP / MCP / CCP) |
| 3. Engine | `pipeline` | PCM-Ringpuffer, Formatumwandlung, Downmix, Kodierung, Mediennutzdaten, Taktung, ABR |
| 4. Kern | `bluecaster-core` (Verzeichnis `core`) | Der Zustandsautomat, Neuverbindungen, Energiesparmodus / Fortsetzen, Profile, Einstellungen und die Migration von v1, Lautstärke-Sync, die Stummschaltung der Ausgabe, Updateprüfung, Download der Realtek-Firmware, der Client für den Empfängertest, Statistik, Herstellerprotokolle (`vendor/`) |
| 4. Kern | `ipc` | Die Protokolltypen, die Kern und Fenster gemeinsam nutzen (serde; TypeScript-Typen mit `ts-rs` generiert), und die Rahmung auf der Pipe |
| 5. Programme | `bluecaster-core-exe` (`app/src/core`) | `bluecaster-core.exe`: Kern, Infobereich (`tray-icon` auf einer `tao`-Ereignisschleife), IPC-Server, `--cli` |
| 5. Programme | `bluecaster-ui` (`app/src/ui`) | `bluecaster.exe`: Tauri 2; die Rust-Seite ist ein IPC-Client, die Seite ist SolidJS + TypeScript + Vite |
| 5. Programme | `bluecaster-driver` (`app/src/driver`) | `bluecaster-driver.exe`: das Hilfsprogramm mit Administratorrechten |

## Audiopfad und Threads

```mermaid
---
config:
  flowchart:
    wrappingWidth: 480
    rankSpacing: 30
---
flowchart TD
    out["Systemaudio-Ausgabe<br>(Loopback / virtuelles Gerät / eine App)"]
    out --> cap["Thread capture:<br>WASAPI, ereignisgesteuert (meist float32, 44,1–96 kHz)"]
    cap -->|"sperrfreier<br>Ringpuffer (PCM)"| enc["Thread encode:<br>Umwandlung, Downmix, Encoder,<br>Mediennutzdaten + RTP-Header, ABR"]
    enc -->|"sperrfreier<br>Ringpuffer (Pakete)"| stack["Thread bt-runloop: BTstack → AVDTP → L2CAP → HCI"]
    stack --> usb["hci-transport → WinUSB → USB-Bluetooth-Adapter → Funk"]
    usb -.-> hp["Kopfhörer / Lautsprecher"]
```

| Thread | Priorität | Aufgabe |
|:-------|:----------|:--------|
| capture | MMCSS "Audio" | Liest WASAPI; füllt stille Abschnitte mit Stille auf |
| encode | MMCSS "Pro Audio" | Wandelt in das Format des Encoders um, kodiert ganze Encoder-Blöcke (der Rest wartet auf die nächste Runde) und packt Frames in Mediennutzdaten. Arbeitet in Runden von etwa 10 ms, damit die Pakete gleichmäßig gesendet werden |
| bt-runloop | -- | Die Run Loop von BTstack. Sendet ein wartendes Paket, sobald L2CAP senden kann. Bei jedem neu eingereihten Paket verwirft die Sendewarteschlange (64 Pakete) Pakete, die 350 ms (100 ms bei LHDC V5 mit niedriger Latenz) gewartet haben, die ältesten zuerst |
| USB | -- | WinUSB-Übertragungen: drei laufende Lesevorgänge auf jeder IN-Pipe (Events, ACL-Daten), ein Schreiber für Befehle und einer für ACL-Daten (bis zu 8 laufende Übertragungen) |
| core | Normal | Zustandsautomat, IPC (tokio; die Warteschlangen des RFCOMM-Passthrough pro Verbindung), Einstellungen, Statistik (4 Hz), Updateprüfung |

Sobald ein Stream läuft, reserviert der Audiopfad keinen Speicher und setzt keine Sperren.
Die Statistik besteht aus atomaren Zählern, die der Kern viermal pro Sekunde liest.
BTstack ist nicht threadsicher. Andere Threads schicken ihm Befehle, die auf
dem Thread bt-runloop ausgeführt werden.

## Kern und IPC

Das Fenster kommuniziert mit dem Kern über die Named Pipe des Benutzers
`\\.\pipe\bluecaster-<user SID>`, deren DACL nur diesen Benutzer zulässt. Nachrichten sind
JSON-Frames mit vorangestellter Länge:

1. Das Fenster sendet `hello { protocol, client }`. Ein Kern mit einer anderen
   Protokollversion antwortet mit `reject`, und das Fenster meldet, dass die beiden
   Programme aus verschiedenen Versionen stammen. Andernfalls antwortet der Kern mit
   `welcome { protocol, server, features }`. `features` nennt Erweiterungen des
   Protokolls (`rfcomm`).
2. Der Kern sendet einen `snapshot`: seinen gesamten Zustand (Adapter, Geräte,
   Stream, Profile, Einstellungen, Update) und eine Revisionsnummer.
3. Danach sendet der Kern `event`s mit den Änderungen. Fehlt eine Revision,
   fordert das Fenster einen neuen Snapshot an.
4. Aktionen sind `request { id, method, params }` mit einer `response { id,
   result | error }`. Die Methodengruppen sind `state.*`, `stream.*`,
   `device.*`, `profile.*`, `settings.*`, `adapter.*`, `audio.*`, `diag.*`,
   `receiver.*`, `rfcomm.*` und `app.*`.

Andere Programme verwenden dieselbe Pipe. Was sie dafür brauchen, steht unter
[Integration (IPC)](integration). Vor 3.0.0 hieß die Pipe
`\\.\pipe\a2dpwb-<user SID>`. Der Kern bedient diesen Namen noch mindestens 6
Monate nach 3.0.0.

Fehler sind Codes mit Parametern (zum Beispiel `adapter.driver_missing`), keine
Texte. Das Fenster übersetzt sie und bietet die Lösung direkt neben der Meldung an.
Häufig aktualisierte Daten (Verbindungsqualität, Protokoll) werden nur gesendet, solange eine Seite
sie abonniert hat.

Der Zustandsautomat der Verbindung:

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Connecting: Start
    Connecting --> Streaming: Stream gestartet
    Connecting --> Idle: gestoppt
    Connecting --> Error: Fehler bei Init /<br>Verbindung / Codec
    Streaming --> Reconnecting: Verbindung verloren
    Reconnecting --> Streaming: wieder verbunden
    Reconnecting --> Error: Neuverbindung<br>fehlgeschlagen
    Streaming --> Idle: Stopp
    Error --> Connecting: erneut starten
```

Ein fehlgeschlagener Verbindungsaufbau wird bis zu 3-mal versucht. Eine abgebrochene Verbindung wird bis zu
3-mal wiederhergestellt, nach einem Neustart des Stacks (Energiesparmodus, ein Adapter,
der nicht mehr reagierte) bis zu 10-mal. Wechselt der PC in den Energiesparmodus, stoppt der Kern BTstack, ohne
den Verbindungsverlust als Fehler zu zählen, und verbindet sich nach dem Aufwachen erneut.

## Treiberzuweisung {#driver-assignment}

Windows 8 und neuer enthalten den von Microsoft signierten WinUSB-Treiber
(`winusb.inf`). `bluecaster-driver.exe` macht über SetupAPI dasselbe wie im Geräte-Manager
„Treiber aktualisieren > Aus einer Liste auswählen > WinUsb Device“:

1. Den Adapter über seine Geräteinstanz-ID öffnen.
2. Einen Wert `DeviceInterfaceGUIDs` in seine `Device Parameters` schreiben, damit
   WinUSB eine Geräteschnittstelle für ihn registriert.
3. Das Modell „WinUsb Device“ aus `%SystemRoot%\INF\winusb.inf` auswählen und
   mit `DiInstallDevice` installieren. Der mitgelieferte Treiber ist signiert, daher wird kein
   Zertifikat installiert (anders als bei Zadig / libwdi, die eine erzeugte
   INF mit einem selbst erstellten Zertifikat signieren).
4. **Wiederherstellen**: das Gerät entfernen (`DiUninstallDevice`) und neu erkennen lassen.
   Windows wählt dann wieder seinen am besten bewerteten Treiber (normalerweise `BthUSB` oder den
   Treiber des Herstellers). Der vorherige Treiber des Adapters wird im
   Einstellungsordner (`driver\`) gespeichert und nach der Wiederherstellung damit verglichen.
   `restore --records <folder>` stellt alle gespeicherten Adapter in einem einzigen
   Lauf mit Administratorrechten wieder her. Das Deinstallationsprogramm nutzt das nach einer Rückfrage.

Der Kern läuft nie als Administrator. Für eine Zuweisung oder Wiederherstellung startet er
das Hilfsprogramm mit einer UAC-Abfrage (`ShellExecuteEx` "runas") und liest dessen
JSON-Ergebnis. Das Auflisten der Adapter und ihrer Treiber benötigt keine Administratorrechte.

```mermaid
sequenceDiagram
    participant Core as bluecaster-core
    participant Drv as bluecaster-driver (mit Administratorrechten)
    participant Win as Windows (SetupAPI)
    Core->>Drv: Start mit UAC-Abfrage
    alt Zuweisen
        Drv->>Win: DeviceInterfaceGUIDs<br>in Device Parameters
        Drv->>Win: winusb.inf<br>(WinUsb Device) installieren
    else Wiederherstellen
        Drv->>Win: Gerät entfernen und neu aufzählen
        Win->>Win: wählt seinen besten Treiber wieder
    end
    Drv-->>Core: Ergebnis (JSON)
```

## Wichtige Module

### Codec-Wahl (`codec::caps`)

- Wertet die Fähigkeiten der Gegenseite aus: die standardisierten Elemente für SBC und AAC sowie die
  Hersteller-Codecs, einschließlich aptX LL unter beiden Vendor IDs und aptX Adaptive
  (R1- und R2-Senken; nur mit 48 kHz Stereo nutzbar)
- Wählt den Codec: Automatisch nimmt den Codec mit der höchsten Klangqualität, den die
  Gegenseite anbietet. Bietet die Gegenseite einen angeforderten Codec nicht an, tritt der Codec an seine Stelle,
  den Automatisch wählen würde (bei einem Profil seit Issue #86 immer, in der
  Befehlszeile nur mit `--fallback`). Andernfalls ist das ein Fehler. Ein Codec,
  der auf seiner Karte gewählt wurde, als die Gegenseite ihn nicht anbot (`codec_forced`
  des Profils), wird trotzdem ohne Ausweichen angefordert (`RemoteCaps::forcing`).
  Ein Codec, dessen Karte mehr anbietet, als die Gegenseite aufführt
  (`unlisted_options`), wird aus `RemoteCaps::unlisted` konfiguriert
- Streamt jedes A2DP-Profil wie Automatisch mit dessen Standardeinstellungen, solange
  die Einstellung `auto_codec` eingeschaltet ist (Codecs > Automatisch den unterstützten Codec mit der
  höchsten Qualität wählen; seit Schema 5 eine Einstellung für alle Geräte). Der für das Gerät
  gewählte Codec bleibt in seinem Profil erhalten, für den Fall, dass die Einstellung ausgeschaltet wird
- Folgt der Codec-Reihenfolge, die der Benutzer durch Ziehen der Karten des Geräts festgelegt hat
  (`codec_order` des Profils; `codec::AutoOrder`, leer
  für höchste Klangqualität zuerst). Das gilt für die Wahl von Automatisch und für das Ausweichen
  eines gewählten Codecs, auch bei automatischer Auswahl. FastStream und Opus
  05 werden nie automatisch gewählt und sind nie Ausweichcodec. (Die 2.0.2-Betas hatten eine
  Reihenfolge für alle Geräte, `auto_order` in settings.json, die jetzt ignoriert wird.)
- Wählt die Abtastrate für die aptX-Familie (44,1 / 48 kHz aus den Raten, die die Gegenseite
  meldet; WASAPI rechnet die Aufnahme um). aptX Adaptive verwendet 48 kHz, oder
  96 kHz, wenn 96 kHz gewählt ist, die Qualität Hoch ist und die Senke R2 ist und
  96 kHz aufführt. Eine Abtastrate auf Automatisch (auch bei automatischer Auswahl) fordert 48 kHz an.
  Führt die Senke 48 kHz nicht auf, nimmt die Auswahl jedes Codecs die beste aufgeführte Rate (Issue #86: nicht
  die höchste aufgeführte Rate und nicht die Mischrate von Windows)
- Ordnet die Qualität des Profils den Qualitäten zu, die der Codec anbietet
  (`Quality::for_codec`, Issue #86): LDAC und LHDC V5 Hoch / Mittel
  (`sq`) / Niedrig (`mq`), LHDC V5 außerdem Mittel-hoch (`mhq`), aptX Adaptive
  Hoch / Niedrige Latenz, jeder andere Codec Hoch (die höchste Bitrate, die das Gerät
  annimmt). Eine Qualität, die der Codec nicht mehr hat (Standard / Mobil bei aptX Adaptive,
  die niedrigeren Stufen von AAC), läuft als Hoch
- Speichert, was das Gerät für jeden angebotenen Codec aufführt (`KnownCodecs.caps`:
  Abtastraten, Bittiefen, maximale Bitrate; `RemoteCaps::listed_*`), damit die
  Codec-Einstellungen nur diese Werte anbieten. Den Bitpool-Bereich von SBC hat BlueCaster schon vorher gespeichert

### Bluetooth-Host (`bt-host`)

**Aufgaben**:
- BTstack mit dem USB- (oder TCP-)HCI-Transport auf einem eigenen Thread ausführen
- Stream-Endpunkte registrieren: Hersteller-Codecs (LDAC, aptX HD, aptX, aptX LL,
  LHDC V5, FastStream, Opus, Opus 05, aptX Adaptive) sowie SBC (auch von SBC XQ
  verwendet) und AAC: insgesamt 11 (BTstacks `MAX_NR_AVDTP_STREAM_ENDPOINTS`)
- SDP-Einträge registrieren (A2DP Source, AVRCP Controller, AVRCP Target; HFP
  Audio Gateway nur für den Akkustand und nur, solange diese Einstellung
  eingeschaltet ist)
- SSP-Kopplung verwalten (Just Works, General Bonding; Link Keys werden gespeichert)
- Eine halboffene Verbindung nach einer Zeitüberschreitung beim Verbinden abbauen, damit erneute Versuche funktionieren
- Einen fehlgeschlagenen Verbindungsaufbau wiederholen (bis zu 3 Versuche) und die Ursachen unterscheiden (keine
  Antwort, stumme Verbindung, Gegenseite hat die Verbindung beendet, Authentifizierung). Einen Link Key,
  den das Gerät nicht mehr kennt, verwerfen und erneut koppeln
- GAP Inquiry für die Gerätesuche
- AVRCP Absolute Volume und die Tasten der Kopfhörer
- AFH-Host-Kanalklassifizierung sowie RSSI- / AFH-Werte für die Anzeige der
  Verbindungsqualität
- Laden der Firmware: Realtek und Broadcom über die Chipsatztreiber von BTstack,
  Intel (Legacy- und TLV-Bootloader) in Rust über die gemeinsame Chip-Loader-Schnittstelle
  (`bringup`). Deren Fortschritt zeigt die Seite Adapter an, während der
  Adapter startet
- Entwicklermodus: die Chip-Abfrage (wie ein Adapter auf standardisierte Lesebefehle
  antwortet, als JSON-Bericht in `logs\probe\`), Chip-Skripte (als Text geschriebene Anfragen,
  die auf einem Adapter mit WinUSB laufen) und die SDP-Dienste, die ein verbundenes
  Gerät aufführt
- HCI-Aufzeichnung (`.pklg`), einschließlich des Verbindungsaufbaus, wenn sie während einer Verbindung beginnt

**Wichtige verwendete BTstack-APIs**:
- `a2dp_source_create_stream_endpoint()` — Codec-Endpunkte registrieren
- `a2dp_source_establish_stream()` — Mit der A2DP-Senke verbinden
- `a2dp_source_set_config_other()` — Herstellerspezifischen Codec konfigurieren
- `a2dp_source_stream_send_media_payload_rtp()` — Kodiertes Audio mit einem
  RTP-Header senden (LDAC, aptX HD, AAC, SBC, SBC XQ, LHDC V5, Opus)
- `a2dp_source_stream_send_media_packet()` — Rohe Medienpakete ohne
  RTP-Header senden (aptX, aptX LL, FastStream) oder mit dem eigenen RTP-Header
  von BlueCaster (aptX Adaptive)

### Audioaufnahme (`audio-capture`)

Nimmt den Ton in Echtzeit über die Windows Audio Session API auf.
- System-Loopback des Standardausgabegeräts, Loopback eines gewählten
  (virtuellen) Geräts oder Prozess-Loopback einer Anwendung und ihrer untergeordneten
  Prozesse (Windows 10 Build 19041 oder neuer)
- Liefert das Mixformat des Geräts im gemeinsamen Modus (meist float32; die
  Pipeline wandelt es für den Encoder um)
- Wartet im Modus Anwendung, bis die Anwendung läuft, und nimmt sie nach einem
  Neustart wieder auf
- Stellt Lautstärke und Stummschaltung einer App auf der Standardausgabe ein, wie der
  Lautstärkemixer von Windows (`mixer`). Die Lautstärke einer Sitzung wirkt sich auf die
  Loopback-Aufnahme aus, auch wenn der Kern diese Ausgabe während eines Streams mit System-Loopback
  stummschaltet (`bluecaster-core::mute_output`)

### Herstellerprotokolle (`bluecaster-core::vendor`)

Manche Ohrhörer bieten einen Codec oder eine Bitrate erst an, nachdem die App des Herstellers über
den RFCOMM-Dienst der Ohrhörer einen Modus umgeschaltet hat. Ein Modul pro Hersteller erledigt das über die
Verbindung von BlueCaster und erkennt die Ohrhörer an ihrem Namen:

- **Huawei / Honor**: Verlangt ein Profil LDAC und führen die Ohrhörer LDAC nicht auf,
  schaltet das Modul ihren Modus „Klangqualität zuerst“ ein und verbindet sich neu.
  Bringt das kein LDAC, wird der Modus bei Ohrhörern, bei denen er vorher aus war, wieder ausgeschaltet. Nie
  bei Automatisch
- **OPPO**: Streamt LHDC V5 (gewählt oder durch Automatisch), fordert das Modul einmal Hi-Res
  an und verbindet sich einmal neu, um zu lesen, was die Ohrhörer danach aufführen. Dieser
  Vermerk wird bei den Codecs des Geräts (`device_codecs.json`) gespeichert, bis
  `device.forget_codecs` ihn löscht

Auf der Registerkarte Hersteller im Entwicklermodus legen Sie fest, wann jedes Modul läuft (Automatisch nach Name,
Immer, Nie). Dort finden Sie auch die eigenen Optionen des Herstellers.

### Kopplungen (`link_keys`, `win-sys::bt_keys`)

Link Keys werden in `link_keys.txt` gespeichert. Beim Hinzufügen von Geräten kann der Kern
die Schlüssel übernehmen, die das Windows-Bluetooth für den Adapter gespeichert hat, den BlueCaster jetzt verwendet.
`bluecaster-driver.exe windows-keys` liest sie (eine UAC-Abfrage). Schlüssel, die BlueCaster
schon hat, bleiben erhalten, und die Audiogeräte unter den übernommenen werden unter ihrem Namen in
Windows gespeichert. Der Entwicklermodus zeigt den Link Key eines Geräts und die SDP-Dienste,
die es aufführt.

Die Schlüsseldateien in `pairing\` (Link Keys, LE-Bonds, koordinierte Sets,
von Windows übernommene LE-Schlüssel, ersetzte Schlüssel von Windows) sind mit
DPAPI für den Windows-Benutzer verschlüsselt (`win-sys::dpapi`). Andere
Benutzer können sie nicht lesen, und auf einen anderen PC kopiert sind sie ebenfalls unlesbar.
Klartextdateien früherer Versionen werden gelesen und verschlüsselt neu
geschrieben. Eine Datei, die sich nicht entschlüsseln lässt, bleibt als
`<Name>.undecryptable` erhalten, und ihre Geräte werden neu gekoppelt. Den
LE-TLV von BTstack verwaltet BlueCaster selbst (der `TlvStore` von `btstack-sys`,
mit dem Inhalt, den `btstack_tlv_windows` geschrieben hat). `keys_for_windows.json`,
das der Helfer mit Administratorrechten beim Wiederherstellen liest, bleibt
Klartext (der Helfer kann als anderer Administrator laufen) und wird gelöscht,
sobald der Helfer beendet ist.

### Audio-Encoder

| Encoder | Bibliothek | Bitrate | Merkmale |
|---------|------------|---------|----------|
| LHDC V5 | lhdcv5-enc (C-Portierung des AOSP-Encoders) | 400/500/900/1000 kbps (Niedrig/Mittel/Mittel-hoch/Hoch), höchstens das von der Senke aufgeführte Maximum, ABR 160–400 kbps | 16/24 Bit, 5-ms-Frames, 2 Byte Header der Mediennutzdaten, RTP-Header; niedrige Latenz, wo die Senke sie aufführt (kodiert alle 10 ms statt alle 20; bei ausgeschaltetem ABR kommt der Warteschlangenschutz unten hinzu) |
| LDAC | libldac (AOSP) | 330/660/990 kbps | Modi Hoch/Mittel/Niedrig (HQ/SQ/MQ), ABR |
| aptX Adaptive | aptx-adaptive (reiner Rust-Encoder von AKS-Logic/aptx-adaptive-linux, MIT) | 427 kbps Hoch, 265 kbps Niedrige Latenz (R2-Senken: 270); ABR stuft auf 364/318 herunter (R2: 270/204) | 48 kHz Stereo (96 kHz in Hoch, wenn gewählt, an eine R2-Senke, die es aufführt: 646/340/255 kbps), 24 Bit; ein Codec-Paket pro Medienpaket hinter dem eigenen RTP-Header von BlueCaster; ABR in Hoch; Automatisch nimmt es nach LHDC V5 und LDAC |
| Opus | libopus 1.6.1 | 256 kbps (Stereo; Mono die Hälfte) | A2DP Opus von Android: 48 kHz, 20-ms-Frames (10 ms, falls die Senke nur diese hat), Constrained VBR, begrenzt auf die Mediennutzdaten, ein Frame pro Paket, 1 Byte Header der Mediennutzdaten, RTP-Header |
| aptX HD | libopenaptx | 576 kbps | 24-Bit-Eingang, feste Rate, RTP-Header |
| aptX | libopenaptx | 352/384 kbps (44,1/48 kHz) | 16 Bit Stereo, kein RTP-Header |
| aptX LL | libopenaptx | 352 kbps | ca. 32 ms Latenz, kein RTP-Header, Pakete ≤ ca. 7,5 ms |
| AAC | fdk-aac | bis 256 kbps (höchstens die von der Senke aufgeführte Bitrate) | AAC-LC, LATM-Transport |
| SBC XQ | derselbe SBC-Encoder | ca. 385/505/552 kbps | Dual Channel, Bitpool 47 / 43 bei 44,1 / 48 kHz (Hoch); nur wenn das SBC der Senke Dual Channel und einen Bitpool von mindestens 29 erlaubt (der Bitpool wird dann wie bei PipeWire auf das Maximum der Senke begrenzt); teilt sich mit SBC den Stream-Endpunkt, den Header der Mediennutzdaten und den RTP-Header |
| SBC | Rust-Portierung des Bluedroid-Encoders | bis ca. 345 kbps | Verpflichtende A2DP-Grundlage; bitgenau zum C-Encoder (über alle Konfigurationen getestet) |
| FastStream | derselbe SBC-Encoder | 198/216 kbps (44,1/48 kHz) | CSR-Hersteller-Codec, nur Musikrichtung (kein Sprachrückkanal): SBC Joint Stereo Bitpool 29, Frames auf 72 Byte aufgefüllt, 3 pro Paket, kein Header der Mediennutzdaten und kein RTP-Header (wie bei PipeWire); wird von Automatisch nie gewählt |

**LHDC-V5-Warteschlangenschutz** (niedrige Latenz an, ABR aus): Solange die
Sendewarteschlange im Mittel über eine Sekunde 12 oder mehr Pakete enthält,
senkt der Encoder die Bitrate um eine Stufe (1000, 900, dann 500 kbps), mit
mindestens 2 s Abstand. Nach einer ruhigen Minute erhöht er sie wieder um eine
Stufe. Hält eine Erhöhung keine 30 s, verdoppelt sich die Wartezeit, bis zu
10 Minuten.

Alle Encoder implementieren den Trait `codec::Encoder`. Sie geben das PCM-Format
und die Anzahl der Frames pro Aufruf an und liefern Codec-Frames zurück.

libopenaptx erwartet Gruppen von 4 Stereo-Samples als gepacktes 24-Bit-Little-Endian-PCM.
Die aptX-Wrapper wandeln die 16-Bit- oder 32-Bit-Samples in dieses Format um,
und ein Mono-Eingang wird auf beide Kanäle kopiert.

## Codec Information Elements der Hersteller-Codecs

Nicht standardisierte Codecs werden in AVDTP als Vendor Specific registriert:

| Codec | Vendor ID | Codec ID |
|-------|-----------|----------|
| LHDC V5 | Savitech (0x0000053A) | 0x4C35 |
| LDAC | Sony (0x0000012D) | 0x00AA |
| aptX Adaptive | Qualcomm (0x000000D7) | 0x00AD |
| Opus | Google (0x000000E0) | 0x0001 |
| aptX HD | Qualcomm (0x000000D7) | 0x0024 |
| aptX | APT (0x0000004F) | 0x0001 |
| aptX Low Latency | CSR (0x0000000A) oder Qualcomm (0x000000D7) | 0x0002 |
| FastStream | CSR (0x0000000A) | 0x0001 |
| Opus 05 | The Linux Foundation (0x000005F1, PipeWire) | 0x1005 |

AAC und SBC verwenden die in der A2DP-Spezifikation definierten Standard-Codec-IDs.

Größen der Codec Information Elements (einschließlich der 6 Byte für Vendor ID / Codec ID):

- **aptX**: 7 Byte (Byte 6 = Abtastrate / Kanalmodus)
- **aptX HD**: 11 Byte (Byte 6 wie bei aptX, dazu 4 reservierte Byte); Stereo ist
  erforderlich
- **aptX LL**: 8 Byte, oder 17 Byte, wenn die Senke das erweiterte Flag
  („new caps“) setzt. BlueCaster registriert einen aptX-LL-Endpunkt und konfiguriert den
  Stream mit der Vendor ID, die die Senke verwendet
- **LHDC V5**: 11 Byte (Android `a2dp_vendor_lhdcv5_constants.h`): Byte 6
  Abtastraten (0x20 44,1k, 0x10 48k, 0x04 96k, 0x01 192k), Byte 7 Bittiefe
  (0x04 16 Bit, 0x02 24 Bit) und die minimale/maximale Bitrate der Senke, Byte 8
  Codec-Unterversion (0x01 = V5 ver.1) und 0x10 = 5-ms-Frames, Byte 9-10
  optionale Funktionen (Low Latency, Lossless, JAS, AR, META). BlueCaster fordert
  Low Latency an (Byte 9, 0x40), wenn in den LHDC-V5-Einstellungen Niedrige Latenz
  eingeschaltet ist und die Senke sie aufführt. Die anderen Funktionen bietet
  BlueCaster nicht an. Nur Stereo. Die Bitratengrenzen der Senke werden in der
  Konfiguration zurückgegeben und auf den Encoder angewendet, wie bei Android. Medienpakete
  tragen einen 2 Byte langen Nutzdaten-Header: Frame-Anzahl << 2 (Latenz-Bits 0) und eine
  Sequenznummer
- **FastStream**: 8 Byte (PipeWire `a2dp_faststream_t`): Byte 6 Richtung
  (0x01 Sink = Musik, 0x02 Source = Sprache), Byte 7 im unteren Nibble die Raten der Senke
  (0x2 44,1k, 0x1 48k), im oberen Nibble die Rate der Quelle (0x2 16k). BlueCaster bietet
  und konfiguriert nur die Musikrichtung
- **Opus**: 7 Byte (Android `a2dp_vendor_opus_constants.h`): Byte 6 Bits
  2..0 Kanalmodus (0x01 Mono, 0x02 Stereo, 0x04 Dual Mono), Bits 4..3
  Frame-Dauer (0x08 10 ms, 0x10 20 ms), Bit 7 48 kHz. BlueCaster bietet Mono
  und Stereo, 10 und 20 ms an und konfiguriert 20 ms Stereo (Mono nur bei einem
  Mono-Eingang an eine Senke, die Mono unterstützt). Medienpakete tragen ein Opus-Paket hinter
  einem 1 Byte langen Nutzdaten-Header (Frame-Anzahl 1 in den unteren 4 Bits)
- **aptX Adaptive**: 40 Byte. Das Capability-Ereignis von BTstack kürzt die
  Codec Information der Senke auf 30 Byte. Diese enthalten trotzdem alles, was BlueCaster braucht.
  SET_CONFIGURATION: 48 kHz mit Source-Typ 2 (Host-Encoder: die Senke steuert das Timing
  der Wiedergabe selbst), Stereo. Eine R1-Senke erhält ihre eigenen Bytes für die
  Ziellatenz zurück, wobei LL0 auf mindestens 80 ms angehoben und eine HQ-Latenz von 0
  durch 200 ms ersetzt wird. Eine R2-Senke erhält den eigenen R2-Block der Quelle

**RTP oder kein RTP**: Die Medienpakete von LDAC, aptX HD, AAC, SBC, SBC XQ, LHDC V5 und Opus tragen den
12 Byte langen RTP-Header. aptX, aptX LL und FastStream werden **ohne** RTP-Header gesendet (wie bei Android und
PipeWire), sodass die gesamte L2CAP-MTU für aptX-Frames zur Verfügung steht.

**aptX Adaptive** sendet ein Codec-Paket pro Medienpaket hinter dem
eigenen 12 Byte langen RTP-Header von BlueCaster (BTstack fügt keinen hinzu): Payload-Typ 96,
Sequenznummer +1 pro Paket, Zeitstempel = Samples pro Kanal im Paket (keine Uhr),
SSRC = Latenzmodus der Senke (0x000000A3 HQ, 0x000000A1 LL). Die Nutzdaten werden in
16-Bit-Wörtern byteweise vertauscht. HQ-Pakete sind 656 Byte (12 ms), LL-Pakete 348 Byte
(10 ms) groß. Automatisch wählt aptX Adaptive nach LHDC V5 und LDAC, aber vor klassischem aptX, Opus und AAC
(die Reihenfolge aus Issue #86).

## Einstellungen und Dateien {#settings-and-files}

Alles liegt im Einstellungsordner `%APPDATA%\BlueCaster` (oder in dem
Ordner, den die Umgebungsvariable `BLUECASTER_CONFIG_DIR` angibt). Die Dateien aus
v1 und 2.0.x behalten ihr Format (2.0.x verwendete `%APPDATA%\A2DPWB`; 3.0.0
benennt diesen Ordner beim ersten Start um und sortiert die Dateien in die
unten genannten Ordner):

| Pfad | Inhalt |
|:-----|:-------|
| `settings.json` | App-Einstellungen, mit einer `schema_version`; unbekannte Schlüssel bleiben erhalten |
| `profiles.json` | Profile |
| `devices\devices.txt` | Bekannte Geräte |
| `devices\le_audio_devices.txt` | LE Audio: die LE-Audio-Einträge der Geräteliste |
| `devices\le_audio_sets.txt` | LE Audio: die koordinierten Sets der gespeicherten Geräte (nur die Adressen der Mitglieder; beide Ohrhörer eines Paars sind ein Gerät) |
| `devices\device_codecs.json` | Was jedes klassische Gerät bei seiner letzten Verbindung angeboten hat (und ob OPPO-Ohrhörer nach Hi-Res gefragt wurden) |
| `pairing\link_keys.txt` | Link Keys: Mit v1 oder 2.0.x gekoppelte Kopfhörer verbinden sich ohne erneute Kopplung wieder |
| `pairing\le_audio_bonds.tlv`, `pairing\le_audio_csip.txt` | LE Audio: LE-Bonds (mit den dauerhaften CCC-Werten des GATT-Servers) und koordinierte Sets |
| `pairing\windows_le_keys.txt`, `pairing\replaced_windows_keys.json` | Von Windows übernommene LE-Schlüssel, bis ein Stack mit LE sie aufnimmt; die Schlüssel von Windows, die **Originaltreiber wiederherstellen** ersetzt hat |
| `adapters\adapter_caps.json` | Was ein Stack über jeden Adapter erfahren hat (Unterstützung für LE Audio, Adresse, HCI-Version, sein Name in Windows) |
| `adapters\driver\<instance id>.json` | Der vorherige Treiber jedes Adapters, dem WinUSB zugewiesen wurde |
| `firmware\` | Firmware-Dateien (`*.bin`, `*.hcd`, `ibt-*`; die von Intel auch in `firmware\intel\`, wie in linux-firmware). Dateien, die frühere Versionen direkt im Einstellungsordner abgelegt haben, werden hierher verschoben |
| `logs\<YYYY-MM-DD>\core.log`, `ui.log`, `driver.log` | Die Protokolle des Kerns, des Fensters und des Treiberhelfers, ein Ordner pro Tag (UTC); nicht maskiert |
| `logs\hci\*.pklg` | HCI-Mitschnitte: die des Flugschreibers, die beim Fehlschlagen eines Streams oder beim Abbruch einer Verbindung geschrieben werden (die neuesten 10), und die, die Sie selbst starten (`hci_<date>_<time>.pklg`) |
| `logs\probe\` | Berichte der Chip-Abfrage und der Chip-Skripte (die neuesten 10) |
| `logs\crash\` | Absturzberichte: ein Minidump (`*.dmp`) und eine Textdatei pro Absturz (die neuesten 5 pro Programm) |
| `logs\bundles\*.zip` | Diagnosepakete (die neuesten 3) |
| `backup\v1\` | Eine Kopie der Einstellungen von v1, einmalig beim ersten Start von v2 angelegt |
| `backup\v2\` | Eine Kopie der Einstellungen und Kopplungen von 2.0.x, einmalig beim ersten Start von 3.0.0 angelegt |
| `backup\profiles-before-codecs.json` | `profiles.json` im Zustand, bevor die Seite Codecs mehrere Profile eines Geräts zu einem zusammengeführt hat (einmalig geschrieben) |

Der Programmordner ist `%LOCALAPPDATA%\BlueCaster\`: die drei Programme, das
Deinstallationsprogramm und `licenses\` (`licenses\README.txt`, `licenses\bluecaster\LICENSE...`,
`licenses\third-party\...`). Der Cache für Firmware-Downloads ist `%TEMP%\bluecaster-cache`.

## Build-System

- **Cargo** baut die Programme, CMake ist dafür nicht nötig. Jedes `-sys`-Crate
  kompiliert in `build.rs` seine C-Bibliothek aus dem Git-Submodul in `extern/` mit MSVC.
  Nur das Entwicklerwerkzeug `tools/a2dp_decode` wird mit CMake gebaut
- Die Seite des Fensters wird mit **Vite** gebaut (`pnpm build` in `app/src/ui`) und
  von Tauri in `bluecaster.exe` eingebettet
- Die TypeScript-Typen des IPC-Protokolls werden aus den Rust-Typen generiert
  (`cargo test -p ipc export_bindings`)
- Das Fenster und das Menü des Kerns im Infobereich verwenden die
  Sprachdateien (`app/lang/*.json`)
- `installer/build-release.ps1` erstellt das Installationsprogramm (Inno Setup, `installer/BlueCaster.iss`),
  das portable ZIP und das Updater-Manifest. Siehe [Bauen](building)

## Wichtige Hinweise

- **Adapter-Kompatibilität**: Realtek (am besten getestet), CSR, Intel, Broadcom, MediaTek
  und weitere ([Geprüfte Hardware](hardware)). Realtek-Adapter benötigen beim Start einen Firmware-Upload,
  ebenso Intel-Adapter im Bootloader-Modus
- **Kopplung**: Verwendet SSP Just Works mit General Bonding. Link Keys werden
  in einer lokalen Datei gespeichert oder für den Adapter aus dem Windows-Bluetooth übernommen
- **Zweiter Adapter empfohlen**: Lassen Sie das integrierte Bluetooth bei Windows und verwenden Sie
  einen eigenen USB-Adapter für BlueCaster
- Die Firmware mancher Bluetooth-Adapter begrenzt die erreichbare Bitrate
- USB-Adapter mit Bluetooth 5.0+ funktionieren mit LDAC im Allgemeinen gut
- Priorität der automatischen Codec-Auswahl (nach Klangqualität, Issue #86): LHDC V5 > LDAC > aptX Adaptive > Opus > aptX HD > aptX > aptX LL > AAC > SBC XQ > SBC (FastStream und Opus 05 nur, wenn ausdrücklich gewählt). Bietet das Gerät einen gewählten Codec nicht mehr an, weicht BlueCaster auf den ersten angebotenen Codec in der Reihenfolge des Geräts aus. Ein Codec, der auf seiner Karte gewählt wurde, als das Gerät ihn nicht anbot, wird trotzdem angefordert (`codec_forced`, kein Ausweichen)

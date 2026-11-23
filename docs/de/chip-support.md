---
title: Unterstützung für Chips hinzufügen
layout: default
parent: Startseite
nav_order: 3.5
---

# Unterstützung für Chips hinzufügen

BlueCaster steuert den Bluetooth-Adapter direkt (WinUSB). Es muss daher selbst erledigen, was sonst der Windows-Treiber des Herstellers tut, damit der Adapter funktioniert. Bei vielen Chips heißt das: bei jedem Start eine Firmware laden. Adapter von Realtek, Intel, Broadcom und CSR funktionieren bereits, ebenso die MediaTek-Chips MT7925, MT7927, MT7922, MT7921, MT7902 und MT7663 und die Qualcomm-Chips QCA61x4, QCA9377, WCN6855, WCN785x und AR30xx (MT7927, MT7922, MT7902, MT7663 und die Qualcomm-Chips sind noch nicht auf der Hardware getestet). Die übrigen MediaTek-Chips (ältere Modelle) und Qualcomm-Chips (QCA6290) funktionieren noch nicht, weil noch niemand ihren Loader geschrieben hat. Für einen weiteren MediaTek-Chip können Sie auf dem MT79xx-Loader in `app/src/crates/bt-host/src/mediatek.rs` aufbauen.

Diese Seite erklärt, wie Sie einen Loader hinzufügen. Dafür brauchen Sie einen Adapter mit dem betreffenden Chip – die Maintainer haben keinen. Auch ohne selbst Code zu schreiben, können Sie helfen: Die Schritte 1 und 2 liefern den Bericht, den ein Entwickler braucht.

{: .note }
Das Konzept dahinter und die offenen Punkte stehen in der [Entwicklernotiz](../chip-bringup-dev).

## Inhalt
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Was Sie brauchen

- Den Adapter, angeschlossen an einen PC mit Windows 10 / 11
- BlueCaster 2.0.2 oder neuer (für die folgenden Schritte genügt die App)
- Um den Loader zu schreiben: eine Build-Umgebung wie unter [Bauen](building) beschrieben sowie [USBPcap](https://desowin.org/usbpcap/) mit [Wireshark](https://www.wireshark.org/), um mitzuschneiden, was der Windows-Treiber tut

## Schritt 1: Den Adapter an BlueCaster übergeben

1. **Einstellungen > Entwicklermodus**: einschalten
2. **Adapter**: Klicken Sie auf der Karte des Adapters auf **Durch WinUSB-Treiber ersetzen** (Windows fragt nach Administratorrechten). Bei einem Chip, den BlueCaster noch nicht verwenden kann, ist diese Schaltfläche nur im Entwicklermodus aktiv
3. Steht auf der Karte **Neu einstecken oder Windows neu starten**, folgen Sie diesem Hinweis

Solange der Adapter WinUSB verwendet, kann das Windows-Bluetooth ihn nicht nutzen. Ist dieser Chip das einzige Bluetooth eines Notebooks, funktionieren über Windows verbundene Tastaturen, Mäuse und Headsets erst wieder, wenn Sie den Treiber zurückgeben. Dazu dient **Originaltreiber wiederherstellen** auf derselben Karte.

## Schritt 2: Auslesen, was der Chip meldet (Chip-Abfrage)

Klicken Sie auf der Karte des Adapters im Bereich **Firmware** auf **Chip-Infos lesen**. Der Bluetooth-Stack hält kurz an, die Abfrage läuft, und der Explorer zeigt den Bericht `probe-<date>_<time>.json` in `logs\probe\` im Einstellungsordner (`%APPDATA%\BlueCaster`). Die 10 neuesten Berichte werden aufbewahrt.

Die Abfrage liest nur. Sie sendet weder HCI Reset noch Herstellerbefehle:

| Anfrage | Was sie verrät |
|:--|:--|
| Read Local Version Information | HCI- / LMP-Version, den Hersteller (Company ID), LMP-Subversion: oft die ROM- oder Firmwareversion |
| Read BD_ADDR | Die Adresse des Adapters (bei manchen Chips null oder ein Platzhalter, solange ihre Firmware nicht läuft) |
| Read Local Supported Commands / Features, Read Buffer Size | Was der laufende Code unterstützt: Ein ROM unterstützt oft viel weniger als die Firmware |
| Read Local Name | Manche ROMs nennen ihren Namen |
| LE Read Local Supported Features, LE Read Buffer Size | Dasselbe für LE |
| USB-Gerätedeskriptor, Hersteller- und Produkt-Strings | Standard-USB-Anfragen auf der Default Pipe |

Außerdem zeichnet sie die USB-Konfiguration auf, die Windows meldet (Interfaces, Endpoints), sowie jedes Event, das der Chip unaufgefordert gesendet hat. Auch eine Anfrage ohne Antwort wird festgehalten: Dass ein Chip vor dem Laden der Firmware nicht antwortet, ist bereits ein Befund.

Über die Befehlszeile, mit der VID:PID oder Instanz-ID des Adapters (die Adapterkarte zeigt beide an):

```bat
"%LOCALAPPDATA%\BlueCaster\bluecaster-core.exe" --cli -u 0e8d:7961 --probe
```

Beenden Sie BlueCaster vorher über den Infobereich, da App und Befehlszeile den Adapter nicht gleichzeitig nutzen können.

**Einen Bericht senden**: Nach **Chip-Infos lesen** zeigt die Karte des Adapters die gelesenen Daten und eine Schaltfläche **Auf GitHub melden**. Sie öffnet ein neues Issue, in dem Version, Adapter, diese Zusammenfassung, Ihr Windows-Build und der Bericht selbst bereits eingetragen sind. Ergänzen Sie, woher der Adapter stammt (bei einem eingebauten Adapter: das Notebookmodell).

Ist der Bericht für das Issue zu lang, bleibt das Feld **Probe report** leer, und der Explorer zeigt stattdessen die Datei. Ziehen Sie die Datei in dieses Feld; ohne sie erstellt GitHub das Issue nicht. Die Berichte liegen in `%APPDATA%\BlueCaster\logs\probe`; der neueste hat das jüngste Datum im Namen. Der Bericht enthält die Bluetooth-Adresse und den Namen des Adapters, aber nichts über Ihre Kopfhörer. Ein **Diagnosepaket** (Seite **Diagnose**) mit eingeschlossenen HCI-Mitschnitten enthält die Abfrageberichte ebenfalls, unter `probe/`.

## Schritt 3: Mitschneiden, was der Windows-Treiber tut

Eine Abfrage zeigt den Zustand des Chips, aber nicht, wie man ihn ändert. Das weiß der Windows-Treiber des Herstellers. Schneiden Sie einmal mit, was er tut, und der Loader macht es ihm nach.

1. Klicken Sie auf der Karte des Adapters auf **Originaltreiber wiederherstellen**, damit wieder der Windows-Treiber den Adapter steuert
2. Installieren Sie USBPcap und Wireshark. Starten Sie Wireshark als Administrator und zeichnen Sie das USBPcap-Interface auf, an dem der Adapter hängt. `USBPcapCMD.exe` (im Ordner von USBPcap) listet die Geräte an jedem Root Hub auf. Der eingebaute Adapter eines Notebooks hängt an einem internen Hub und erscheint oft nur als „USB Composite Device“ oder mit seiner VID:PID. Zeichnen Sie im Zweifel alle USBPcap-Interfaces gleichzeitig auf
3. Bringen Sie den Treiber dazu, die Firmware zu laden: Ziehen Sie den Adapter ab und stecken Sie ihn wieder ein. Bei einem eingebauten Adapter wählen Sie im Geräte-Manager **Gerät deaktivieren** und dann **Gerät aktivieren**. Firmware, die einen Neustart übersteht, wird eventuell erst nach vollständigem Herunterfahren neu geladen
4. Beenden Sie die Aufzeichnung, sobald der Adapter in den Bluetooth-Einstellungen von Windows erscheint. Suchen Sie unter den ersten Paketen den Gerätedeskriptor des Adapters (seine VID:PID), filtern Sie nach seiner Adresse (`usb.bus_id == <bus> && usb.device_address == <address>`) und speichern Sie die Aufzeichnung

Worauf Sie achten sollten, in dieser Reihenfolge:

- **Control Transfers** vor jedem HCI-Befehl: `bmRequestType` 0x40 / 0xC0 (Vendor, OUT / IN) mit ihren Werten `bRequest`, `wValue`, `wIndex`. Sie lesen oft eine Chip-ID aus oder setzen einen Teil des Chips zurück
- **HCI-Befehle** (Class Requests, `bmRequestType` 0x20) mit Opcodes von 0xFC00 bis 0xFFFF: Herstellerbefehle. Eine lange Folge desselben Opcodes mit großen Parametern ist die Übertragung der Firmware. Die Antworten darauf zeigen, wie jeder Teil bestätigt wird
- **Events**, die weder Command Complete noch Command Status sind: Hersteller-Events (Code 0xFF oder ein eigener Code des Herstellers wie 0xE4), zum Beispiel „Firmware gestartet“ oder die Bestätigung jedes Firmwareteils anstelle eines Command Complete
- **Bulk-OUT**-Daten vor dem ersten HCI Reset: Manche Chips nehmen die Firmware über die ACL-Pipe entgegen
- Woher die Datei stammt: Vergleichen Sie die gesendeten Daten mit den Dateien im Ordner des Treibers (Geräte-Manager > Treiberdetails, oder `C:\Windows\System32\DriverStore\FileRepository\`) und mit den Dateien des Chips in [linux-firmware](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/) (`mediatek/`, `qca/`), die weitergegeben werden dürfen und von BlueCaster-Nutzern heruntergeladen werden können

Notieren Sie jede Pause, die der Treiber zwischen den Schritten einlegt (siehe Zeitstempel der Aufzeichnung). Ein Chip, der gerade zurückgesetzt wird, antwortet nicht.

{: .warning }
Arbeiten Sie mit Aufzeichnungen, Abfrageberichten und Herstellerdokumentation. Kopieren Sie keinen GPL-Code und orientieren Sie sich nicht eng daran, etwa an den Linux-Kernel-Treibern `btmtk` oder `btqca`. BlueCaster steht unter Apache-2.0 oder MIT, und ein davon abgeleiteter Loader könnte nicht aufgenommen werden.

## Ohne Build ausprobieren: Chip-Skripte {#chip-scripts}

Bevor Sie einen Loader schreiben, probieren Sie am Adapter aus, was die Aufzeichnung gezeigt hat. Ein **Chip-Skript** beschreibt den Ablauf als Text, eine Anfrage pro Zeile, und läuft anstelle des Bluetooth-Stacks. Wer den Adapter nicht hat, kann ein Skript schreiben und an jemanden schicken, der ihn hat. Das Ergebnis kommt als Bericht zurück, ganz ohne neuen Build.

**Ein Skript ausführen**: Schalten Sie den Entwicklermodus ein und öffnen Sie **Entwickler > Chip**. Wählen Sie den Adapter (mit WinUSB), geben Sie das Skript ein oder öffnen Sie eine Datei und klicken Sie auf **Ausführen**. Die Antwort auf jede Zeile erscheint darunter. **Bericht anzeigen** öffnet `script-<date>_<time>.json` in `logs\probe\` (der Bericht enthält auch das Skript). Über die Befehlszeile: `bluecaster-core --cli -u <adapter> --script <file>`.

```text
# eine Anfrage pro Zeile; Zahlen sind hexadezimal (0x optional), außer bei wait, timeout= und chunk=
hci 1001                           # ein HCI-Befehl: Opcode, dann Parameterbytes
hci fc05 ff
bulk fc09 00 01 02                 # dasselbe über die Bulk-OUT-Pipe
usb in vendor device 63 0 0 4      # Control IN: Typ, Empfänger, Request, Wert, Index, Länge
usb out vendor device 64 0 0 01    # Control OUT: ..., dann die Datenbytes
wait 100                           # Millisekunden
expect e4 02 01 timeout=5000       # ein Event: sein Code und seine ersten Parameterbytes
bootloader off                     # Bootloader-Modus (von Anfang an ein)
upload mediatek/patch.bin fc6f 01 01 chunk=240 ack=event:e4:02,01
upload-bulk patch.bin fc09 01      # dasselbe über die Bulk-OUT-Pipe
```

- `hci` und `bulk` warten auf Command Complete oder Command Status des Befehls (2 s) und zeichnen die Antwort unabhängig vom Status auf
- `upload` sendet eine Datei aus dem Firmware-Ordner in Befehlen mit diesem Opcode, jeder mit den Header-Bytes und bis zu `chunk` Bytes der Datei (Standard 240), und wartet jeweils auf die Bestätigung: `ack=complete` (Standard: ein Command Complete oder Command Status des Opcodes mit Status 0) oder `ack=event:<code>[:<bytes>]`. Ein Fehlerstatus bricht den Upload ab
- Das Skript hält bei der ersten fehlgeschlagenen Zeile an (keine Antwort, ein Fehler, eine fehlende Datei) und nennt sie
- Events, die zu keiner Zeile gehören, stehen ebenfalls im Bericht

**Sicherheit**: Ohne Rückfrage laufen nur Anfragen, die bekanntermaßen nur lesen: die Standard-HCI-Lesebefehle (informative und Statusparameter, Read Local Name, die LE-Lesebefehle), Standard-USB-Deskriptoranfragen, `wait`, `expect` und `bootloader`. Ein Skript mit einer anderen Zeile läuft nur, wenn Sie es erlauben (**Ausführen erlauben** unter der Liste dieser Zeilen; `--allow-writes` in der Befehlszeile). Herstellerbefehle, die Speicher beschreiben können, dessen Inhalt ein Chip auch nach dem Aus- und Wiedereinschalten behält, laufen nie, egal was Sie erlauben (`denied_opcodes` in `app/src/crates/bt-host/src/chips.rs`: bisher BCCMD von CSR). Der Bootloader-Modus ist von Anfang an eingeschaltet, daher sendet BlueCaster beim Schließen des Adapters danach kein HCI Reset. In den RAM eines Chips geladene Firmware ist nach erneutem Einstecken wieder weg.

## Schritt 4: Den Loader schreiben

Ab hier sind Pfade relativ zum Wurzelverzeichnis des Repositorys. Drei Crates sind wichtig:

| Crate (Paket) | Ordner | Inhalt |
|:--|:--|:--|
| `bt-host` | `app/src/crates/bt-host` | Der Bluetooth-Host: Loader, die Abfrage, Chip-Erkennung |
| `bluecaster-core` (Bibliothek) | `app/src/crates/core` | Einstellungen und Adapter: welcher Loader läuft |
| `bluecaster-core-exe` | `app/src/core` | Das Dienstprogramm (`bluecaster-core.exe`): die Anfragen des Fensters, Hinweise zu Adaptern |

Die letzten beiden haben „core“ im Namen. `cargo -p` erwartet die oben genannten Paketnamen.

Ein Loader ist ein Zustandsautomat ohne eigene I/O. Er erhält die Events des Controllers, die Ergebnisse seiner USB-Anfragen und seinen Timer und gibt **Aktionen** zurück, die die App ausführt. So lässt er sich ohne den Chip testen, und Details zu USB und Threading bleiben außen vor.

1. Kopieren Sie `app/src/crates/bt-host/src/bringup/sample.rs`, einen vollständigen Loader für einen erfundenen Chip samt Tests, nach `app/src/crates/bt-host/src/<vendor>.rs` (zum Beispiel `mediatek.rs`). Fügen Sie `pub mod <vendor>;` in `app/src/crates/bt-host/src/lib.rs` ein, alphabetisch bei den anderen Modulen
2. Behalten Sie die Namen bei: Der Typ heißt `Loader` (verwendet als `<vendor>::Loader`), `vendor()` gibt den Namen des Herstellers zurück. Lassen Sie die Tests wie im Beispiel in der Datei
3. Ersetzen Sie alles, was das Beispiel als „made up“ markiert, durch das, was die Schritte 2 und 3 gezeigt haben: die Versionsprüfung, die USB-Anfrage, den Dateinamen, den Download-Befehl und wie er bestätigt wird, wie die Firmware gestartet wird
4. Schreiben Sie den Modulkommentar am Anfang neu: was der Chip braucht, die Schritte in ihrer Reihenfolge und woher jede Angabe stammt (Ihre Aufzeichnung des Windows-Treibers, Herstellerdokumentation, Abfrageberichte). Nennen Sie nicht den Quellcode anderer Treiber als Quelle eines Schritts

### Die Schnittstelle

Alles steht in `app/src/crates/bt-host/src/bringup.rs`.

| `ChipLoader`-Methode | Aufgerufen |
|:--|:--|
| `vendor()` | Für Protokollzeilen („MediaTek“) |
| `start()` | Einmal, auf dem geöffneten Adapter, vor der HCI-Initialisierung |
| `on_event(event)` | Für jedes HCI-Event: Code, Länge, Parameter |
| `on_control(result)` | Mit dem Ergebnis des letzten `UsbControl`: die gelesenen Bytes oder der Grund des Fehlschlags |
| `on_timeout()` | Wenn der Timer des letzten `Timer` abläuft |
| `missing_file_hint(file)` | Für das Protokoll, wenn der Loader mit `MissingFile` endete: was wohin gehört („x.bin from linux-firmware (mediatek/)“) |
| `step()`, `progress()` | Nach jedem Stapel von Aktionen: der Name des Schritts („download“) und wie weit er ist (gesendete von allen Befehlen), für die Seite Adapter, während der Adapter startet |

Nach `Done` verwirft die App den Loader. Eine verspätete Antwort erreicht ihn also nie.

| `Action` | Wirkung |
|:--|:--|
| `Command(bytes)` | Sendet einen HCI-Befehl (Opcode, Länge, Parameter) über die Control Pipe; wird in der HCI-Aufzeichnung protokolliert |
| `BulkCommand(bytes)` | Sendet einen Befehl, dieselben Bytes wie bei `Command`, über die Bulk-OUT-Pipe, über die manche Bootloader die Firmware entgegennehmen |
| `UsbControl(request)` | Ein Control Transfer auf der Default Pipe (Herstellerregister); wird sofort ausgeführt, `on_control` erhält das Ergebnis vor der nächsten Aktion. Wird in der HCI-Aufzeichnung als Notiz protokolliert |
| `Bootloader(on)` | Für einen Chip, dessen Loader über die Bulk-IN-Pipe antwortet oder kein HCI Reset bekommen darf (manche hängen sich dabei auf); siehe die Muster. Ein ROM-Chip, der normal auf HCI antwortet, braucht es nicht |
| `Timer(ms)` | Startet den einzigen Timer des Loaders oder startet ihn neu |
| `Done(Ok(outcome))` | Fertig: Die HCI-Initialisierung folgt. `AlreadyRunning`, `Loaded { firmware, detail }`, `Rom` (`Probed` gehört der Abfrage) |
| `Done(Err(error))` | Der Adapter kann nicht verwendet werden: `MissingFile(name)`, `BadFile { file, why }`, `Command { opcode, status }`, `Timeout(step)`, `Rejected(code)`, `BadVersion`, `UnsupportedVariant(v)`, `Unsupported(why)`, `Host(why)` |

Hilfsfunktionen, damit ein Loader keine Bytes von Hand zerlegen muss:

- `hci_command(opcode, params)` baut einen Befehl
- `parse_event(event)` liefert `HciEvent::Complete { opcode, status, ret }` (Rückgabeparameter nach dem Status), `Status { opcode, status }`, `Vendor(params)` (Event 0xFF) oder `Other { code, params }` (jeder andere Code, auch ein eigener des Herstellers wie 0xE4)

**Firmwaredateien**: Der Loader erhält einen `FileReader` und fragt ihn nach einem **Dateinamen ohne Ordner** (`"BT_RAM_CODE_MT7961_1_2_hdr.bin"`). Die App sucht im Firmware-Ordner (`%APPDATA%\BlueCaster\firmware`) und in dessen Unterordner für den Hersteller (`firmware\mediatek\`), so wie linux-firmware die Dateien anordnet. Schritt 6 legt diesen Unterordner fest.

### Muster

Das Beispiel zeigt alle diese Muster. Lassen Sie weg, was Ihr Chip nicht braucht.

- **Ein Befehl, eine Bestätigung**: senden, den Timer starten und im Wartezustand nur die Bestätigung dieses Opcodes annehmen: ein Command Complete oder, bei einem Befehl, dessen Ergebnis später kommt, ein Command Status mit Status 0. Alles andere wird ignoriert, damit eine verspätete Antwort den Loader nicht versehentlich weiterschaltet
- **Bestätigung durch ein Hersteller-Event**: Manche Chips beantworten jeden Firmwareteil mit einem eigenen Event statt mit einem Command Complete. Dann ist dieses Event die Bestätigung. Warten Sie nur darauf (anhand seines Codes und der Parameter, die es kennzeichnen). Ignorieren Sie in diesem Zustand ein Command Complete oder einen Command Status mit Status 0 dieses Befehls, brechen Sie aber bei einem Fehlerstatus in einem der beiden ab. Das Beispiel behandelt Command Complete und Command Status als dieselbe Bestätigung, daher braucht sein `on_event` dafür ein `match` auf Zustand und Event
- **Mehrere Dateien** (ein Patch, dann eine Konfiguration): Alle laden und prüfen, bevor irgendetwas gesendet wird. Dann nacheinander senden und die letzte in `Loaded { firmware, .. }` nennen (die anderen nennen die Protokollzeilen)
- **Eine bewusste Pause** (der Treiber wartet 100 ms vor dem nächsten Schritt): ein eigener Zustand dafür und `Timer(100)`; In diesem Zustand sendet `on_timeout` den nächsten Schritt, statt fehlzuschlagen. In den Tests zählt `SimRun::timeouts` auch solche Pausen
- **Ein Befehl mit zwei Antworten** (eine Bestätigung, dann ein Hersteller-Event wie „Firmware gestartet“): zwei Zustände, der zweite mit eigenem Timer. So machen es `Launching` und `Starting` im Beispiel. Enthält das Event einen Ergebniscode, beendet ein Fehlercode den Loader mit `Rejected(code)`
- **Ein Fehlerstatus** in einem Command Complete oder Command Status beendet den Loader mit `Command { opcode, status }`
- **Bootloader-Modus**: `Bootloader(true)` vor dem ersten Befehl, noch vor der Versionsprüfung. Bei Erfolg (`Loaded` und auch `AlreadyRunning`) direkt vor `Done` `Bootloader(false)` senden, damit die HCI-Initialisierung normal funktioniert. Nach einem Fehler eingeschaltet lassen, da der Chip dann noch in seinem Bootloader ist. `finish()` im Beispiel erledigt das
- **Läuft bereits**: Zeigen Version oder Register, dass die Betriebsfirmware läuft, sofort mit `AlreadyRunning` beenden. Der Adapter behält seine Firmware, bis er keinen Strom mehr hat, und ein zweiter Download kann fehlschlagen
- **Die Datei zuerst prüfen**: die ganze Datei zerlegen und prüfen, bevor etwas davon gesendet wird (`BadFile` mit dem Grund), und genau die fehlende Datei nennen
- **Nur Ihr Chip**: Deckt der Loader nur einige Chips eines Herstellers ab, prüfen Sie vor dem Senden, ob der Adapter dazugehört (über das eigene Chip-ID-Register oder die USB-IDs mit `ControlRequest::device_descriptor()`), und beenden Sie andernfalls mit `Unsupported`
- **Protokollieren** Sie, was Sie vom Chip erfahren, auf Stufe `info` (Chip-ID, ROM-Version, die gewählte Datei). Genau das zeigen später die Berichte
- Schreiben Sie nie in den Flash- oder OTP-Speicher des Chips, und lesen Sie Dateien nur über den `FileReader`

## Schritt 5: Gegen einen simulierten Controller testen

Die Tests verwenden `app/src/crates/bt-host/src/bringup/sim.rs`. Sie schreiben einen kleinen simulierten Chip (`impl SimController`), der Befehle und Control Transfers so beantwortet wie der echte Chip in Ihrer Aufzeichnung. `sim::run(&mut loader, &mut chip)` steuert den Loader genau wie die App, einschließlich des Timers. Das Ergebnis (`SimRun`) enthält das Resultat, jeden gesendeten Befehl mit seiner Pipe (`commands[i].bulk`), jeden Control Transfer, den Bootloader-Modus am Ende (`bootloader`) und wie oft der Timer abgelaufen ist. `sim::command_complete`, `command_status`, `vendor_event`, `event` (beliebiger Code), `opcode` und `params` bauen und lesen die Bytes. Bulk-Befehle beantworten Sie in `bulk_command`.

Decken Sie wie das Beispiel mindestens ab:

- den vollständigen Download, mit Prüfung der gesendeten Befehle (Anzahl, Reihenfolge, Größen, Pipe)
- den Bootloader-Modus am Ende jedes Falls, wenn der Loader ihn verwendet
- bereits laufende Firmware
- eine fehlende und eine beschädigte Datei (von der nichts gesendet wird)
- einen abgelehnten Befehl (Fehlerstatus), einen stummen Controller (Timeout) und jedes Warten, das in ein Timeout laufen kann
- einen fehlgeschlagenen USB-Transfer, wenn der Loader einen verwendet

Bytes aus Ihrer Aufzeichnung sind die besten Testdaten. Ausführen mit:

```bat
cargo test -p bt-host
```

## Schritt 6: In die App einbinden

Hier stehen alle Dateien, die Sie ändern müssen, für einen Hersteller, der bereits als nicht unterstützt geführt wird (MediaTek, Qualcomm), oder für einen neuen. Bei einem neuen Hersteller zeigt der Compiler die `match`-Ausdrücke, die seine Variante brauchen. Alles Übrige, und bei einem bereits geführten Hersteller alles, finden Sie nur in dieser Liste.

**Ein Chip oder der ganze Hersteller?** Ein Loader deckt meist nur einige Chips eines Herstellers ab (den MT7921, aber nicht den MT7925). Dann dürfen nur diese Chips den Status „nicht unterstützt“ verlieren: Tragen Sie ihre VID:PIDs in `LOADER_CHIPS` in `app/src/crates/bt-host/src/chips.rs` ein und lassen Sie den Hersteller in `listed_chips_only()`. `chips::chip_supported(vendor, vid, pid)` und `chip_support_tag()` antworten dann pro Adapter. Danach richten sich das Namensetikett des Adapters, sein Hinweis auf der Seite Adapter, ob dort WinUSB angeboten wird, und die Warnung „not implemented“. Nehmen Sie einen Hersteller nur dann aus `listed_chips_only()`, wenn der Loader alle seine Chips abdeckt (wie der von Intel).

{: .note }
Behalten Sie die Zeilenenden jeder Datei bei. Manche Quelldateien verwenden CRLF, andere LF. Manche Editoren und `sed -i` in Git Bash ändern sie unbemerkt, und `cargo fmt` stellt sie nicht wieder her. `git diff --stat` macht das sichtbar: Bei einer Datei, die Sie nur an wenigen Stellen geändert haben, zählt dann jede Zeile als geändert. Wandeln Sie die Datei dann vor dem Commit zurück (zum Beispiel mit dem Zeilenende-Schalter in der Statusleiste von VS Code).

**Erkennung** (`app/src/crates/bt-host/src/chips.rs`):

1. `ChipVendor`: Ein neuer Hersteller bekommt eine Variante (mit Doc-Zeile) und einen Zweig in `name()`. Aktualisieren Sie für jeden Hersteller den Doc-Kommentar des Enums (welche Hersteller Loader haben) und die Doc-Zeile der Variante („not supported“)
2. Einige Chips: ihre VID:PIDs in `LOADER_CHIPS` und ein Test im Modul des Loaders, dass seine eigene Chipprüfung (Chip-ID, Version) genau diese Einträge akzeptiert. Der ganze Hersteller: aus `listed_chips_only()` nehmen
3. `vendor_for_usb()`: die USB-VIDs des Herstellers. Eine VID, die auch andere Hersteller verwenden, muss als `VID if pid == PID` abgeglichen werden, nie als Ganzes: Notebook-Module unter der VID des Modulherstellers (AzureWave 0x13D3, Foxconn 0x0489) und geteilte VIDs wie pid.codes 0x1209. Jede hier aufgeführte VID lässt außerdem `is_bluetooth_vendor_vid` die Geräte dieses Herstellers als Bluetooth-Adapter behandeln. Eine VID:PID in der Realtek-Tabelle von BTstack oder in `OEM_TABLE` (beide in `resolve_realtek_pid` dieser Datei) gilt als Realtek, bevor `vendor_for_usb` überhaupt gefragt wird. Prüfen Sie, dass Ihre VID:PID in keiner der beiden steht
4. `company_name()`: die Company ID der Bluetooth SIG aus Read Local Version, falls sie noch fehlt
5. Tests: `app/src/crates/bt-host/tests/chips.rs` (die VID:PIDs und das Etikett)

**Den Adapter öffnen**: Geräte der Bluetooth-Klasse (Klasse E0/01/01) werden direkt geöffnet. Ein Chip, der in seinem ROM eine Herstellerklasse (FF) meldet, braucht zusätzlich seine VID:PIDs in `BUILTIN_VENDOR_CLASS` in `app/src/crates/core/src/adapter.rs`; die Zeile `usb.interfaces` im Abfragebericht zeigt die Klasse.

**Den Loader starten**, wie bei Intel (suchen Sie nach `intel_loader`):

6. `app/src/crates/bt-host/src/lib.rs`: ein `HostConfig`-Feld wie `intel_loader` (mit Standardwert in `HostConfig::new`, der Destrukturierung in `BtHost::launch` und der Debug-Zeile) und der Zweig in `BtHost::launch`, der den Transport einrichtet und statt `setup_hci_and_power_on` die Startfunktion der App aufruft, nur wenn nicht über H4 (`&& !tcp`, wie bei Intel: Der Controller des Emulators ist kein echter Chip). Pro Start läuft nur ein Loader. Sind die Flags zweier Hersteller gesetzt (beide Adapter auf WinUSB), gewinnt der erste Zweig. Testen Sie daher nur mit Ihrem Adapter auf WinUSB
7. `app/src/crates/bt-host/src/app.rs`: ein `start_<vendor>_loader` wie `start_intel_loader` (mit `use crate::<vendor>;`), das dem Loader `read_firmware_file(&dir, "<vendor>", name)` übergibt (der Name des Unterordners wie in linux-firmware) und `start_loader` aufruft
8. `app/src/crates/core/src/adapter.rs`, `configure()`: das Feld setzen, wenn ein Adapter des Herstellers mit `chips::chip_supported(a.vendor, a.device.vid, a.device.pid)` auf WinUSB und nicht in Gebrauch ist (wie `config.intel_loader`, mit Intels Protokollmeldung „in use by another program“ für die anderen), und es dort in die Debug-Zeile `adapter:` aufnehmen. Die Warnung „firmware loading for this chip is not implemented“ darunter richtet sich bereits nach `chip_supported`
9. `app/src/crates/core/src/service.rs`, `host_config()`: das Feld in die Debug-Zeile aufnehmen

**Was das Fenster zeigt**:

10. `app/src/core/src/state.rs`, `vendor_note()`: ein Zweig mit einem neuen Schlüssel `firmware.vendor_<vendor>`, herausgelöst aus dem Zweig `MediaTek | Qualcomm`, und sein Test. Für Chips, die `chip_supported` ablehnt, antwortet die Funktion vorher mit `firmware.vendor_unsupported`, sodass die anderen Chips des Herstellers diesen Hinweis behalten. Die Seite Adapter bietet WinUSB jedem Adapter an, dessen Hinweis nicht `firmware.vendor_unsupported` ist
11. `app/lang/*.json` (alle 10 Sprachen, dieselbe Zeile in jeder): der neue Schlüssel, wie der von Intel („Intel adapter. …“). Wo er eine Schaltfläche oder einen Ordner nennt, verwenden Sie die Wörter der jeweiligen Sprache aus derselben Datei (die Schaltfläche des Firmware-Ordners ist `firmware.open_folder`). Ganzer Hersteller: seinen Namen aus `firmware.vendor_unsupported` entfernen. Einige Chips: dort stehen lassen, da die anderen Chips des Herstellers diesen Text weiterhin zeigen
12. `app/src/ui/src/lib/links.ts`: die Firmware-Seite des Herstellers (in `LINKS`, `firmwareUrl()` und `vendorFirmwareUrl()`, wie Intels Ordner in linux-firmware), damit die Seite Adapter zeigt, woher die Dateien kommen; Tests in `app/src/ui/src/lib/chips.test.ts`. Die Seite Adapter zeigt den Link nur für Adapter, deren Hinweis nicht `firmware.vendor_unsupported` ist, sodass die anderen Chips des Herstellers ihn nicht bekommen

**Dokumentation**:

13. Die Chiptabelle in `docs/setup.md` und `docs/*/setup.md` (alle Sprachen): eine Zeile wie die von Intel, mit den abgedeckten Chips, den Firmwaredateien und wohin sie gehören (der Firmware-Ordner: `%APPDATA%\BlueCaster\firmware` oder sein Unterordner `<vendor>`; ältere Zeilen sagen noch „config folder“). Nennen Sie die Schaltflächen des Fensters so, wie die jeweilige `app/lang/*.json` sie hat (`ui.nav.adapter` für die Seite Adapter, `firmware.open_folder` für die Schaltfläche, die diesen Ordner öffnet). Ganzer Hersteller: aus der Zeile „Not supported“ entfernen. Einige Chips: den Hersteller dort lassen und die weiterhin nicht unterstützten Chips nennen. Ergänzen Sie den Hersteller im Satz am Anfang des Abschnitts Firmware („Intel and Broadcom adapters may need firmware files of their own“)
14. [Geprüfte Hardware](hardware) (`docs/hardware.md`), sobald jemand ihn verwendet hat, und die Einleitung dieser Seite (`docs/chip-support.md`, alle Sprachen), die die Hersteller ohne Loader nennt (einige Chips: „the other <vendor> chips“)

## Schritt 7: Am Adapter ausprobieren

Bauen Sie im Wurzelverzeichnis des Repositorys (`cargo build --release -p bluecaster-core-exe`), beenden Sie die installierte App (über ihr Symbol im Infobereich) und starten Sie den Kern über die Befehlszeile mit einer HCI-Aufzeichnung:

```bat
target\release\bluecaster-core.exe --cli -u 0e8d:7961 --scan 5 --hci-capture loader.pklg
```

Das Protokoll zeigt jeden Schritt. `loader.pklg` öffnet sich in Wireshark und zeigt jeden Befehl, jedes Event und jeden Control Transfer in der richtigen Reihenfolge. Ziehen Sie den Adapter ab (oder fahren Sie den PC herunter) und versuchen Sie es erneut. Der Download muss bei einem kalten Chip funktionieren und darf nichts tun, wenn die Firmware bereits läuft.

{: .note }
Sind mehrere Adapter auf WinUSB, wird ein Realtek-Adapter bevorzugt und seine Chip-Einrichtung angewendet, selbst mit `-u`. Stellen Sie **Adapter > Bluetooth-Chip** auf **Nicht Realtek** oder testen Sie nur mit Ihrem Adapter auf WinUSB.

Streamen Sie danach eine Weile über das Fenster an Kopfhörer und prüfen Sie, ob der Adapter nach **Bluetooth neu starten** und nach erneutem Einstecken wieder läuft. Während der Adapter startet, zeigt die Seite **Adapter** den Loader unter dem Bluetooth-Status an („wird geladen 42 %“ oder einen Fehlschlag). Im Entwicklermodus erscheint er auch, wenn der Adapter bereit ist, mit Ihren `step()`-Namen und dem Ergebnis.

## Schritt 8: Den Pull Request eröffnen

Führen Sie vorher Folgendes in dieser Reihenfolge aus (die Einrichtung steht unter [Bauen](building)). Zuerst die UI in `app\src\ui`. Sie prüft die Änderung aus Schritt 6 (Punkt 12), und der Rust-Workspace enthält die Crate des Fensters, die die gebaute UI braucht:

```bat
pnpm install
pnpm build
pnpm exec tsc --noEmit
pnpm exec vitest run
```

Dann im Wurzelverzeichnis des Repositorys:

```bat
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python tools/check_lang.py
```

Ohne Node.js führen Sie die Tests und Clippy stattdessen für die drei Crates oben aus (`-p bt-host -p bluecaster-core -p bluecaster-core-exe`), überlassen die UI-Prüfungen der CI des Pull Requests und vermerken das im Pull Request. `pwsh ./build.ps1 -Test` führt alles aus.

Clippy prüft auch Namen: Eine `State`-Variante namens `PatchState` scheitert an `enum_variant_names`. Nennen Sie sie `Patch`. `check_lang.py` meldet eventuell „long hints left“, die schon vor Ihrer Änderung da waren. Beheben müssen Sie nur Fehler und fehlende Schlüssel.

An einigen anderen Stellen werden MediaTek oder Qualcomm nebenbei als nicht unterstützt erwähnt (Mock-Daten und Tests der UI, Kommentare in `Adapter.tsx` und `probe.rs`, die Auswahl „Not Realtek“). Diese Stellen bleiben unverändert.

Im Pull Request:

- Basieren Sie ihn auf dem aktuellen Branch `develop-v*`
- Nennen Sie die getesteten Adapter (Produkt, VID:PID, Chip), Kalt- und Warmstart, und hängen Sie einen Abfragebericht und die HCI-Aufzeichnung eines Downloads an
- Nennen Sie, woher die Angaben im Loader stammen

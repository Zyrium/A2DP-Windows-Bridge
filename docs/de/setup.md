---
title: Einrichtung
layout: default
parent: Startseite
nav_order: 1
---

# Einrichtung
{: .no_toc }

## Inhalt
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## Voraussetzungen

- **Windows 10 (22H2) / 11** (x64 oder ARM64)
- **Ein USB-Bluetooth-Adapter für BlueCaster**: Ein separater Dongle wird empfohlen. Ein eingebauter USB-Adapter funktioniert auch, dann hat Windows aber kein eigenes Bluetooth mehr (siehe [Schritt 2](#winusb))

BlueCaster weist dem Adapter den WinUSB-Treiber selbst zu. [Zadig](https://zadig.akeo.ie/) brauchen Sie daher nicht mehr (es funktioniert aber weiterhin, siehe [Stattdessen Zadig verwenden](#zadig)).

## BlueCaster installieren

Jedes [Release](https://github.com/SeiyaFunaokaJP/BlueCaster/releases) gibt es in zwei Varianten mit denselben Dateien:

- **Installationsprogramm** (`BlueCaster-<version>-win64-setup.exe`, auf ARM64 `-arm64-setup.exe`): Installiert für den aktuellen Benutzer ohne Administratorrechte, legt einen Startmenüeintrag (**BlueCaster**) und ein Deinstallationsprogramm an und installiert bei Bedarf die Microsoft Edge WebView2 Runtime (in Windows 11 ist sie bereits enthalten). Updates werden direkt in der App installiert (**Einstellungen > Updates > Jetzt aktualisieren**). Die Programme landen in `%LOCALAPPDATA%\BlueCaster\` (die drei Programme, das Deinstallationsprogramm und ein Ordner `licenses\` mit den Lizenztexten)
- **Portabel** (`BlueCaster-<version>-win64-portable.zip`, auf ARM64 `-arm64-portable.zip`): An beliebiger Stelle entpacken und `bluecaster.exe` starten. Lassen Sie die drei Programme (`bluecaster.exe`, `bluecaster-core.exe`, `bluecaster-driver.exe`) im selben Ordner. Die portable Version weist nur auf Updates hin und setzt eine installierte WebView2 Runtime voraus

Beide Varianten speichern Einstellungen, Profile, Link Keys und Firmware in `%APPDATA%\BlueCaster`. Sie können also zwischen ihnen wechseln. Bei der Deinstallation wird dieser Ordner gelöscht, es sei denn, Sie schalten **Benutzerdaten löschen** aus (eine stille Deinstallation behält ihn).

### Umstieg von v1 {#from-v1}

Führen Sie das Installationsprogramm über v1 aus. Die Seite **Bereit zur Installation** nennt die früheren Versionen, die zuerst deinstalliert werden (v1 oder eine beliebige 2.0.x). Läuft v1 noch, werden Sie aufgefordert, es zu schließen. Danach wird v1 deinstalliert, der Einstellungsordner bleibt aber erhalten: Profile, Kopfhörer-Kopplungen und Firmwaredateien werden übernommen, und Sie müssen die Kopfhörer nicht erneut koppeln. Ein Adapter, der bereits WinUSB hat (über Zadig), behält es. War **Mit Windows starten** eingeschaltet, startet Windows jetzt v2.

Beim ersten Start kopiert v2 die Einstellungen von v1 nach `%APPDATA%\BlueCaster\backup\v1\`, bevor es sie einliest.

### Update von 2.0.x (A2DP Windows Bridge) {#from-2x}

BlueCaster 3.0.0 ist der neue Name von A2DP Windows Bridge. Führen Sie das Installationsprogramm von 3.0.0 über der installierten 2.0.x aus. Das Setup entfernt die alte Version automatisch und behält Ihre Einstellungen. Der Startmenüeintrag und der Eintrag für **Mit Windows starten** heißen danach „BlueCaster“.

Beim ersten Start benennt 3.0.0 den Ordner `%APPDATA%\A2DPWB` in `%APPDATA%\BlueCaster` um und sortiert die Dateien in Unterordner (siehe [Einstellungen und Dateien](architecture#settings-and-files)). `backup\v2\` enthält eine Kopie der Einstellungen und Kopplungen von 2.0.x, falls Sie zu 2.0.2 zurückkehren möchten. Kopieren Sie diese Dateien dazu zurück nach `%APPDATA%\A2DPWB`.

Die Kopfhörer zeigen möglicherweise weiterhin den alten Bluetooth-Namen „A2DPWB“ an, bis Sie sie erneut koppeln.

## Schritt 1: Einrichtung beim ersten Start {#first-run}

Stecken Sie den USB-Bluetooth-Adapter ein und starten Sie BlueCaster. Beim ersten Start (noch kein Audiogerät oder kein Adapter mit WinUSB) öffnet BlueCaster den **Einrichtungsassistent** mit vier Schritten. Nachdem Sie ihn abgeschlossen oder übersprungen haben, öffnet er sich nicht mehr automatisch. Geräte fügen Sie dann unter **Start** oder **Audiogeräte** hinzu.

1. **Adapter auswählen**: Zeigt die gefundenen USB-Bluetooth-Adapter. Fehlt Ihrer, stecken Sie ihn ein und klicken Sie auf **Aktualisieren**. Ein grünes **Unterstützt** oder ein rotes **Nicht unterstützt** zeigt, ob BlueCaster den jeweiligen Adapter verwenden kann. Nicht unterstützte Adapter werden angezeigt, lassen sich aber nicht auswählen
2. **Treiber und Firmware**: Der Schritt zeigt, was er tun wird, und **Diesen Adapter einrichten** führt es aus. BlueCaster ersetzt den Treiber des Adapters durch WinUSB (Windows fragt nach Administratorrechten, siehe [Schritt 2](#winusb)) und lädt die Realtek-Firmwaredateien herunter, die der Adapter braucht (siehe [Firmware](#firmware)). Firmware, die BlueCaster nicht herunterladen kann, müssen Sie von Hand in den Firmware-Ordner kopieren. Hat Windows Geräte über diesen Adapter gekoppelt (etwa eine Tastatur oder Maus am eingebauten Adapter), nennt die Schaltfläche diese zuerst. Solange BlueCaster den Adapter verwendet, funktionieren sie in Windows nicht mehr. Mit **Abbrechen** bleiben sie funktionsfähig. Geräte mit eigenem USB-Empfänger sind nicht betroffen. Um die übrigen Geräte zu behalten, verwenden Sie BlueCaster mit einem separaten USB-Adapter. Auch **Durch WinUSB-Treiber ersetzen** auf der Seite **Adapter** nennt diese Geräte
3. **Audiogeräte finden**: Die Suche startet automatisch. Versetzen Sie das Audiogerät in den Kopplungsmodus und klicken Sie daneben auf **Verbinden** (oder geben Sie seine Adresse ein und klicken Sie auf **Verwenden**). BlueCaster speichert das Gerät mit dem besten Codec, den es unterstützt (Automatisch), und verbindet sich. Das Gerät erscheint dann unter **Gekoppelte Geräte** mit einem Statuspunkt, und die Suche stoppt. **Gerät hinzufügen** trennt es und startet die Suche nach einem weiteren Gerät. Das erste Gerät bleibt gespeichert, und ein falsches Gerät können Sie dort entfernen. **Kopplungen aus Windows übernehmen** erscheint, wenn Windows Geräte über diesen Adapter gekoppelt hat. Die Schaltfläche **Übernehmen** listet diese Geräte auf, alle angehakt (siehe [Schritt 3](#headphones)). **Weiter** funktioniert auch ohne Gerät. Audiogeräte können Sie später unter **Audiogeräte** hinzufügen. Neue Geräte nehmen das **Hi-Fi Cable** auf, wenn es installiert ist, sonst VB-CABLE. Das Hi-Fi Cable verursacht die geringste Verzögerung und wird daher für den Ton empfohlen, VB-CABLE für das Mikrofon des Kopfhörers. Fehlt ein Kabel, zeigt der Schritt eine Warnung, und **Mehr erfahren** listet beide Kabel auf. **Installieren** bei VB-CABLE lädt das VB-CABLE-Paket von VB-Audio herunter (1,3 MB) und installiert es (mit einer Abfrage nach Administratorrechten; erscheint sie nicht, starten Sie Windows neu). **Herunterladen** beim Hi-Fi Cable öffnet die Seite von VB-Audio. Bei einem installierten Kabel steht dort stattdessen **Deinstallieren**. Beide Kabel sind Donationware von VB-Audio ([vb-cable.com](https://vb-cable.com/)), wie auch unter **Info** vermerkt: BlueCaster enthält VB-CABLE nicht, und Spenden gehen an VB-Audio. Unternehmen und andere Organisationen benötigen laut VB-Audio eine Lizenz pro Installation ([Lizenzierung](https://vb-audio.com/Services/licensing.htm)). Findet BlueCaster später zum ersten Mal ein Kabel, nehmen Geräte mit **Standardgerät** dieses Kabel auf (VB-CABLE nur, solange das Hi-Fi Cable nicht installiert ist). Geräte, deren Mikrofon keine funktionierende Ausgabe hat, geben das Mikrofon dort aus. Den Codec können Sie später unter **Codecs** ändern
4. **Allgemeine Einstellungen**: Design, **Start**, **Benachrichtigungen** und Update-Prüfung. Es sind dieselben Einstellungen wie unter Einstellungen, und sie werden sofort gespeichert. Die erste Einrichtung schaltet **Mit Windows starten** und **Im Infobereich starten** ein

**Später einrichten** überspringt die restlichen Schritte. Mit **Einstellungen > Einrichtung ausführen** öffnen Sie die Einrichtung erneut.

## Schritt 2: WinUSB dem Adapter zuweisen {#winusb}

BlueCaster kommuniziert über WinUSB, den in Windows enthaltenen generischen USB-Treiber, direkt mit einem USB-Bluetooth-Adapter. Ein **eigener Adapter** wird empfohlen -- dann funktioniert Ihr integriertes Bluetooth in Windows weiterhin wie gewohnt.

1. Öffnen Sie die Seite **Adapter** (im Einrichtungsschritt **Treiber und Firmware** erledigt **Diesen Adapter einrichten** die Schritte 2 bis 4)
2. Beim **Treiber** des Adapters steht **Originaltreiber** (oder **Kein Treiber**). Das ist der Treiber von Windows oder vom Hersteller, den WinUSB ersetzt. Klicken Sie auf **Durch WinUSB-Treiber ersetzen**
3. Lesen Sie die Bestätigung und fahren Sie fort. Windows fragt nach Administratorrechten (UAC)
4. Beim Treiber steht jetzt **WinUSB-Treiber**

Kann Windows WinUSB nicht sofort laden (weil der Adapter in Gebrauch ist), meldet BlueCaster das, und in der Treiberzeile steht **Neu einstecken oder Windows neu starten**. Ziehen Sie den Adapter ab und stecken Sie ihn wieder ein. Bei einem eingebauten Adapter starten Sie Windows neu. Bis dahin schlägt das Verbinden fehl, und im Protokoll steht `failed to initialize WinUSB (error 50)`.

{: .warning }
Solange WinUSB zugewiesen ist, sieht das Windows-Bluetooth den Adapter nicht mehr. Geräte, die über diesen Adapter mit Windows gekoppelt sind, funktionieren erst wieder, wenn der Treiber wiederhergestellt ist. Verwenden Sie für normale Peripheriegeräte (Tastatur, Maus usw.) Ihr integriertes Bluetooth. Sie können BlueCaster auch einen eingebauten Adapter überlassen, der ein USB-Gerät ist (bei vielen Notebooks ein Realtek-Adapter). Dann hat der PC aber **überhaupt kein Windows-Bluetooth mehr** (Bluetooth-Einstellungen, Mäuse, Tastaturen, andere Audiogeräte), bis Sie den Treiber wiederherstellen.

**Originaltreiber wiederherstellen** auf derselben Seite gibt den Adapter an Windows zurück. Windows wählt wieder seinen eigenen Treiber (normalerweise den Bluetooth-Treiber), und der Adapter funktioniert mit dem Windows-Bluetooth. BlueCaster kann erst wieder darüber streamen, wenn WinUSB erneut zugewiesen ist. Windows verwendet immer nur einen Bluetooth-Adapter gleichzeitig. Ist ein anderer Adapter aktiv, meldet BlueCaster das nach der Wiederherstellung. Um diesen Adapter in Windows zu verwenden, deaktivieren Sie den anderen im Geräte-Manager und stecken diesen ab und wieder ein. Kopfhörer, die BlueCaster erneut gekoppelt hat, haben einen neuen Schlüssel. Bei Kopfhörern, die Windows über diesen Adapter gekoppelt hatte, übergibt die Wiederherstellung den neuen Schlüssel an Windows, sodass sie sich dort ohne erneutes Koppeln verbinden. **Einschränkung:** Übergeben werden nur klassische Kopplungen, keine LE-Schlüssel. Ein Gerät, das Windows über LE (LE Audio) nutzt, müssen Sie in Windows erneut koppeln (im Entwicklermodus können Sie die Übergabe von LE-Schlüsseln ausprobieren: **Entwickler > Debug**).

Das Deinstallationsprogramm bietet dasselbe an. Eine Seite des Deinstallationsassistenten enthält **Den ursprünglichen Bluetooth-Treiber wiederherstellen** (wenn BlueCaster Adapter auf WinUSB umgestellt hat; mit einer Abfrage nach Administratorrechten) und **Benutzerdaten löschen** (Einstellungen, Codecs, Kopplungen und Firmware in `%APPDATA%\BlueCaster` sowie Einstellungen, die sich das Fenster merkt, etwa **Nur unterstützte Codecs anzeigen**). Beide Optionen sind standardmäßig eingeschaltet. Wurden alle Adapter mit WinUSB über Zadig oder v1 eingerichtet (BlueCaster hat dann keine Aufzeichnung dazu), wird die erste Option angezeigt, ist aber ausgeschaltet. Schalten Sie sie ein, wählt Windows den Treiber für diese Adapter. Updates behalten WinUSB bei, ebenso eine stille Deinstallation (`/S`). Sind VB-CABLE oder Hi-Fi Cable installiert, bietet dieselbe Seite **VB-CABLE entfernen** und **Hi-Fi Cable entfernen** an. Diese Optionen sind standardmäßig ausgeschaltet, da andere Apps die Kabel nutzen können. Die Kabel werden ohne das Setup-Fenster von VB-Audio entfernt, nachdem Windows nach Administratorrechten gefragt hat. Anschließend sucht Windows nach geänderter Hardware, damit das Kabel aus der Sound-Liste verschwindet. Ein Neustart wird nur angeboten, wenn es dort noch aufgeführt ist.

### Stattdessen Zadig verwenden {#zadig}

Ein mit [Zadig](https://zadig.akeo.ie/) installierter WinUSB-Treiber funktioniert genauso gut:

1. Öffnen Sie Zadig und wählen Sie **Options > List All Devices**
2. Wählen Sie Ihren USB-Bluetooth-Adapter in der Dropdown-Liste aus
3. Wählen Sie **WinUSB** als Zieltreiber und klicken Sie auf **Replace Driver**

### Von Hand im Geräte-Manager {#device-manager}

Fehlt der Adapter in der Liste von BlueCaster, schlägt eine Schaltfläche auf der Seite **Adapter** fehl oder verhält sich Ihr Adapter anders als andere Adapter derselben Art, können Sie dasselbe von Hand im Geräte-Manager erledigen (Rechtsklick auf **Start** > **Geräte-Manager**). Für beide Aufgaben brauchen Sie Administratorrechte. Die Menünamen können je nach Windows-Version leicht abweichen.

**Durch WinUSB-Treiber ersetzen**

1. Suchen Sie den Adapter. Normalerweise steht er unter **Bluetooth** mit seinem eigenen Namen (zum Beispiel „TP-Link Bluetooth USB Adapter“) oder mit einem gelben Warnzeichen, wenn sein Treiber nicht starten konnte. Mit **Ansicht > Geräte nach Verbindung** können Sie ihn leichter vom eingebauten Bluetooth des PCs unterscheiden. Lassen Sie das eingebaute Bluetooth unverändert, es sei denn, Sie möchten es BlueCaster überlassen
2. Rechtsklick > **Treiber aktualisieren** > **Auf meinem Computer nach Treibern suchen** > **Aus einer Liste verfügbarer Treiber auf meinem Computer auswählen**
3. Fragt Windows nach einem Gerätetyp, wählen Sie **USB-Geräte**. Deaktivieren Sie **Kompatible Hardware anzeigen**, wählen Sie den Hersteller **WinUsb-Gerät** und das Modell **WinUsb-Gerät**, klicken Sie auf **Weiter** und bestätigen Sie die Warnung
4. Der Adapter erscheint jetzt als **WinUsb-Gerät** unter **USB-Geräte**. Starten Sie BlueCaster neu (oder öffnen Sie die Seite **Adapter** erneut). BlueCaster verwendet den Adapter dann genauso wie nach Zadig

**Den ursprünglichen Treiber wiederherstellen**

1. Klicken Sie unter **USB-Geräte** mit der rechten Maustaste auf den Adapter (**WinUsb-Gerät**) > **Gerät deinstallieren**. Stammt der WinUSB-Treiber von Zadig, aktivieren Sie zusätzlich **Versuchen, den Treiber für dieses Gerät zu entfernen**. Sonst wählt Windows wieder den Treiber von Zadig
2. Ziehen Sie den Adapter ab und stecken Sie ihn wieder ein (oder wählen Sie **Aktion > Nach geänderter Hardware suchen**). Windows installiert seinen eigenen Treiber, und der Adapter erscheint unter **Bluetooth**
3. Zeigt er dort stattdessen ein gelbes Warnzeichen (sein Treiber konnte nicht starten, zum Beispiel Code 31), funktioniert der Treiber des Herstellers mit diesem Adapter nicht. Wählen Sie dann Rechtsklick > **Treiber aktualisieren** > **Auf meinem Computer nach Treibern suchen** > **Aus einer Liste verfügbarer Treiber auf meinem Computer auswählen** > **Generischer Bluetooth-Adapter**. Oder weisen Sie wie oben wieder WinUSB zu, um ihn mit BlueCaster zu verwenden

## Schritt 3: Kopfhörer finden {#headphones}

**Geräte suchen** (unter **Audiogeräte**) sucht 30 Sekunden lang über den USB-Adapter nach Geräten in der Nähe und zeigt sie an, sobald sie antworten. In der Einrichtung startet die Suche automatisch. Sie läuft, bis ein Gerät verbunden ist (**Gerät hinzufügen** trennt es und setzt die Suche fort), bis Sie auf **Suche beenden** klicken oder die Einrichtung schließen. Versetzen Sie die Kopfhörer vorher in den Kopplungsmodus. Geräte, mit denen BlueCaster schon einmal gekoppelt war, sind als **Gekoppelt** gekennzeichnet.

Findet die Suche die Kopfhörer nicht, geben Sie die Bluetooth-Adresse unter **Manuell:** ein und klicken Sie auf **Verwenden**. BlueCaster formatiert eingegebene oder eingefügte Adressen automatisch: `AA:BB:CC:DD:EE:FF`, `aa-bb-cc-dd-ee-ff`, `AABBCCDDEEFF` und `AA BB CC DD EE FF` werden alle zu `AA:BB:CC:DD:EE:FF`.

Kopfhörer, die Windows über diesen Adapter gekoppelt hat, bevor er WinUSB erhielt, können diese Kopplung behalten. **Kopplungen aus Windows übernehmen** (im Geräteschritt der Einrichtung, wenn es solche Geräte gibt) listet sie auf, alle angehakt. Die angehakten Geräte werden übernommen und verbinden sich dann ohne Kopplungsmodus. Windows fragt dabei nach Administratorrechten. Ein solches Gerät ist meist nicht im Kopplungsmodus und erscheint daher nicht in der Suche. Erscheint es doch, oder geben Sie seine Adresse ein, warnen **Verbinden** und **Hinzufügen** zuerst, dass Windows es gekoppelt hat. **Trotzdem hinzufügen** koppelt es erneut als neues Gerät.

**Über die Windows-Einstellungen** (wenn die Kopfhörer mit dem integrierten Bluetooth des PCs gekoppelt wurden):
1. Öffnen Sie **Einstellungen > Bluetooth und Geräte**
2. Klicken Sie auf Ihr Audiogerät
3. Klicken Sie auf **Eigenschaften**
4. Dort steht die Bluetooth-Adresse (Format: `AA:BB:CC:DD:EE:FF`)

**Über die Befehlszeile** (beenden Sie BlueCaster vorher über Infobereich > **Beenden**):
```
bluecaster-core.exe --cli --scan
```

Neue Kopfhörer starten mit **Automatisch** als Codec (**Codecs**) und dem Systemton als Ausgabequelle (**Audiogeräte**). Mit diesen Standardwerten erhalten Sie den besten Klang. Alles Weitere finden Sie unter [Verwendung](usage).

---

## Firmware (Realtek-Adapter) {#firmware}

USB-Bluetooth-Adapter mit Realtek-Chip (z. B. TP-Link UB500, RTL8761BU-Dongles) benötigen eine proprietäre Firmware. CSR-Adapter benötigen keine Firmware. Intel-, MediaTek-MT79xx-, Qualcomm- und Broadcom-Adapter benötigen eventuell eigene Firmwaredateien, siehe [Empfohlene Adapter](#adapters).

### In der App (geführter Download)

Die Seite **Adapter** (und der Einrichtungsschritt **Treiber und Firmware**) zeigt den Firmwarestatus des Adapters an: **Firmware OK**, **Keine Realtek-Dateien nötig** (ein Adapter, den BlueCaster nicht erkennt, mit dem Chip auf **Nicht Realtek**) oder **Firmware nicht gefunden** mit den Namen der benötigten Dateien. Öffnen Sie mit **Downloadseite öffnen** die Seite [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) im Browser und anschließend mit **Firmware-Ordner öffnen** den Zielordner. Laden Sie die benötigten `.bin`-Dateien herunter und legen Sie sie im Firmware-Ordner ab.

Bei Adaptern anderer Hersteller gibt es keine Auswahl **Bluetooth-Chip**. Stattdessen zeigt ein Etikett, woher die Firmware kommt: **Aus dem Treiber von MediaTek** oder **Aus dem Treiber von Qualcomm** (aus dem Windows-Treiber des Herstellers gelesen, der nach der Umstellung auf WinUSB installiert bleibt), **Aus dem WLAN-Treiber** (Marvell; WLAN eingeschaltet lassen), **Aus dem Firmware-Ordner** (Intel, mit **Downloadseite öffnen**, **Firmware-Ordner öffnen** und **Einrichtungsanleitung**), **Optional** (die `.hcd`-Datei von Broadcom) oder **Nicht nötig** (CSR, Barrot, Actions). Hat BlueCaster die Firmware über WinUSB geladen, steht auf dem Etikett **Geladen**, oder **Nicht geladen**, wenn das Laden fehlgeschlagen ist. Daneben stehen **Downloadseite öffnen**, **Firmware-Ordner öffnen** und eine Zeile, woher die Firmware kommt.

Ein RTL8761B-Adapter (z. B. TP-Link UB500) braucht keinen Download, wenn der Windows-Treiber von Realtek auf dem PC installiert ist. BlueCaster verwendet dann die Firmware aus diesem Treiber, der nach der Umstellung auf WinUSB installiert bleibt. Um eine andere Firmware zu laden, etwa die von Linux, wählen Sie auf der Seite **Adapter** im Firmware-Bereich des Adapters **Eigene Firmware verwenden**. BlueCaster lädt dann die Dateien im Firmware-Ordner. Mit **Firmware des Treibers verwenden** wechseln Sie zurück.

Für die Chips in der Liste **Bluetooth-Chip** erledigt **Firmware herunterladen** das für Sie. Nach Ihrer Bestätigung lädt BlueCaster die Dateien von linux-firmware (kernel.org) herunter, prüft jede gegen die erwartete SHA-256-Prüfsumme und speichert sie im Firmware-Ordner. Hat linux-firmware für einen Chip keine Config-Datei, speichert BlueCaster eine leere `_config.bin`, und der Chip verwendet seine eigenen Einstellungen. Die Dateien sind nicht Teil von BlueCaster und werden nur heruntergeladen, wenn Sie es veranlassen.

BlueCaster erkennt den Chipsatz des Adapters und zeigt an, welche Firmwaredateien benötigt werden.

Ohne Firmware lässt sich ein Realtek-Adapter trotzdem öffnen und antwortet mit seinem ROM-Code. Er wirkt also in Ordnung, aber Verbinden oder Streamen schlägt fehl, und BlueCaster meldet, dass die Firmware nicht geladen ist. Ein Adapter behält die in einer früheren Sitzung hochgeladene Firmware, bis er keinen Strom mehr hat. Auch nach dem Entfernen der Dateien kann er daher weiter funktionieren, bis der PC neu startet oder der Adapter abgezogen wird.

### Einstellung Bluetooth-Chip

Die Liste **Bluetooth-Chip** auf der Seite **Adapter** legt fest, ob BlueCaster die Realtek-Initialisierung (Firmware-Upload) auf dem Adapter ausführt:

| Auswahl | Bedeutung |
|:--------|:----------|
| **Automatisch** (Standard) | Wird bei jedem Start von BlueCaster anhand des erkannten Adapters entschieden. Ein bekannter Realtek-Adapter erhält seinen Chip, jeder andere Adapter keine Realtek-Initialisierung |
| **Nicht Realtek** | Führt nie eine Realtek-Initialisierung aus. Verwenden Sie diese Option für Intel-, Broadcom-, CSR- und andere Adapter, falls Automatisch Ihren Adapter nicht erkennt |
| **RTL8761BU**, **RTL8822CU**, ... | Der jeweilige Realtek-Chip. Nötig für einen Adapter mit Realtek-Chip, den Automatisch nicht erkennt (ein OEM-Produkt mit eigener VID:PID). Die Statuszeile zeigt, ob die zugehörigen `.bin`-Dateien im Firmware-Ordner liegen |
| **Custom** | Verwendet `custom_fw.bin` und `custom_config.bin` aus dem Firmware-Ordner |

Wählen Sie für einen Adapter ohne Realtek-Chip weder einen Realtek-Chip noch Custom. BlueCaster würde sonst versuchen, Realtek-Firmware auf den Adapter hochzuladen, und der Adapter startet dann möglicherweise nicht.

### Manueller Download

1. Laden Sie die Firmware- und Config-`.bin`-Dateien für Ihren Chipsatz von [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) herunter (z. B. `rtl8761bu_fw.bin` und `rtl8761bu_config.bin`)
2. Legen Sie beide Dateien im Firmware-Ordner von BlueCaster ab (`%APPDATA%\BlueCaster\firmware`, auch über **Firmware-Ordner öffnen** erreichbar)

{: .note }
Diese Firmwaredateien sind proprietäre Realtek-Binärdateien, die über das Projekt linux-firmware verteilt werden. Sie sind nicht in diesem Repository enthalten. Die Bedingungen für die Weitergabe finden Sie in [WHENCE](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/WHENCE).

---

## Empfohlene Adapter {#adapters}

Welche Adapter und Kopfhörer tatsächlich mit BlueCaster im Einsatz waren, mit Codecs und Berichten, finden Sie unter [Geprüfte Hardware](hardware).

| Chipsatz | Beispielprodukte | Hinweise |
|:---------|:-----------------|:---------|
| Realtek | TP-Link UB500, RTL8761BU | Verwendet die Firmware aus dem Windows-Treiber von Realtek, falls dieser installiert ist; sonst die Firmware herunterladen (siehe oben). Am besten getestet |
| Realtek | RTL8852BD (`0BDA:B853`), RTL8852BT, RTL8922A | BlueCaster liest die Firmware (z. B. `rtl8852bd_mp_chip_new.dat`) aus dem Windows-Bluetooth-Treiber von Realtek. Lassen Sie diesen Treiber also installiert, oder legen Sie die im Protokoll genannte Datei in den Firmware-Ordner. Noch nicht auf der Hardware getestet |
| CSR | Generische CSR8510-Dongles | Funktioniert sofort, keine Firmware nötig |
| Barrot | Bluetooth-5.4-/6.0-Dongles (`33FA:0010`, `33FA:0012`) | Läuft mit der Firmware im ROM, keine Firmware nötig. Bisher ein Nutzerbericht |
| Marvell | Eingebaut in Microsoft Surface Pro / Surface Book (88W8897) | Keine Firmwaredateien: Der WLAN-Treiber lädt auch die Bluetooth-Firmware, lassen Sie WLAN daher eingeschaltet. Noch nicht auf der Hardware getestet |
| Actions | ATS2851-Dongles (`10D7:B012`) | Läuft mit der Firmware im ROM, keine Firmware nötig. Der Adapter meldet Befehle, die er nicht unterstützt. BlueCaster blendet sie aus. Noch nicht auf der Hardware getestet |
| Broadcom | ASUS BT400 (BCM20702), andere Dongles und Laptop-Module mit BCM20702 / BCM20703 / BCM43142 | Adapter, die statt der Bluetooth-Klasse `E0/01/01` die Herstellerklasse `FF/01/01` melden (OEM-Dongles mit BCM20702), werden ebenfalls erkannt. Läuft mit der Firmware im ROM. Ist der Windows-Bluetooth-Treiber von Broadcom installiert, lädt BlueCaster beim Start den Patch aus diesem Treiber hoch. Liegt eine PatchRAM-Datei `.hcd` (z. B. `BCM20702A1-0b05-17cb.hcd`) im Firmware-Ordner, verwendet BlueCaster stattdessen diese Datei. Schlägt der Treiber-Patch fehl, fordert BlueCaster Sie auf, den Adapter abzuziehen und wieder einzustecken. Danach lädt BlueCaster den Treiber-Patch nicht mehr auf diesen Adapter hoch. Der Treiber-Patch ist noch nicht auf der Hardware getestet |
| Intel | Intel 7260 / 3160 / 7265 / 3165 / 3168 / 8260 / 8265 / 9260 / AX200 / AX201 / AX210 / AX211 | Im Bootloader-Modus (z. B. nach einem Kaltstart) werden die passenden Dateien `ibt-*.sfi` + `ibt-*.ddc` aus linux-firmware `intel/` im Firmware-Ordner benötigt. Das Protokoll nennt sie (z. B. `ibt-0180-0041.sfi`). 7260 / 3160 / 7265 / 3165 / 3168 laufen aus dem ROM und brauchen keine Dateien. Liegt der passende Patch `ibt-hw-*.bseq` im selben Ordner, wird er angewendet |
| MediaTek | MT7925, MT7927, MT7922, MT7921, MT7902 (WLAN-Karten wie AMD RZ717, RZ738, RZ616, RZ608) | Benötigt die Datei `BT_RAM_CODE_MT*_hdr.bin` des Chips (z. B. `BT_RAM_CODE_MT7925_1_1_hdr.bin`). BlueCaster liest sie aus dem Windows-Bluetooth-Treiber von MediaTek, wenn dieser installiert ist. Andernfalls legen Sie die im Protokoll genannte Datei aus linux-firmware `mediatek/` in den Firmware-Ordner. Der MT7927 ist noch nicht auf der Hardware getestet |
| Qualcomm | QCA61x4, QCA9377 (ROME), WCN6855 (FastConnect 6900), WCN785x (FastConnect 7800) | Benötigt Rampatch und NVM des Chips (z. B. `AthrBT_0x00000302.dfu` und `ramps_0x00000302_48.dfu`). BlueCaster liest sie aus dem Windows-Bluetooth-Treiber von Qualcomm, wenn dieser installiert ist. Andernfalls legen Sie die im Protokoll genannten Dateien aus linux-firmware `qca/` (`rampatch_usb_*.bin`, `nvm_usb_*.bin`) in den Firmware-Ordner. Noch nicht auf der Hardware getestet |
| MediaTek | MT7663 | Benötigt die Patch-Datei des Chips (z. B. `mt7663_patch_e2_hdr.bin`). BlueCaster liest sie aus dem Windows-Bluetooth-Treiber von MediaTek, wenn dieser installiert ist. Andernfalls legen Sie die im Protokoll genannte Datei in den Firmware-Ordner. Noch nicht auf der Hardware getestet |
| Qualcomm Atheros | AR3012 und andere AR30xx-Chips (Bluetooth 4.0 in älteren Laptops) | Benötigt die Patch- und die Konfigurationsdatei des Chips (z. B. `AthrBT_0x31010000.dfu` und `ramps_0x31010000_40.dfu`). BlueCaster liest sie aus dem Windows-Bluetooth-Treiber von Qualcomm Atheros, wenn dieser installiert ist. Andernfalls legen Sie die im Protokoll genannten Dateien in den Firmware-Ordner. Noch nicht auf der Hardware getestet |
| MediaTek, Qualcomm | Ältere MediaTek-Chips (etwa der MT7630 und der MT7650), andere Qualcomm-Chips (QCA6290) | Nicht unterstützt: Sie benötigen Firmware-Loader, die BlueCaster nicht hat ([beim Hinzufügen helfen](chip-support)) |

USB-Bluetooth-Adapter ab Bluetooth 5.0 eignen sich im Allgemeinen am besten für Codecs mit hoher Bitrate wie LDAC.

Haben mehrere Adapter WinUSB, verwendet BlueCaster den ersten, den es findet, außer Sie klicken auf der Seite **Adapter** auf der Karte eines anderen auf **Diesen Adapter verwenden**. Der verwendete Adapter ist mit **In Verwendung** markiert.

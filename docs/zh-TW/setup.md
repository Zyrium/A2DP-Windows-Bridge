---
title: 安裝設定
layout: default
parent: 首頁
nav_order: 1
---

# 安裝設定
{: .no_toc }

## 目錄
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## 必要條件

- **Windows 10 (22H2) / 11** (x64 或 ARM64)
- **供 BlueCaster 使用的 USB 藍牙介面卡**：建議使用另一張獨立的介面卡。USB 類型的內建介面卡也可以使用，但這段期間 Windows 將無法使用藍牙（請參閱 [步驟 2](#winusb)）

BlueCaster 會自動為介面卡指派 WinUSB 驅動程式，因此不再需要 [Zadig](https://zadig.akeo.ie/)（Zadig 仍然可用，請參閱 [改用 Zadig](#zadig)）。

## 安裝 BlueCaster

每個 [發行版本](https://github.com/SeiyaFunaokaJP/BlueCaster/releases) 都提供兩種形式，內含的檔案相同：

- **安裝程式**（`BlueCaster-<version>-win64-setup.exe`，ARM64 版為 `-arm64-setup.exe`）：不需系統管理員權限，即可為目前的使用者安裝。安裝程式會新增「開始」功能表項目（**BlueCaster**）與解除安裝程式，若缺少 Microsoft Edge WebView2 執行階段也會一併安裝（Windows 11 已內建）。更新可直接在應用程式中安裝（**設定 > 更新 > 立即更新**）。程式會安裝到 `%LOCALAPPDATA%\BlueCaster\`（三個程式、解除安裝程式，以及收錄授權條文的 `licenses\` 資料夾）
- **免安裝版**（`BlueCaster-<version>-win64-portable.zip`，ARM64 版為 `-arm64-portable.zip`）：解壓縮到任意位置，然後執行 `bluecaster.exe`。請將三個程式（`bluecaster.exe`、`bluecaster-core.exe`、`bluecaster-driver.exe`）放在同一個資料夾。免安裝版只會通知您有更新，且需要已安裝 WebView2 執行階段

兩者都會把設定、設定檔、連結金鑰與韌體儲存在 `%APPDATA%\BlueCaster`，因此可以互相切換。解除安裝時會刪除這個資料夾，除非您關閉 **刪除使用者資料**（無訊息解除安裝會保留資料夾）。

### 從 v1 升級 {#from-v1}

直接在 v1 上執行安裝程式即可。安裝程式的 **準備安裝** 頁面會列出要先解除安裝的舊版本（v1 或任何 2.0.x）。如果 v1 正在執行，安裝程式會請您先結束 v1，接著解除安裝 v1，但保留設定資料夾：設定檔、耳機配對與韌體檔案都會沿用，耳機不需要重新配對。已（透過 Zadig）安裝 WinUSB 的介面卡會維持不變。如果先前開啟了 **隨 Windows 啟動**，Windows 現在會改為啟動 v2。

v2 第一次啟動時，會先把 v1 的設定複製到 `%APPDATA%\BlueCaster\backup\v1\`，再讀取這些設定。

### 從 2.0.x（A2DP Windows Bridge）升級 {#from-2x}

BlueCaster 3.0.0 是 A2DP Windows Bridge 的新名稱。直接在已安裝的 2.0.x 上執行 3.0.0 的安裝程式即可：安裝程式會自動移除舊版本，並保留您的設定。「開始」功能表項目與 **隨 Windows 啟動** 項目都會改名為「BlueCaster」。

3.0.0 第一次啟動時，會把 `%APPDATA%\A2DPWB` 資料夾重新命名為 `%APPDATA%\BlueCaster`，並將檔案整理到子資料夾中（請參閱 [設定與檔案](architecture#settings-and-files)）。`backup\v2\` 保留了 2.0.x 設定與配對的副本，方便您改回 2.0.2：只要把這些檔案複製回 `%APPDATA%\A2DPWB` 即可。

在重新配對之前，耳機可能仍會顯示舊的藍牙名稱「A2DPWB」。

## 步驟 1：首次執行設定 {#first-run}

插入 USB 藍牙介面卡並啟動 BlueCaster。第一次啟動時（還沒有任何音訊裝置，或沒有介面卡使用 WinUSB），BlueCaster 會開啟四個步驟的 **設定精靈**。完成或略過精靈後，就不會再自動開啟；之後請在 **首頁** 或 **音訊裝置** 畫面新增裝置。

1. **選擇介面卡**：列出找到的 USB 藍牙介面卡。如果沒有看到您的介面卡，請插入後按 **重新整理**。綠色的 **支援** 或紅色的 **不支援** 標籤表示 BlueCaster 能否使用該介面卡；不支援的介面卡會列出，但無法選取
2. **驅動程式與韌體**：這個步驟會列出即將進行的作業，按 **準備此介面卡** 即會執行：BlueCaster 會把介面卡的驅動程式替換為 WinUSB（Windows 會要求系統管理員權限，請參閱 [步驟 2](#winusb)），並下載介面卡所需的 Realtek 韌體檔案（請參閱 [韌體](#firmware)）。BlueCaster 無法下載的韌體，必須手動複製到韌體資料夾。如果 Windows 有透過這張介面卡配對的裝置（例如內建介面卡上的鍵盤或滑鼠），按鈕會先列出這些裝置：BlueCaster 使用該介面卡期間，這些裝置在 Windows 中將無法使用，按 **取消** 即可讓它們繼續運作。使用自有 USB 接收器的裝置不受影響；若要保留其他裝置，請讓 BlueCaster 使用另一張 USB 介面卡。**介面卡** 畫面上的 **替換為 WinUSB 驅動程式** 也會列出這些裝置
3. **尋找音訊裝置**：掃描會自動開始。請讓音訊裝置進入配對模式，然後按裝置旁的 **連線**（或輸入位址後按 **使用**）：BlueCaster 會以裝置支援的最佳編解碼器（自動）儲存裝置並連線。裝置隨後會出現在 **已配對的裝置** 下方，以圓點標示狀態，掃描也會停止。按 **新增裝置** 會中斷這個裝置的連線，並重新掃描以新增另一個裝置；第一個裝置仍會保留，選錯的裝置可以在這裡移除。如果 Windows 曾透過這張介面卡配對過裝置，會顯示 **匯入 Windows 的配對**：按 **匯入** 會列出這些裝置，並全部勾選（請參閱 [步驟 3](#headphones)）。沒有裝置也可以按 **下一步**，之後再到 **音訊裝置** 畫面新增。已安裝 **Hi-Fi Cable** 時，新裝置會擷取它，否則擷取 VB-CABLE。Hi-Fi Cable 增加的延遲最少，因此建議用於聲音，VB-CABLE 則建議用於耳機的麥克風。缺少音訊線時，這個步驟會顯示警告，按 **深入了解** 會列出兩種音訊線：VB-CABLE 的 **安裝** 會從 VB-Audio 下載 VB-CABLE 安裝套件（1.3 MB）並安裝（會要求系統管理員權限；若沒有出現提示，請重新啟動 Windows），Hi-Fi Cable 的 **下載** 則會開啟 VB-Audio 的網頁。已安裝的音訊線會改為顯示 **解除安裝**。兩者都是 VB-Audio 的捐贈軟體（[vb-cable.com](https://vb-cable.com/)，**關於** 畫面也有說明）：BlueCaster 並未隨附 VB-CABLE，捐款歸 VB-Audio 所有。VB-Audio 要求公司及其他組織每次安裝都需取得授權（[授權](https://vb-audio.com/Services/licensing.htm)）。之後 BlueCaster 第一次找到某條音訊線時，設為 **預設裝置** 的裝置會改為擷取它（VB-CABLE 只在未安裝 Hi-Fi Cable 時才會），麥克風沒有可用輸出的裝置則會把麥克風送到這條音訊線。編解碼器之後可在 **編解碼器** 畫面變更
4. **一般設定**：主題、**啟動**、**通知** 與更新檢查，與「設定」中的控制項相同，變更會立即儲存。首次設定會開啟 **隨 Windows 啟動** 與 **在系統匣中啟動**

**稍後設定** 會略過其餘步驟；之後可從 **設定 > 執行設定精靈** 再次開啟。

## 步驟 2：為介面卡指派 WinUSB {#winusb}

BlueCaster 透過 WinUSB（Windows 內建的通用 USB 驅動程式）直接與 USB 藍牙介面卡通訊。建議使用 **專用的介面卡**，這樣內建藍牙仍可在 Windows 中照常使用。

1. 開啟 **介面卡** 畫面（在設定精靈的 **驅動程式與韌體** 步驟中，**準備此介面卡** 會完成步驟 2 到 4）
2. 介面卡的 **驅動程式** 會顯示 **原始驅動程式**（或 **無驅動程式**），也就是 Windows 或製造商安裝的驅動程式，WinUSB 會取代它。按一下 **替換為 WinUSB 驅動程式**
3. 閱讀確認訊息後繼續：Windows 會要求系統管理員權限（UAC）
4. 驅動程式現在會顯示 **WinUSB 驅動程式**

如果 Windows 無法立即載入 WinUSB（因為介面卡正在使用中），BlueCaster 會通知您，驅動程式那一行會顯示 **請重新插拔或重新啟動 Windows**。請拔下介面卡再重新插入；內建介面卡則請重新啟動 Windows。在此之前，連線都會失敗，記錄中會出現 `failed to initialize WinUSB (error 50)`。

{: .warning }
指派 WinUSB 後，Windows 本身的藍牙就看不到這張介面卡：透過它與 Windows 配對的裝置，在還原驅動程式之前都無法使用。一般周邊裝置（鍵盤、滑鼠等）請使用內建藍牙。您也可以把 USB 類型的內建介面卡（許多筆電上的 Realtek 介面卡）交給 BlueCaster，但在還原驅動程式之前，**電腦將完全無法使用 Windows 藍牙**（藍牙設定、滑鼠、鍵盤、其他音訊裝置）。

同一畫面上的 **還原原始驅動程式** 會把介面卡交還給 Windows：Windows 會重新選擇自己的驅動程式（通常是藍牙驅動程式），介面卡即可搭配 Windows 藍牙使用。在重新指派 WinUSB 之前，BlueCaster 無法透過這張介面卡串流。Windows 一次只會使用一張藍牙介面卡。如果另一張介面卡正在使用中，BlueCaster 會在還原後通知您；若要在 Windows 中使用這張介面卡，請在裝置管理員中停用另一張，再重新插拔這張。BlueCaster 重新配對過的耳機會有新的金鑰。對於 Windows 曾透過這張介面卡配對的耳機，還原時會把新金鑰交給 Windows，因此不必重新配對就能在 Windows 中連線。**限制：** 只會轉移傳統配對，不會轉移 LE 金鑰。Windows 透過 LE（LE Audio）使用的裝置，必須在 Windows 中重新配對（開發人員模式可以嘗試轉移 LE 金鑰：**開發者 > 偵錯**）。解除安裝程式也提供相同的功能：解除安裝精靈的其中一頁有 **還原原本的藍牙驅動程式**（BlueCaster 曾為介面卡指派 WinUSB 時顯示；只會要求一次系統管理員權限）與 **刪除使用者資料**（`%APPDATA%\BlueCaster` 中的設定、編解碼器、配對與韌體，以及視窗記住的內容，例如 **僅顯示支援的編解碼器**），兩者預設都會勾選。如果使用 WinUSB 的介面卡全都是以 Zadig 或 v1 設定的（BlueCaster 沒有它們的記錄），仍會顯示第一個選項，但預設不勾選；勾選後由 Windows 選擇驅動程式。更新與無訊息解除安裝（`/S`）都會保留 WinUSB。已安裝 VB-CABLE 或 Hi-Fi Cable 時，同一頁還會提供 **移除 VB-CABLE** 與 **移除 Hi-Fi Cable**，由於其他應用程式也可能在使用，預設不勾選。移除時不會顯示 VB-Audio 的安裝視窗，Windows 會要求系統管理員權限。接著 Windows 會掃描硬體變更，讓音訊線從音效清單中消失；只有在音訊線仍列在清單中時，才會建議重新啟動。

### 改用 Zadig {#zadig}

以 [Zadig](https://zadig.akeo.ie/) 安裝的 WinUSB 驅動程式也一樣可以使用：

1. 開啟 Zadig，選擇 **Options > List All Devices**
2. 從下拉式選單中選取您的 USB 藍牙介面卡
3. 選擇 **WinUSB** 作為目標驅動程式，然後按一下 **Replace Driver**

### 在裝置管理員中手動操作 {#device-manager}

如果 BlueCaster 的清單中沒有這張介面卡、**介面卡** 畫面上的按鈕操作失敗，或您的介面卡與同類介面卡的表現不同，可以在 Windows 的裝置管理員（以滑鼠右鍵按一下 **開始** > **裝置管理員**）中手動完成相同的操作。這兩項操作都需要系統管理員權限。選單名稱可能因 Windows 版本而略有不同。

**替換為 WinUSB 驅動程式**

1. 找到介面卡：通常位於 **藍牙** 下方，顯示介面卡本身的名稱（例如「TP-Link Bluetooth USB Adapter」）；如果驅動程式啟動失敗，則會帶有黃色標記。**檢視 > 依連線排列裝置** 可協助您區分這張介面卡與電腦內建的藍牙。除非要交給 BlueCaster 使用，否則請勿變更內建的藍牙
2. 以滑鼠右鍵按一下介面卡 > **更新驅動程式** > **瀏覽電腦上的驅動程式** > **讓我從電腦上的可用驅動程式清單中挑選**
3. 如果 Windows 詢問裝置類型，請選擇 **通用序列匯流排裝置**。取消勾選 **顯示相容硬體**，製造商選擇 **WinUsb 裝置**，型號也選擇 **WinUsb 裝置**，按一下 **下一步** 並確認警告
4. 介面卡現在會以 **WinUsb 裝置** 顯示在 **通用序列匯流排裝置** 下方。重新啟動 BlueCaster（或重新開啟 **介面卡** 畫面），即可像使用 Zadig 後一樣使用這張介面卡

**還原原本的驅動程式**

1. 在 **通用序列匯流排裝置** 下方，以滑鼠右鍵按一下介面卡（**WinUsb 裝置**）> **解除安裝裝置**。如果 WinUSB 驅動程式來自 Zadig，請同時勾選 **嘗試移除此裝置的驅動程式**，否則 Windows 會再次選擇 Zadig 的驅動程式
2. 拔下介面卡再重新插入（或選擇 **動作 > 掃描硬體變更**）。Windows 會安裝自己的驅動程式，介面卡會出現在 **藍牙** 下方
3. 如果介面卡在那裡帶有黃色標記（驅動程式啟動失敗，例如代碼 31），表示製造商的驅動程式不適用於這張介面卡：請以滑鼠右鍵按一下 > **更新驅動程式** > **瀏覽電腦上的驅動程式** > **讓我從電腦上的可用驅動程式清單中挑選** > **一般藍牙介面卡**；或依照上述步驟重新指派 WinUSB，以便在 BlueCaster 中使用

## 步驟 3：尋找耳機 {#headphones}

**掃描裝置**（位於 **音訊裝置** 畫面）會透過 USB 介面卡搜尋附近的裝置 30 秒，裝置一回應就會列出。在設定精靈中，掃描會自動開始，並持續到有裝置連線（**新增裝置** 會中斷該裝置並繼續掃描）、按下 **停止掃描**，或關閉設定精靈為止。請先讓耳機進入配對模式。BlueCaster 先前配對過的裝置會標示為 **已配對**。

如果掃描找不到耳機，請在 **手動:** 欄位輸入藍牙位址，然後按一下 **使用**。BlueCaster 會在您輸入或貼上位址時自動調整格式：`AA:BB:CC:DD:EE:FF`、`aa-bb-cc-dd-ee-ff`、`AABBCCDDEEFF` 與 `AA BB CC DD EE FF` 都會變成 `AA:BB:CC:DD:EE:FF`。

在這張介面卡改用 WinUSB 之前，Windows 透過它配對的耳機可以沿用原本的配對。**匯入 Windows 的配對**（位於設定精靈的裝置步驟，有這類裝置時才會顯示）會列出這些裝置並全部勾選，匯入勾選的裝置後，耳機不必進入配對模式即可連線。Windows 會要求系統管理員權限。這類裝置通常不在配對模式，所以不會出現在掃描結果中。如果出現了，或您輸入了它的位址，按 **連線** 或 **新增** 時會先提醒您該裝置已由 Windows 配對；按 **仍要新增** 則會把它當成新裝置重新配對。

**從 Windows 設定**（如果耳機曾與電腦內建的藍牙配對）：
1. 開啟 **設定 > 藍牙與裝置**
2. 按一下您的音訊裝置
3. 按一下 **內容**
4. 畫面上會顯示藍牙位址（格式：`AA:BB:CC:DD:EE:FF`）

**從命令列**（請先結束 BlueCaster：系統匣 > **結束**）：
```
bluecaster-core.exe --cli --scan
```

耳機預設使用 **自動** 編解碼器（**編解碼器** 畫面），並以系統音訊作為輸出來源（**音訊裝置** 畫面）；這些預設值能提供最佳音質。其他內容請參閱 [使用方式](usage)。

---

## 韌體（Realtek 介面卡） {#firmware}

採用 Realtek 晶片的 USB 藍牙介面卡（例如 TP-Link UB500、RTL8761BU 接收器）需要專有韌體才能運作。CSR 介面卡不需要韌體。Intel、MediaTek MT79xx、Qualcomm 與 Broadcom 介面卡可能需要各自的韌體檔案，請參閱 [建議的介面卡](#adapters)。

### 在應用程式中（下載引導）

**介面卡** 畫面（以及設定精靈的 **驅動程式與韌體** 步驟）會顯示介面卡的韌體狀態：**韌體正常**、**不需要 Realtek 檔案**（BlueCaster 無法辨識的介面卡，且晶片設為 **非 Realtek**），或 **找不到韌體** 並列出所需的檔案名稱。按 **開啟下載頁面** 可在瀏覽器中開啟 [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) 頁面，按 **開啟韌體資料夾** 則可開啟目的資料夾。請下載所需的 `.bin` 檔案，並放到韌體資料夾中。

其他廠商的介面卡沒有 **藍牙晶片** 選項，而是以標籤顯示韌體的來源：**來自 MediaTek 的驅動程式** 或 **來自 Qualcomm 的驅動程式**（從廠商的 Windows 驅動程式讀取；改用 WinUSB 後，該驅動程式仍會保留），**來自 Wi-Fi 驅動程式**（Marvell；請保持 Wi-Fi 開啟），**來自韌體資料夾**（Intel，附有 **開啟下載頁面**、**開啟韌體資料夾** 與 **設定指南**），**選用**（Broadcom 的 `.hcd`），或 **不需要**（CSR、Barrot、Actions）。BlueCaster 透過 WinUSB 載入韌體後，標籤會顯示 **已載入**；載入失敗時則顯示 **未載入**，並附上 **開啟下載頁面**、**開啟韌體資料夾**，以及說明韌體來源的一行文字。

若電腦上已安裝 Realtek 的 Windows 驅動程式，RTL8761B 介面卡（例如 TP-Link UB500）就不需要下載韌體。BlueCaster 會直接使用該驅動程式附帶的韌體。介面卡改用 WinUSB 後，這些韌體仍會保留在電腦上。若要載入其他韌體（例如 Linux 的韌體），請在 **介面卡** 畫面中該介面卡的韌體區塊選取 **使用自訂韌體**。BlueCaster 隨後會載入韌體資料夾中的檔案。選取 **使用驅動程式的韌體** 即可切換回來。

對於 **藍牙晶片** 清單中的晶片，**下載韌體** 可以代您完成這些步驟：確認後，BlueCaster 會從 linux-firmware (kernel.org) 下載檔案，逐一比對預期的 SHA-256，並儲存到韌體資料夾。如果 linux-firmware 沒有某個晶片的 config 檔案，BlueCaster 會儲存一個空的 `_config.bin`，晶片則使用自身的設定。這些檔案不屬於 BlueCaster，只有在您要求時才會下載。

BlueCaster 會偵測介面卡的晶片組，並顯示需要哪些韌體檔案。

沒有韌體時，Realtek 介面卡仍會以 ROM 程式碼開啟並回應，看起來一切正常，但連線或串流會失敗，BlueCaster 也會提示韌體未載入。介面卡會保留先前工作階段上傳的韌體，直到斷電為止。因此即使刪除了檔案，在重新啟動電腦或拔下介面卡之前，介面卡仍可能繼續運作。

### 藍牙晶片設定

**介面卡** 畫面上的 **藍牙晶片** 清單，決定 BlueCaster 是否要對介面卡執行 Realtek 初始化（上傳韌體）：

| 選項 | 意義 |
|:-------|:--------|
| **自動**（預設） | 每次啟動 BlueCaster 時，依偵測到的介面卡決定。已知的 Realtek 介面卡會套用對應的晶片；其他介面卡則不執行 Realtek 初始化 |
| **非 Realtek** | 一律不執行 Realtek 初始化。如果「自動」無法辨識您的 Intel、Broadcom、CSR 或其他介面卡，請選擇此項 |
| **RTL8761BU**、**RTL8822CU**、... | 指定的 Realtek 晶片。適用於「自動」無法辨識的 Realtek 介面卡（使用自有 VID:PID 銷售的 OEM 產品）。狀態列會顯示所需的 `.bin` 檔案是否已在韌體資料夾中 |
| **Custom** | 使用韌體資料夾中的 `custom_fw.bin` 與 `custom_config.bin` |

請勿為非 Realtek 介面卡選擇 Realtek 晶片或 Custom：BlueCaster 會嘗試把 Realtek 韌體上傳到介面卡，可能導致介面卡無法啟動。

### 手動下載

1. 從 [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) 下載適用於您晶片組的韌體與設定 `.bin` 檔案（例如 `rtl8761bu_fw.bin` 與 `rtl8761bu_config.bin`）
2. 將兩個檔案放到 BlueCaster 韌體資料夾（`%APPDATA%\BlueCaster\firmware`，也可以按 **開啟韌體資料夾** 開啟）

{: .note }
這些韌體檔案是透過 linux-firmware 專案發佈的 Realtek 專有二進位檔，並未包含在本儲存庫中。再散布條款請參閱 [WHENCE](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/WHENCE)。

---

## 建議的介面卡 {#adapters}

實際與 BlueCaster 搭配使用過的介面卡與耳機，以及編解碼器與相關回報，請參閱 [已驗證的硬體](hardware)。

| 晶片組 | 產品範例 | 備註 |
|:--------|:-----------------|:------|
| Realtek | TP-Link UB500, RTL8761BU | 已安裝 Realtek 的 Windows 驅動程式時使用其中的韌體，否則需要下載韌體（見上文）。測試最完整 |
| Realtek | RTL8852BD（`0BDA:B853`）、RTL8852BT、RTL8922A | BlueCaster 會從 Realtek 的 Windows 藍牙驅動程式讀取韌體（例如 `rtl8852bd_mp_chip_new.dat`），因此請保留該驅動程式，或將記錄中列出的檔案放入韌體資料夾。尚未在實際硬體上測試 |
| CSR | 一般的 CSR8510 接收器 | 隨插即用，不需要韌體 |
| Barrot | Bluetooth 5.4 / 6.0 接收器（`33FA:0010`、`33FA:0012`） | 使用 ROM 韌體運作，不需要韌體。目前只有一份使用者回報 |
| Marvell | Microsoft Surface Pro / Surface Book 內建（88W8897） | 不需要韌體檔案：Wi-Fi 驅動程式也會載入藍牙韌體，請保持 Wi-Fi 開啟。尚未在實際硬體上測試 |
| Actions | ATS2851 介面卡（`10D7:B012`） | 使用 ROM 韌體運作，不需要韌體。介面卡會列出一些實際不支援的命令，BlueCaster 會將其隱藏。尚未在實際硬體上測試 |
| Broadcom | ASUS BT400 (BCM20702) 及其他採用 BCM20702 / BCM20703 / BCM43142 的接收器與筆電內建模組 | 回報廠商類別 `FF/01/01` 而非藍牙類別 `E0/01/01` 的介面卡（BCM20702 OEM 接收器）也能偵測。使用 ROM 韌體運作。若已安裝 Broadcom 的 Windows 藍牙驅動程式，BlueCaster 會在啟動時上傳該驅動程式中的修補檔。如果韌體資料夾中有 PatchRAM `.hcd`（例如 `BCM20702A1-0b05-17cb.hcd`），BlueCaster 會改用該檔案。如果驅動程式的修補檔上傳失敗，BlueCaster 會提示您拔下介面卡再重新插入。之後 BlueCaster 不會再對該介面卡上傳修補檔。驅動程式的修補檔尚未在實際硬體上測試 |
| Intel | Intel 7260 / 3160 / 7265 / 3165 / 3168 / 8260 / 8265 / 9260 / AX200 / AX201 / AX210 / AX211 | 處於 bootloader 模式時（例如冷開機後），韌體資料夾中需要 linux-firmware `intel/` 內對應的 `ibt-*.sfi` + `ibt-*.ddc`，記錄中會列出檔案名稱（例如 `ibt-0180-0041.sfi`）。7260 / 3160 / 7265 / 3165 / 3168 從 ROM 執行，不需要檔案；同一資料夾中若有對應的 `ibt-hw-*.bseq` 修補檔，則會套用 |
| MediaTek | MT7925、MT7927、MT7922、MT7921、MT7902（AMD RZ717、RZ738、RZ616、RZ608 等 Wi-Fi 網路卡） | 需要該晶片的 `BT_RAM_CODE_MT*_hdr.bin`（例如 `BT_RAM_CODE_MT7925_1_1_hdr.bin`）。若已安裝 MediaTek 的 Windows 藍牙驅動程式，BlueCaster 會從中讀取；否則請將 linux-firmware `mediatek/` 中的檔案放入韌體資料夾，記錄中會列出檔案名稱。MT7927 尚未在實際硬體上測試 |
| Qualcomm | QCA61x4、QCA9377（ROME）、WCN6855（FastConnect 6900）、WCN785x（FastConnect 7800） | 需要該晶片的 rampatch 與 NVM（例如 `AthrBT_0x00000302.dfu` 與 `ramps_0x00000302_48.dfu`）。若已安裝 Qualcomm 的 Windows 藍牙驅動程式，BlueCaster 會從中讀取；否則請將 linux-firmware `qca/` 中的檔案（`rampatch_usb_*.bin`、`nvm_usb_*.bin`）放入韌體資料夾，記錄中會列出檔案名稱。尚未在實際硬體上測試 |
| MediaTek | MT7663 | 需要該晶片的修補檔（例如 `mt7663_patch_e2_hdr.bin`）。若已安裝 MediaTek 的 Windows 藍牙驅動程式，BlueCaster 會從中讀取；否則請將該檔案放入韌體資料夾，記錄中會列出檔案名稱。尚未在實際硬體上測試 |
| Qualcomm Atheros | AR3012 及其他 AR30xx 晶片（較舊筆記型電腦的藍牙 4.0） | 需要該晶片的修補檔與設定檔（例如 `AthrBT_0x31010000.dfu` 與 `ramps_0x31010000_40.dfu`）。若已安裝 Qualcomm Atheros 的 Windows 藍牙驅動程式，BlueCaster 會從中讀取；否則請將這些檔案放入韌體資料夾，記錄中會列出檔案名稱。尚未在實際硬體上測試 |
| MediaTek、Qualcomm | 較舊的 MediaTek 晶片（例如 MT7630 與 MT7650）、其他 Qualcomm 晶片（QCA6290） | 不支援：需要 BlueCaster 目前沒有的韌體載入程式（[協助加入支援](chip-support)） |

支援藍牙 5.0 以上的 USB 介面卡，通常最適合 LDAC 等高位元率的編解碼器。

如果有多張介面卡使用 WinUSB，BlueCaster 會使用找到的第一張，除非在**介面卡**頁面另一張介面卡的卡片上按了**使用此介面卡**。正在使用的介面卡標示為**使用中**。

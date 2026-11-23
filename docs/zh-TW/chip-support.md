---
title: 加入晶片支援
layout: default
parent: 首頁
nav_order: 3.5
---

# 加入晶片支援

BlueCaster 直接驅動藍牙介面卡（WinUSB），因此必須完成廠商 Windows 驅動程式所做的工作，介面卡才能運作；對許多晶片來說，這代表每次啟動都要載入韌體。Realtek、Intel、Broadcom 與 CSR 介面卡目前都能使用，MediaTek MT7925、MT7927、MT7922、MT7921、MT7902、MT7663 與 Qualcomm QCA61x4、QCA9377、WCN6855、WCN785x、AR30xx 也可以（MT7927、MT7922、MT7902、MT7663 與 Qualcomm 晶片尚未在實際硬體上測試）。其他 MediaTek 晶片（較舊的晶片）與 Qualcomm 晶片（QCA6290）目前還無法使用，因為還沒有人為它們撰寫載入程式。若要支援其他 MediaTek 晶片，請從 `app/src/crates/bt-host/src/mediatek.rs` 中的 MT79xx 載入程式著手。

本頁說明如何加入載入程式。您需要一張採用該晶片的介面卡，維護者手上沒有這些介面卡。即使不寫程式，您也能幫忙：步驟 1 與 2 可以產生貢獻者所需的回報。

{: .note }
這項功能的設計與尚待解決的問題，請參閱 [開發者筆記](../chip-bringup-dev)。

## 目錄
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## 需要準備的東西

- 該介面卡，插在 Windows 10 / 11 電腦上
- BlueCaster 2.0.2 或更新版本（以下步驟只需要應用程式本身）
- 若要撰寫載入程式：依照 [建置](building) 準備的建置環境，以及用來記錄 Windows 驅動程式動作的 [USBPcap](https://desowin.org/usbpcap/) 與 [Wireshark](https://www.wireshark.org/)

## 步驟 1：將介面卡交給 BlueCaster

1. **設定 > 開發人員模式**：開啟
2. **介面卡**：在該介面卡的卡片上按 **替換為 WinUSB 驅動程式**（Windows 會要求系統管理員權限）。對於 BlueCaster 尚無法使用的晶片，這個按鈕只有在開發人員模式下才能使用
3. 如果卡片顯示 **請重新插拔或重新啟動 Windows**，請照著做

介面卡使用 WinUSB 期間，Windows 本身的藍牙無法使用它。如果筆電唯一的藍牙就是這顆晶片，透過 Windows 連線的鍵盤、滑鼠與耳機都會停止運作，直到您在同一張卡片上按 **還原原始驅動程式** 還原驅動程式為止。

## 步驟 2：讀取晶片的回應（晶片探測）

在介面卡卡片的 **韌體** 區段按 **讀取晶片資訊**。藍牙堆疊會短暫停止並執行探測，接著檔案總管會開啟報告：設定資料夾（`%APPDATA%\BlueCaster`）內 `logs\probe\` 中的 `probe-<date>_<time>.json`。最多保留最新的 10 份。

探測只會讀取資料，不會傳送 HCI Reset，也不會傳送任何廠商命令：

| 請求 | 可得知的內容 |
|:--|:--|
| Read Local Version Information | HCI / LMP 版本、製造商（company ID）、LMP subversion：通常就是 ROM 或韌體的版本 |
| Read BD_ADDR | 介面卡的位址（部分晶片在韌體執行前為零或佔位值） |
| Read Local Supported Commands / Features, Read Buffer Size | 目前執行中的程式碼提供哪些功能：ROM 提供的功能通常比韌體少得多 |
| Read Local Name | 有些 ROM 會回報自己的名稱 |
| LE Read Local Supported Features, LE Read Buffer Size | LE 的同類資訊 |
| USB 裝置描述元、製造商與產品字串 | 在預設管道上發出的標準 USB 請求 |

探測也會記錄 Windows 回報的 USB 組態（介面、端點），以及晶片主動送出的任何事件。沒有回應的請求也會記錄下來：載入韌體前完全不回應的晶片，本身就是一項發現。

也可以從命令列執行，並指定介面卡的 VID:PID 或執行個體 ID（顯示在介面卡卡片上）：

```bat
"%LOCALAPPDATA%\BlueCaster\bluecaster-core.exe" --cli -u 0e8d:7961 --probe
```

請先從系統匣結束 BlueCaster，因為同一時間只有一方能使用介面卡。

**傳送回報**：按下 **讀取晶片資訊** 後，介面卡卡片會顯示讀取到的內容，以及 **在 GitHub 回報** 按鈕。這個按鈕會開啟新的 issue，預先填入版本、介面卡、摘要、您的 Windows 組建以及報告內容。請補充說明介面卡的來源（筆電內建的介面卡請註明筆電型號）。

如果報告太長，無法填入 issue，**Probe report** 欄位會保持空白，檔案總管則會顯示報告檔案。請將檔案拖曳到這個欄位；沒有這個檔案，GitHub 不會建立 issue。報告存放在 `%APPDATA%\BlueCaster\logs\probe`，檔名日期最新的就是最新的報告。報告包含介面卡的藍牙位址與名稱，但不含任何與您耳機有關的資訊。開啟 HCI 擷取的 **診斷套件**（**診斷** 畫面）也會在 `probe/` 下附上探測報告。

## 步驟 3：記錄 Windows 驅動程式的動作

探測只能看出晶片目前的狀態，看不出如何改變它。廠商的 Windows 驅動程式知道該怎麼做：只要記錄一次，載入程式就能重複相同的步驟。

1. 在介面卡卡片上按 **還原原始驅動程式**，讓 Windows 的驅動程式重新接管介面卡
2. 安裝 USBPcap 與 Wireshark；以系統管理員身分啟動 Wireshark，擷取介面卡所在的 USBPcap 介面。`USBPcapCMD.exe`（位於 USBPcap 的資料夾）會列出每個根集線器上的裝置；筆電的內建介面卡位於內部集線器上，通常只顯示為「USB Composite Device」或 VID:PID。不確定時，請同時擷取所有 USBPcap 介面
3. 讓驅動程式載入韌體：拔下介面卡再重新插入；內建介面卡則在裝置管理員中按 **停用裝置**，再按 **啟用裝置**。如果韌體在暖開機後仍會保留，可能要完全關機後才會重新載入
4. 介面卡出現在 Windows 的藍牙設定後，停止擷取。在最前面的幾個封包中找到介面卡的裝置描述元（VID:PID），以它的位址篩選（`usb.bus_id == <bus> && usb.device_address == <address>`），然後儲存擷取檔

請依序檢查以下內容：

- 任何 HCI 命令之前的 **控制傳輸**：`bmRequestType` 為 0x40 / 0xC0（廠商，OUT / IN），以及對應的 `bRequest`、`wValue`、`wIndex`。這些傳輸通常用來讀取晶片 ID，或重設晶片的某個部分
- opcode 介於 0xFC00 到 0xFFFF 的 **HCI 命令**（類別請求，`bmRequestType` 0x20）：也就是廠商命令。一長串參數很大的相同 opcode，就是正在傳送韌體；從回應可以看出每個部分如何確認
- 不是 Command Complete / Status 的 **事件**：廠商事件（代碼 0xFF，或廠商自訂的代碼，例如 0xE4），例如「韌體已啟動」，或是取代 Command Complete、用來確認每個韌體部分的事件
- 第一個 HCI Reset 之前的 **Bulk OUT** 資料：有些晶片透過 ACL 管道接收韌體
- 檔案的來源：將傳送的資料與驅動程式資料夾中的檔案（裝置管理員 > 驅動程式詳細資料，或 `C:\Windows\System32\DriverStore\FileRepository\`），以及 [linux-firmware](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/) 中該晶片的檔案（`mediatek/`、`qca/`）比對；linux-firmware 的檔案可以再散布，BlueCaster 的使用者也能下載

記下驅動程式在各步驟之間等待的時間（請看擷取檔的時間戳記）：仍在重設中的晶片不會回應。

{: .warning }
請以擷取檔、探測報告與廠商文件為依據。請勿複製或密切參照 GPL 程式碼，例如 Linux 核心的 `btmtk` 或 `btqca` 驅動程式：BlueCaster 採用 Apache-2.0 或 MIT 授權，衍生自這些程式碼的載入程式無法收錄。

## 不必建置也能試：晶片指令碼 {#chip-scripts}

撰寫載入程式之前，可以先用 **晶片指令碼** 在介面卡上試試擷取到的序列：指令碼就是以文字寫成的序列，每行一個請求，取代藍牙堆疊來執行。沒有該介面卡的人也能撰寫指令碼，再交給擁有介面卡的人執行；結果會以報告的形式傳回，不需要重新建置。

**執行方式**：在開發人員模式下開啟 **開發者 > 晶片**，選擇介面卡（使用 WinUSB），輸入指令碼或開啟檔案，然後按 **執行**。每一行的回應會顯示在下方，**顯示報告** 會開啟 `logs\probe\` 中的 `script-<date>_<time>.json`（其中也包含指令碼）。從命令列執行：`bluecaster-core --cli -u <adapter> --script <file>`。

```text
# 每行一個請求；數字為十六進位（0x 可省略），wait、timeout= 與 chunk= 除外
hci 1001                           # HCI 命令：opcode，接著是參數位元組
hci fc05 ff
bulk fc09 00 01 02                 # 同上，但走 bulk OUT 管道
usb in vendor device 63 0 0 4      # 控制 IN：類型、接收者、請求、value、index、長度
usb out vendor device 64 0 0 01    # 控制 OUT：...，接著是資料位元組
wait 100                           # 毫秒
expect e4 02 01 timeout=5000       # 事件：其代碼與最前面的參數位元組
bootloader off                     # bootloader 模式（從一開始就開啟）
upload mediatek/patch.bin fc6f 01 01 chunk=240 ack=event:e4:02,01
upload-bulk patch.bin fc09 01      # 同上，但走 bulk OUT 管道
```

- `hci` 與 `bulk` 會等待該命令的 Command Complete 或 Command Status（2 秒）並記錄下來，不論狀態為何
- `upload` 會把韌體資料夾中的檔案，以該 opcode 的命令分段傳送；每個命令包含標頭位元組，以及檔案中最多 `chunk` 個位元組（預設 240），並等待每一段的確認：`ack=complete`（預設：該 opcode 狀態為 0 的 Command Complete 或 Command Status）或 `ack=event:<code>[:<bytes>]`。遇到錯誤狀態就會停止
- 指令碼會在第一個失敗的行（沒有回應、發生錯誤、找不到檔案）停止，並指出是哪一行
- 沒有對應任何一行的事件，也會保留在報告中

**安全性**：只有已知為唯讀的請求會直接執行：標準 HCI 讀取（資訊參數與狀態參數、Read Local Name、LE 讀取）、標準 USB 描述元請求、`wait`、`expect` 與 `bootloader`。指令碼只要含有其他任何一行，就必須經您允許才會執行（在這些行的清單下方按 **允許執行**；命令列則使用 `--allow-writes`）。可能寫入晶片斷電後仍會保留之記憶體的廠商命令，無論您是否允許都不會執行（`app/src/crates/bt-host/src/chips.rs` 中的 `denied_opcodes`：目前為 CSR 的 BCCMD）。bootloader 模式從一開始就是開啟的，因此執行後關閉介面卡時不會傳送 HCI Reset。載入到晶片 RAM 的韌體，在重新插拔後就會消失。

## 步驟 4：撰寫載入程式

從這裡開始，路徑都是相對於儲存庫根目錄。相關的 crate 有三個：

| Crate（套件） | 資料夾 | 內容 |
|:--|:--|:--|
| `bt-host` | `app/src/crates/bt-host` | 藍牙主機：載入程式、探測、晶片偵測 |
| `bluecaster-core`（程式庫） | `app/src/crates/core` | 設定與介面卡：決定執行哪個載入程式 |
| `bluecaster-core-exe` | `app/src/core` | 服務執行檔（`bluecaster-core.exe`）：處理視窗的請求、介面卡說明 |

後兩者的名稱中都有「core」；`cargo -p` 使用的是上表中的套件名稱。

載入程式是一個本身不做 I/O 的狀態機：它接收控制器的事件、自己發出的 USB 請求的結果與計時器，並傳回 **動作** 交由應用程式執行。因此不需要晶片也能測試載入程式，USB 與執行緒的細節也不會混入其中。

1. 將 `app/src/crates/bt-host/src/bringup/sample.rs`（一個虛構晶片的完整載入程式，附有測試）複製為 `app/src/crates/bt-host/src/<vendor>.rs`（例如 `mediatek.rs`）。在 `app/src/crates/bt-host/src/lib.rs` 中加入 `pub mod <vendor>;`，並與其他模組依字母順序排列
2. 保留名稱：型別為 `Loader`（以 `<vendor>::Loader` 使用），`vendor()` 傳回廠商名稱。比照範例，將測試保留在同一個檔案中
3. 將範例中標示為「made up」的所有內容，換成步驟 2 與 3 得到的資訊：版本檢查、USB 請求、檔案名稱、下載命令及其確認方式，以及韌體的啟動方式
4. 改寫最上方的模組註解：晶片需要什麼、各步驟的順序，以及每項資訊的來源（您擷取的 Windows 驅動程式動作、廠商文件、探測報告）。請勿把其他驅動程式的原始碼列為任何步驟的來源

### 介面

全部位於 `app/src/crates/bt-host/src/bringup.rs`。

| `ChipLoader` 方法 | 呼叫時機 |
|:--|:--|
| `vendor()` | 用於記錄行（"MediaTek"） |
| `start()` | 介面卡開啟後、HCI 初始化之前，呼叫一次 |
| `on_event(event)` | 每個 HCI 事件：代碼、長度、參數 |
| `on_control(result)` | 傳入最後一個 `UsbControl` 的結果：讀到的位元組，或失敗原因 |
| `on_timeout()` | 最後一個 `Timer` 的計時器到期時 |
| `missing_file_hint(file)` | 載入程式以 `MissingFile` 結束時寫入記錄：該把什麼檔案放到哪裡（"x.bin from linux-firmware (mediatek/)"） |
| `step()`, `progress()` | 每批動作之後：步驟名稱（"download"）與進度（已傳送的命令數 / 總數），供介面卡啟動期間的介面卡畫面顯示 |

`Done` 之後，應用程式會捨棄載入程式：遲到的回應不會再送到載入程式。

| `Action` | 作用 |
|:--|:--|
| `Command(bytes)` | 在控制管道上傳送 HCI 命令（opcode、長度、參數）；會記錄在 HCI 擷取中 |
| `BulkCommand(bytes)` | 在 bulk OUT 管道上傳送命令（位元組與 `Command` 相同）；有些 bootloader 會從這裡接收韌體 |
| `UsbControl(request)` | 在預設管道上進行控制傳輸（廠商暫存器）；會立即執行，`on_control` 會在下一個動作之前收到結果。以註記形式記錄在 HCI 擷取中 |
| `Bootloader(on)` | 適用於載入程式在 bulk IN 管道上回應，或不能收到 HCI Reset 的晶片（有些晶片收到後會當住）；請參閱「模式」。能正常回應 HCI 的 ROM 晶片不需要 |
| `Timer(ms)` | 啟動或重新啟動載入程式唯一的計時器 |
| `Done(Ok(outcome))` | 完成，接著進行 HCI 初始化。`AlreadyRunning`、`Loaded { firmware, detail }`、`Rom`（`Probed` 為探測專用） |
| `Done(Err(error))` | 介面卡無法使用：`MissingFile(name)`、`BadFile { file, why }`、`Command { opcode, status }`、`Timeout(step)`、`Rejected(code)`、`BadVersion`、`UnsupportedVariant(v)`、`Unsupported(why)`、`Host(why)` |

以下輔助函式讓載入程式不必手動解析位元組：

- `hci_command(opcode, params)` 建立命令
- `parse_event(event)` 會傳回 `HciEvent::Complete { opcode, status, ret }`（status 之後的回傳參數）、`Status { opcode, status }`、`Vendor(params)`（事件 0xFF），或 `Other { code, params }`（其他任何代碼，包括廠商自訂的代碼，例如 0xE4）

**韌體檔案**：載入程式會取得一個 `FileReader`，並以 **不含資料夾的檔名**（`"BT_RAM_CODE_MT7961_1_2_hdr.bin"`）要求檔案。應用程式會在韌體資料夾（`%APPDATA%\BlueCaster\firmware`）及其中的廠商子資料夾（`firmware\mediatek\`）中尋找，配置與 linux-firmware 相同；子資料夾在步驟 6 中設定。

### 模式

範例示範了以下每一種模式；用不到的請刪除。

- **一個命令，一個確認**：傳送命令、啟動計時器，並在等待狀態中只接受該 opcode 的確認：Command Complete，或是（結果稍後才傳回的命令）status 為 0 的 Command Status。其他一律忽略，以免遲到的回應讓載入程式往下走
- **以廠商事件確認**：有些晶片會以自己的事件，而不是 Command Complete，來回應每個韌體部分。此時只等待該事件（其代碼，以及用來識別它的參數）；在該狀態下，忽略該命令的 Command Complete 或 status 為 0 的 Command Status，但兩者帶有錯誤 status 時仍要結束。範例把 Command Complete 與 Command Status 視為同一種確認，因此在這種情況下，`on_event` 需要同時比對狀態與事件
- **多個檔案**（先 patch，再 configuration）：傳送前先載入並檢查所有檔案，再依序傳送，並在 `Loaded { firmware, .. }` 中寫出最後一個檔案（其他檔案會寫在記錄中）
- **刻意的暫停**（驅動程式在下一步之前等待 100 ms）：為暫停建立一個狀態，並送出 `Timer(100)`；在該狀態下，`on_timeout` 會傳送下一步，而不是視為失敗。測試中的 `SimRun::timeouts` 也會計入這類暫停
- **回應兩次的命令**（先確認，再送出「韌體已啟動」之類的廠商事件）：使用兩個狀態，第二個狀態有自己的計時器。範例中的 `Launching` 與 `Starting` 就是如此。如果事件帶有結果代碼，代碼錯誤時以 `Rejected(code)` 結束載入程式
- Command Complete 或 Command Status 中的 **錯誤 status** 會以 `Command { opcode, status }` 結束載入程式
- **Bootloader 模式**：在第一個命令之前（甚至在版本檢查之前）送出 `Bootloader(true)`。成功時（`Loaded` 以及 `AlreadyRunning`），在 `Done` 之前送出 `Bootloader(false)`，讓 HCI 初始化正常進行；失敗時則保持開啟，因為晶片仍在 bootloader 中。範例的 `finish()` 就是這樣做
- **已在執行**：版本或暫存器顯示正式韌體已在執行時，立即以 `AlreadyRunning` 結束。介面卡在斷電之前都會保留韌體，再次下載可能會失敗
- **先檢查檔案**：傳送任何內容之前，先解析並檢查整個檔案（`BadFile` 附上原因），並指出缺少的確切檔案
- **只限您的晶片**：如果載入程式只涵蓋某廠商的部分晶片，傳送任何內容之前，請先確認介面卡是其中之一（晶片 ID 暫存器，或以 `ControlRequest::device_descriptor()` 取得的 USB ID），否則以 `Unsupported` 結束
- 以 `info` 等級 **記錄** 從晶片得知的資訊（晶片 ID、ROM 版本、選用的檔案）：回報中看到的就是這些記錄
- 絕對不要寫入晶片的快閃記憶體或 OTP 記憶體，並且只透過 `FileReader` 讀取檔案

## 步驟 5：以模擬控制器測試

測試使用 `app/src/crates/bt-host/src/bringup/sim.rs`。您要撰寫一個小型的模擬晶片（`impl SimController`），讓它依照擷取檔中真實晶片的方式回應命令與控制傳輸；`sim::run(&mut loader, &mut chip)` 則會完全比照應用程式（包括計時器）來驅動載入程式。結果（`SimRun`）包含最終結果、每個已傳送的命令及其管道（`commands[i].bulk`）、每次控制傳輸、結束時的 bootloader 模式（`bootloader`），以及計時器觸發的次數。`sim::command_complete`、`command_status`、`vendor_event`、`event`（任何代碼）、`opcode` 與 `params` 用來建立及讀取位元組；bulk 命令則在 `bulk_command` 中回應。

請比照範例，至少涵蓋：

- 完整的下載流程，檢查傳送的命令（數量、順序、大小、管道）
- 如果載入程式使用 bootloader 模式，檢查每個案例結束時的狀態
- 韌體已在執行
- 缺少檔案，以及損毀的檔案（不傳送任何內容）
- 被拒絕的命令（錯誤 status）、沒有回應的控制器（逾時），以及每個可能逾時的等待
- 如果載入程式使用 USB 傳輸，檢查傳輸失敗的情況

從擷取檔複製的位元組是最好的測試資料。使用下列命令執行測試：

```bat
cargo test -p bt-host
```

## 步驟 6：接入應用程式

以下是所有需要修改的檔案，適用於已列為不支援的廠商（MediaTek、Qualcomm）或新廠商。若是新廠商，編譯器會指出哪些 `match` 需要加入新的變體；其餘部分，包括已列出廠商的所有修改，都只能靠這份清單。

**一顆晶片還是整個廠商？** 載入程式通常只涵蓋某廠商的部分晶片（例如 MT7921，但不含 MT7925）。此時只有這些晶片可以移除「不支援」的狀態：在 `app/src/crates/bt-host/src/chips.rs` 的 `LOADER_CHIPS` 中列出它們的 VID:PID，並將該廠商保留在 `listed_chips_only()` 中。這樣 `chips::chip_supported(vendor, vid, pid)` 與 `chip_support_tag()` 就會逐一判斷每張介面卡，介面卡的名稱標籤、**介面卡** 畫面上的說明、該畫面是否提供 WinUSB，以及「not implemented」警告，都會依據判斷結果變化。只有涵蓋該廠商所有晶片的載入程式（例如 Intel 的），才可以把該廠商從 `listed_chips_only()` 中移除。

{: .note }
請保留每個檔案原本的換行字元：有些原始檔使用 CRLF，有些使用 LF。部分編輯器與 Git Bash 中的 `sed -i` 會在不知不覺中改寫換行字元，而 `cargo fmt` 不會改回來。`git diff --stat` 可以看出這個問題：只改了幾處的檔案，卻顯示每一行都有變更。此時請在提交前把檔案轉回原本的換行字元（例如使用 VS Code 狀態列中的換行字元切換）。

**偵測**（`app/src/crates/bt-host/src/chips.rs`）：

1. `ChipVendor`：新廠商需要一個變體（附一行文件註解），以及 `name()` 中的一個分支。不論是哪個廠商，都要更新 enum 的文件註解（哪些廠商有載入程式）與變體的文件註解（"not supported"）
2. 部分晶片：將 VID:PID 加入 `LOADER_CHIPS`，並在載入程式的模組中加入測試，確認載入程式本身的晶片檢查（晶片 ID、版本）只接受這些項目。整個廠商：將廠商從 `listed_chips_only()` 中移除
3. `vendor_for_usb()`：該廠商的 USB VID。其他製造商也會使用的 VID，必須以 `VID if pid == PID` 比對，絕不能比對整個 VID：例如以模組製造商 VID 出貨的筆電模組（AzureWave 0x13D3、Foxconn 0x0489），以及 pid.codes 0x1209 這類共用 VID。這裡列出的每個 VID，也會讓 `is_bluetooth_vendor_vid` 把該製造商的裝置視為藍牙介面卡。BTstack 的 Realtek 表或 `OEM_TABLE`（兩者都在本檔案的 `resolve_realtek_pid` 中）裡的 VID:PID，會在查詢 `vendor_for_usb` 之前就被視為 Realtek：請確認您的 VID:PID 不在兩者之中
4. `company_name()`：Read Local Version 傳回的 Bluetooth SIG company ID（如果尚未列出）
5. 測試：`app/src/crates/bt-host/tests/chips.rs`（VID:PID 與標籤）

**開啟介面卡**：藍牙類別的裝置（類別 E0/01/01）不需修改即可開啟。如果晶片在 ROM 狀態下回報廠商類別（FF），還需要將它的 VID:PID 加入 `app/src/crates/core/src/adapter.rs` 的 `BUILTIN_VENDOR_CLASS`；探測報告中的 `usb.interfaces` 一行會顯示類別。

**啟動載入程式**，比照 Intel 的做法（搜尋 `intel_loader`）：

6. `app/src/crates/bt-host/src/lib.rs`：一個類似 `intel_loader` 的 `HostConfig` 欄位（包括它在 `HostConfig::new` 中的預設值、`BtHost::launch` 中的解構，以及除錯記錄行），以及 `BtHost::launch` 中的分支：安裝傳輸層並呼叫應用程式的啟動函式，以取代 `setup_hci_and_power_on`，且只在非 H4 時執行（`&& !tcp`，與 Intel 相同：模擬器的控制器並非真正的晶片）。每次啟動只會執行一個載入程式：如果兩個廠商的旗標都已設定（兩張介面卡都使用 WinUSB），會由前面的分支優先，因此測試時請只讓您的介面卡使用 WinUSB
7. `app/src/crates/bt-host/src/app.rs`：一個類似 `start_intel_loader` 的 `start_<vendor>_loader`（加上 `use crate::<vendor>;`），把 `read_firmware_file(&dir, "<vendor>", name)`（子資料夾名稱與 linux-firmware 相同）交給載入程式，並呼叫 `start_loader`
8. `app/src/crates/core/src/adapter.rs` 的 `configure()`：當該廠商的介面卡符合 `chips::chip_supported(a.vendor, a.device.vid, a.device.pid)`、使用 WinUSB 且未被占用時，設定這個欄位（比照 `config.intel_loader`；其他情況則使用 Intel 的「in use by another program」記錄），並把欄位加入該處的 `adapter:` 除錯記錄行。下方的「firmware loading for this chip is not implemented」警告已經依循 `chip_supported`
9. `app/src/crates/core/src/service.rs` 的 `host_config()`：把這個欄位加入除錯記錄行

**視窗顯示的內容**：

10. `app/src/core/src/state.rs` 的 `vendor_note()`：從 `MediaTek | Qualcomm` 分支中拆出一個使用新鍵 `firmware.vendor_<vendor>` 的分支，並加入測試。在這個分支之前，函式會對 `chip_supported` 拒絕的晶片傳回 `firmware.vendor_unsupported`，因此該廠商的其他晶片仍會顯示原本的說明。**介面卡** 畫面會為說明不是 `firmware.vendor_unsupported` 的每張介面卡提供 WinUSB
11. `app/lang/*.json`（全部 10 種語言，每個檔案的同一行）：新增鍵，比照 Intel 的鍵（「Intel 介面卡。…」）。提到按鈕或資料夾時，請使用同一檔案中該語言的用語（韌體資料夾的按鈕為 `firmware.open_folder`）。整個廠商：從 `firmware.vendor_unsupported` 中移除廠商名稱。部分晶片：保留名稱，因為該廠商的其他晶片仍會顯示這段文字
12. `app/src/ui/src/lib/links.ts`：該廠商的韌體頁面（加在 `LINKS`、`firmwareUrl()` 與 `vendorFirmwareUrl()` 中，比照 Intel 的 linux-firmware 資料夾），讓 **介面卡** 畫面顯示檔案的取得位置；測試位於 `app/src/ui/src/lib/chips.test.ts`。**介面卡** 畫面只會為說明不是 `firmware.vendor_unsupported` 的介面卡顯示這個頁面，因此該廠商的其他晶片不會看到

**文件**：

13. `docs/setup.md` 與 `docs/*/setup.md`（所有語言）中的晶片表格：比照 Intel 新增一列，寫出涵蓋的晶片、韌體檔案及存放位置（韌體資料夾 `%APPDATA%\BlueCaster\firmware`，或其中的 `<vendor>` 子資料夾；較舊的列仍寫著「config folder」）。視窗中的按鈕名稱，請依照各語言的 `app/lang/*.json`（**介面卡** 畫面為 `ui.nav.adapter`，開啟該資料夾的按鈕為 `firmware.open_folder`）。整個廠商：從「Not supported」那一列中移除。部分晶片：保留該廠商，並寫出仍不支援的晶片。將該廠商加入 Firmware 一節開頭的句子（"Intel and Broadcom adapters may need firmware files of their own"）
14. 有人實際使用過後，更新 [已驗證的硬體](hardware)（`docs/hardware.md`），以及本頁的開頭段落（`docs/chip-support.md`，所有語言），其中列出尚無載入程式的廠商（部分晶片時寫成「the other <vendor> chips」）

## 步驟 7：在介面卡上實測

在儲存庫根目錄建置（`cargo build --release -p bluecaster-core-exe`），結束已安裝的應用程式（從系統匣圖示），然後從命令列執行核心並進行 HCI 擷取：

```bat
target\release\bluecaster-core.exe --cli -u 0e8d:7961 --scan 5 --hci-capture loader.pklg
```

記錄會顯示每個步驟；`loader.pklg` 可在 Wireshark 中開啟，依序列出每個命令、事件與控制傳輸。拔下介面卡（或關機）後再試一次：晶片冷啟動時下載必須成功，韌體已在執行時則不做任何事。

{: .note }
如果有多張介面卡使用 WinUSB，即使指定了 `-u`，BlueCaster 也會優先使用 Realtek 介面卡並套用其晶片設定。請將 **介面卡 > 藍牙晶片** 設為 **非 Realtek**，或測試時只讓您的介面卡使用 WinUSB。

接著透過視窗串流到耳機一段時間，並確認 **重新啟動藍牙** 與重新插拔都能讓介面卡恢復運作。介面卡啟動期間，**介面卡** 畫面會在藍牙狀態下方顯示載入程式的狀態（「正在載入 42%」或載入失敗）；在開發人員模式下，介面卡就緒後狀態仍會顯示，並附上您的 `step()` 名稱與結果。

## 步驟 8：開啟 pull request

開啟之前，請依序執行以下檢查（環境準備請參閱 [建置](building)）。先從 UI 開始，在 `app\src\ui` 中執行：這會檢查您在步驟 6 的修改（第 12 項），而且 Rust workspace 包含視窗的 crate，需要先建置好 UI：

```bat
pnpm install
pnpm build
pnpm exec tsc --noEmit
pnpm exec vitest run
```

接著在儲存庫根目錄執行：

```bat
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python tools/check_lang.py
```

如果沒有 Node.js，請改為只對上述三個 crate 執行測試與 clippy（`-p bt-host -p bluecaster-core -p bluecaster-core-exe`），把 UI 檢查交給 pull request 的 CI，並在 pull request 中註明。`pwsh ./build.ps1 -Test` 會執行全部檢查。

Clippy 也會檢查命名：名為 `PatchState` 的 `State` 變體會因 `enum_variant_names` 而失敗，請改名為 `Patch`。`check_lang.py` 可能會回報您修改之前就已存在的「long hints left」；您只需要修正錯誤與缺少的鍵。

其他地方也順帶提到 MediaTek 或 Qualcomm 不受支援（UI 的模擬資料與測試、`Adapter.tsx` 與 `probe.rs` 中的註解、「非 Realtek」選項）：這些地方維持原樣即可。

在 pull request 中：

- 以目前的 `develop-v*` 分支為基礎
- 說明您測試了哪些介面卡（產品、VID:PID、晶片），以及冷啟動與暖啟動的情況，並附上一份探測報告與一次下載的 HCI 擷取
- 說明載入程式中的資訊來自何處

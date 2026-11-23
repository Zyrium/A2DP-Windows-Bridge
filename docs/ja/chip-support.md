---
title: チップ対応の追加
layout: default
parent: ホーム
nav_order: 3.5
---

# チップ対応の追加

BlueCaster は Bluetooth アダプターを（WinUSB を通して）直接動かします。そのため、アダプターを使えるようにするためにメーカーの Windows ドライバーが行っている処理を、BlueCaster 自身が行う必要があります。多くのチップでは、起動のたびにファームウェアを読み込ませる処理です。Realtek、Intel、Broadcom、CSR のアダプターはすでに使えます。MediaTek の MT7925、MT7927、MT7922、MT7921、MT7902、MT7663 と、Qualcomm の QCA61x4、QCA9377、WCN6855、WCN785x、AR30xx も使えます（MT7927、MT7922、MT7902、MT7663 と Qualcomm のチップは実機では未検証）。ほかの MediaTek チップ（古いもの）と Qualcomm チップ（QCA6290）は、ローダーをまだ誰も書いていないため使えません。ほかの MediaTek チップに取り組むときは、`app/src/crates/bt-host/src/mediatek.rs` にある MT79xx 用のローダーを土台にしてください。

このページでは、ローダーを追加する手順を説明します。作業には、そのチップを積んだアダプターが必要です（メンテナーは持っていません）。コードを書かなくても協力できます。手順 1 と 2 を行えば、コントリビューターに必要なレポートを作成できます。

{: .note }
この仕組みの設計と未解決の課題は、[開発者向けノート](../chip-bringup-dev)で説明しています。

## 目次
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## 必要なもの

- そのアダプターと、それを挿した Windows 10 / 11 の PC
- BlueCaster 2.0.2 以降（手順 1 と 2 はアプリだけで行えます）
- ローダーを書く場合: [ビルド](building)で説明しているビルド環境と、Windows 自身のドライバーの動作を記録するための [USBPcap](https://desowin.org/usbpcap/) と [Wireshark](https://www.wireshark.org/)

## 手順 1: アダプターを BlueCaster に渡す

1. **設定 > 開発者モード**: オン
2. **アダプター**: そのアダプターのカードで **WinUSB ドライバーに置き換える**（Windows が管理者の許可を求めます）。BlueCaster がまだ対応していないチップでは、このボタンは開発者モードのときだけ押せます
3. カードに **挿し直すか Windows を再起動** と表示されたら、そのとおりにします

アダプターが WinUSB ドライバーを使っている間は、Windows 自身の Bluetooth はそのアダプターを使えません。Bluetooth がそのチップしかないノート PC では、Windows で接続しているキーボード、マウス、ヘッドセットが、ドライバーを戻すまで使えなくなります。ドライバーを戻すには、同じカードの **元のドライバーに戻す** を押します。

## 手順 2: チップの応答を読む（チッププローブ）

アダプターのカードの **ファームウェア** 欄で **チップ情報を読む** をクリックします。Bluetooth スタックが少しの間停止してプローブが実行され、エクスプローラーでレポートが開きます。レポートは設定フォルダー（`%APPDATA%\BlueCaster`）の `logs\probe\` にある `probe-<date>_<time>.json` で、最新の 10 個が保存されます。

プローブは読み取りだけを行います。HCI Reset もベンダーコマンドも送りません。

| 要求 | わかること |
|:--|:--|
| Read Local Version Information | HCI / LMP のバージョン、メーカー（company ID）、LMP サブバージョン（多くの場合、ROM かファームウェアのバージョン） |
| Read BD_ADDR | アダプターのアドレス（チップによっては、ファームウェアが動くまではゼロか仮の値） |
| Read Local Supported Commands / Features、Read Buffer Size | 動作中のコードが提供する機能。ROM の機能はファームウェアよりずっと少ないことがよくあります |
| Read Local Name | 名前を返す ROM もあります |
| LE Read Local Supported Features、LE Read Buffer Size | LE について同じ内容 |
| USB デバイスディスクリプター、メーカーと製品の文字列 | デフォルトパイプでの標準の USB 要求 |

ほかに、Windows が報告する USB 構成（インターフェイス、エンドポイント）と、チップが要求なしに送ってきたイベントも記録します。応答のなかった要求も記録されます。ファームウェアを読み込むまで何も応答しないチップであれば、それ自体が手がかりになります。

コマンドラインから実行するときは、アダプターの VID:PID かインスタンス ID（アダプターのカードに表示されます）を指定します。

```bat
"%LOCALAPPDATA%\BlueCaster\bluecaster-core.exe" --cli -u 0e8d:7961 --probe
```

アダプターは一度に 1 つのプログラムしか使えないので、先にトレイから BlueCaster を終了してください。

**レポートを送る**: **チップ情報を読む** のあと、アダプターのカードに読み取った内容と **GitHub で報告** ボタンが表示されます。このボタンを押すと、バージョン、アダプター、読み取った内容の概要、Windows のビルド、レポート本体が記入された新しい issue が開きます。アダプターの入手元（ノート PC 内蔵ならその機種）を書き足してください。

レポートが長すぎて issue に入りきらない場合は、**Probe report** 欄が空のままになり、代わりにエクスプローラーにファイルが表示されます。そのファイルをこの欄にドラッグしてください。ファイルがないと GitHub は issue を作成しません。レポートは `%APPDATA%\BlueCaster\logs\probe` にあり、ファイル名の日付がいちばん新しいものが最新のレポートです。レポートにはアダプターの Bluetooth アドレスと名前が含まれますが、ヘッドホンの情報は含まれません。HCI キャプチャを含めた **診断パッケージ**（診断画面）にも、プローブのレポートが `probe/` に入ります。

## 手順 3: Windows のドライバーの動きを記録する

プローブでわかるのはチップの状態だけで、状態を変える方法はわかりません。その方法を知っているのは、メーカーの Windows ドライバーです。ドライバーの動作を一度記録すれば、ローダーで同じ手順を再現できます。

1. アダプターのカードで **元のドライバーに戻す** を押し、アダプターを Windows のドライバーに戻します
2. USBPcap と Wireshark をインストールし、Wireshark を管理者として起動して、アダプターがつながっている USBPcap のインターフェイスをキャプチャします。`USBPcapCMD.exe`（USBPcap のフォルダーにあります）で、ルートハブごとのデバイスを一覧表示できます。ノート PC 内蔵のアダプターは内部のハブにつながっていて、「USB 複合デバイス」（"USB Composite Device"）や VID:PID だけで表示されることがよくあります。どれかわからなければ、USBPcap のインターフェイスをすべて同時にキャプチャします
3. ドライバーにファームウェアを読み込ませます。アダプターを抜いて挿し直してください。内蔵アダプターの場合は、デバイス マネージャーで **デバイスを無効にする** と **デバイスを有効にする** を実行します。再起動では消えないファームウェアは、完全にシャットダウンしたあとでないと読み込まれないことがあります
4. Windows の Bluetooth 設定にアダプターが表示されたら、キャプチャを止めます。最初のほうのパケットからアダプターのデバイスディスクリプター（VID:PID）を探し、そのアドレスで絞り込んで（`usb.bus_id == <bus> && usb.device_address == <address>`）キャプチャを保存します

確認する箇所（この順に）:

- HCI コマンドより前の **コントロール転送**: `bmRequestType` 0x40 / 0xC0（ベンダー、OUT / IN）と、その `bRequest`、`wValue`、`wIndex`。チップ ID の読み取りや、チップの一部のリセットであることがよくあります
- オペコード 0xFC00〜0xFFFF の **HCI コマンド**（クラス要求、`bmRequestType` 0x20）: ベンダーコマンドです。同じオペコードが大きなパラメーター付きで長く続いていれば、ファームウェアを送っています。その応答から、各部分がどのように確認されるかがわかります
- Command Complete / Status 以外の **イベント**: ベンダーイベント（コード 0xFF、または 0xE4 のようなベンダー独自のコード）。たとえば「ファームウェアが起動した」という通知や、Command Complete の代わりにファームウェアの各部分を確認するイベントです
- 最初の HCI Reset より前の **Bulk OUT** のデータ: ACL パイプでファームウェアを受け取るチップもあります
- ファイルの入手元: 送られたデータを、ドライバーのフォルダー（デバイス マネージャー > ドライバーの詳細、または `C:\Windows\System32\DriverStore\FileRepository\`）のファイルや、[linux-firmware](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/) にあるそのチップのファイル（`mediatek/`、`qca/`）と比べます。linux-firmware のファイルは再配布でき、BlueCaster のユーザーも自分でダウンロードできます

ドライバーが手順の間に入れている待ち時間（キャプチャのタイムスタンプでわかります）も書き留めてください。リセット中のチップは応答しません。

{: .warning }
キャプチャ、プローブのレポート、メーカーの資料をもとに作業してください。Linux カーネルの `btmtk` や `btqca` ドライバーのような GPL のコードを、コピーしたり、なぞって書いたりしないでください。BlueCaster は Apache-2.0 または MIT のライセンスなので、それらから派生したローダーは取り込めません。

## ビルドせずに試す: チップスクリプト {#chip-scripts}

ローダーを書く前に、キャプチャでわかった手順を **チップスクリプト** でアダプターに試せます。チップスクリプトは手順をテキストにしたもので、1 行に 1 つの要求を書き、Bluetooth スタックの代わりに実行します。アダプターを持っていない人がスクリプトを書き、持っている人に送ることもできます。結果はレポートとして返ってくるので、新しいビルドは不要です。

**スクリプトの実行**: 開発者モードで **開発者 > チップ** を開き、アダプター（WinUSB のもの）を選んで、スクリプトを入力するかファイルを開き、**実行** をクリックします。各行の応答が下に表示されます。**レポートを表示** を押すと、`logs\probe\` の `script-<date>_<time>.json`（スクリプトも含む）が開きます。コマンドラインでは `bluecaster-core --cli -u <adapter> --script <file>` を使います。

```text
# 1 行に 1 つの要求。数値は 16 進数（0x は省略可）。ただし wait、timeout=、chunk= は 10 進数
hci 1001                           # HCI コマンド: オペコード、続いてパラメーターのバイト
hci fc05 ff
bulk fc09 00 01 02                 # 同じものを Bulk OUT パイプで
usb in vendor device 63 0 0 4      # コントロール IN: 種類、受信者、要求、値、インデックス、長さ
usb out vendor device 64 0 0 01    # コントロール OUT: …、続いてデータのバイト
wait 100                           # ミリ秒
expect e4 02 01 timeout=5000       # イベント: そのコードと、パラメーターの先頭のバイト
bootloader off                     # ブートローダーモード（最初からオン）
upload mediatek/patch.bin fc6f 01 01 chunk=240 ack=event:e4:02,01
upload-bulk patch.bin fc09 01      # 同じものを Bulk OUT パイプで
```

- `hci` と `bulk` は、そのコマンドの Command Complete か Command Status を待ち（2 秒）、ステータスにかかわらず結果を記録します
- `upload` は、ファームウェアフォルダーにあるファイルを分割し、そのオペコードのコマンドで送ります。各コマンドには、ヘッダーのバイトと、ファイルの最大 `chunk` バイト（既定は 240）が入ります。コマンドごとに確認を待ちます。確認は `ack=complete`（既定。そのオペコードの、ステータス 0 の Command Complete か Command Status）か `ack=event:<code>[:<bytes>]` です。エラーのステータスが返ると停止します
- スクリプトは最初に失敗した行（応答がない、エラー、ファイルがない）で停止し、その行を表示します
- どの行への応答でもないイベントも、レポートに記録されます

**安全のための制限**: 確認なしで実行されるのは、読み取りだけだとわかっている要求です。標準の HCI の読み取り（情報パラメーターとステータスパラメーター、Read Local Name、LE の読み取り）、標準の USB ディスクリプター要求、`wait`、`expect`、`bootloader` が該当します。それ以外の行を含むスクリプトは、許可したときだけ実行されます（該当する行の一覧の下にある **実行を許可**。コマンドラインでは `--allow-writes`）。電源を切っても保持されるメモリーに書き込めるベンダーコマンドは、許可しても実行されません（`app/src/crates/bt-host/src/chips.rs` の `denied_opcodes`。現在は CSR の BCCMD）。ブートローダーモードは最初からオンなので、実行後にアダプターを閉じても HCI Reset は送られません。チップの RAM に読み込んだファームウェアは、挿し直すと消えます。

## 手順 4: ローダーを書く

ここからのパスは、リポジトリのルートからの相対パスです。関係するクレートは 3 つあります。

| クレート（パッケージ） | フォルダー | 中身 |
|:--|:--|:--|
| `bt-host` | `app/src/crates/bt-host` | Bluetooth ホスト: ローダー、プローブ、チップの検出 |
| `bluecaster-core`（ライブラリ） | `app/src/crates/core` | 設定とアダプター: どのローダーを動かすか |
| `bluecaster-core-exe` | `app/src/core` | サービスの実行ファイル（`bluecaster-core.exe`）: ウィンドウからの要求、アダプターの説明 |

下の 2 つは、どちらも名前に "core" が付いています。`cargo -p` には、表のパッケージ名を渡します。

ローダーは、自身では I/O を行わないステートマシンです。コントローラーのイベント、自分が出した USB 要求の結果、タイマーを受け取り、アプリが実行する **アクション** を返します。そのため、チップがなくてもテストでき、USB やスレッドの細かい処理をローダーに書く必要もありません。

1. 架空のチップ用の完全なローダーとそのテストである `app/src/crates/bt-host/src/bringup/sample.rs` を、`app/src/crates/bt-host/src/<vendor>.rs`（例: `mediatek.rs`）にコピーします。`app/src/crates/bt-host/src/lib.rs` には、ほかのモジュールとアルファベット順になる位置に `pub mod <vendor>;` を追加します
2. 名前は変えません。型は `Loader`（`<vendor>::Loader` として使います）で、`vendor()` はベンダー名を返します。テストは、サンプルと同じくそのファイルに書きます
3. サンプルで "made up" となっている箇所を、手順 2 と 3 でわかったことにすべて置き換えます。バージョンの確認、USB 要求、ファイル名、ダウンロードのコマンドとその確認方法、ファームウェアの起動方法です
4. 先頭のモジュールコメントを書き直します。チップに必要な処理、手順の順番、それぞれの事実の出典（Windows のドライバーのキャプチャ、メーカーの資料、プローブのレポート）を書いてください。手順の出典として、ほかのドライバーのソースコードを挙げないでください

### インターフェイス

すべて `app/src/crates/bt-host/src/bringup.rs` にあります。

| `ChipLoader` のメソッド | 呼ばれるとき |
|:--|:--|
| `vendor()` | ログの行に使う（"MediaTek"） |
| `start()` | 開いたアダプターで、HCI の初期化の前に 1 回 |
| `on_event(event)` | HCI イベントごと: コード、長さ、パラメーター |
| `on_control(result)` | 最後の `UsbControl` の結果（読んだバイト、または失敗の理由）を渡すとき |
| `on_timeout()` | 最後の `Timer` のタイマーが切れたとき |
| `missing_file_hint(file)` | ローダーが `MissingFile` で終わったときのログ用: 何をどこに置くか（"x.bin from linux-firmware (mediatek/)"） |
| `step()`、`progress()` | アクションをまとめて返すたび: 手順の名前（"download"）と進捗（全体のうち送信済みのコマンドの数）。アダプターの起動中、アダプター画面に表示されます |

`Done` のあと、アプリはローダーを破棄します。遅れて届いた応答がローダーに渡されることはありません。

| `Action` | 動作 |
|:--|:--|
| `Command(bytes)` | HCI コマンド（オペコード、長さ、パラメーター）をコントロールパイプで送ります。HCI キャプチャに記録されます |
| `BulkCommand(bytes)` | `Command` と同じバイトのコマンドを Bulk OUT パイプで送ります。ここでファームウェアを受け取るブートローダーもあります |
| `UsbControl(request)` | デフォルトパイプでのコントロール転送（ベンダーのレジスター）。すぐに実行され、次のアクションの前に `on_control` が結果を受け取ります。HCI キャプチャにはメモとして記録されます |
| `Bootloader(on)` | ローダーが Bulk IN パイプで応答するチップや、HCI Reset を送ってはいけないチップ（受け取ると停止するものがあります）のためのアクションです。「パターン」を参照してください。HCI に普通に応答する ROM のチップには不要です |
| `Timer(ms)` | ローダーのただ 1 つのタイマーを開始、または開始し直します |
| `Done(Ok(outcome))` | 完了。続いて HCI の初期化が行われます。`AlreadyRunning`、`Loaded { firmware, detail }`、`Rom`（`Probed` はプローブ専用） |
| `Done(Err(error))` | アダプターは使えません: `MissingFile(name)`、`BadFile { file, why }`、`Command { opcode, status }`、`Timeout(step)`、`Rejected(code)`、`BadVersion`、`UnsupportedVariant(v)`、`Unsupported(why)`、`Host(why)` |

バイトを手で解析しなくて済むように、ヘルパーが用意されています。

- `hci_command(opcode, params)` はコマンドを組み立てます
- `parse_event(event)` は `HciEvent::Complete { opcode, status, ret }`（ステータスのあとの戻りパラメーター）、`Status { opcode, status }`、`Vendor(params)`（イベント 0xFF）、`Other { code, params }`（ほかのすべてのコード。0xE4 のようなベンダー独自のものも含む）を返します

**ファームウェアのファイル**: ローダーは `FileReader` を受け取り、**フォルダーを含まないファイル名**（`"BT_RAM_CODE_MT7961_1_2_hdr.bin"`）でファイルを要求します。アプリは、ファームウェアフォルダー（`%APPDATA%\BlueCaster\firmware`）と、その中のベンダーのサブフォルダー（`firmware\mediatek\`）を、linux-firmware と同じ配置で探します。このサブフォルダーは手順 6 で設定します。

### パターン

どのパターンもサンプルに含まれています。チップに不要なものは削除してください。

- **1 つのコマンドに 1 つの確認**: コマンドを送ってタイマーを開始し、待ち状態ではそのオペコードの確認だけを受け付けます。確認は Command Complete か、結果があとで届くコマンドならステータス 0 の Command Status です。それ以外は無視するので、遅れて届いた応答でローダーが先に進むことはありません
- **ベンダーイベントで確認するチップ**: ファームウェアの各部分に、Command Complete ではなく独自のイベントで応答するチップがあります。その場合は、そのイベント（コードと、それを識別するパラメーター）だけを待ち、その状態ではそのコマンドの Command Complete やステータス 0 の Command Status を無視します。ただし、どちらかでエラーのステータスが返ったら終了します。サンプルは Command Complete と Command Status を同じ確認として扱うので、このためには `on_event` で状態とイベントの両方を match する必要があります
- **複数のファイル**（パッチのあとに設定ファイル）: 送り始める前にすべてのファイルを読み込んで確認し、順に送ります。最後のファイル名を `Loaded { firmware, .. }` に入れます（ほかのファイル名はログに出します）
- **意図的な待ち時間**（ドライバーが次の手順の前に 100 ms 待つ）: 待つための状態を追加して `Timer(100)` を送ります。その状態での `on_timeout` は、失敗とせずに次の手順を送ります。テストでは、`SimRun::timeouts` がこうした待ちも数えます
- **2 回応答するコマンド**（確認のあとに、「ファームウェアが起動した」のようなベンダーイベントが届く）: 状態を 2 つにし、2 つ目には専用のタイマーを付けます。サンプルの `Launching` と `Starting` がこの例です。イベントに結果コードがあり、それが不正な値なら `Rejected(code)` でローダーを終了します
- Command Complete か Command Status で **エラーのステータス** が返ったら、`Command { opcode, status }` でローダーを終了します
- **ブートローダーモード**: 最初のコマンドの前（バージョンの確認より前）に `Bootloader(true)` を送ります。成功したとき（`Loaded` と `AlreadyRunning`）は、`Done` の直前に `Bootloader(false)` を送り、HCI の初期化が通常どおり動くようにします。失敗したときはチップがまだブートローダーにいるので、オンのままにします。サンプルの `finish()` がこの処理を行います
- **すでに動作中**: バージョンやレジスターが動作用のファームウェアを示していれば、すぐに `AlreadyRunning` で終了します。アダプターは電源が切れるまでファームウェアを保持しており、2 回目のダウンロードは失敗することがあります
- **先にファイルを確認する**: 送り始める前にファイル全体を解析して確認し（問題があれば理由付きの `BadFile`）、ファイルが見つからないときは、その正確なファイル名を示します
- **対象のチップだけ**: ローダーがベンダーのチップの一部だけに対応する場合は、何かを送る前に、アダプターが対象のチップであることを確認します（チップ自身のチップ ID のレジスター、または `ControlRequest::device_descriptor()` で得る USB ID）。対象外なら `Unsupported` で終了します
- チップからわかったこと（チップ ID、ROM のバージョン、選んだファイル）は、`info` で **ログに出します**。レポートに表示されるのはこの行です
- チップのフラッシュや OTP メモリーには決して書き込まないでください。ファイルは `FileReader` を通してだけ読み込みます

## 手順 5: シミュレートしたコントローラーでテストする

テストには `app/src/crates/bt-host/src/bringup/sim.rs` を使います。小さな模擬チップ（`impl SimController`）を書き、キャプチャで実物が応答したとおりに、コマンドとコントロール転送に応答させます。`sim::run(&mut loader, &mut chip)` は、タイマーも含めてアプリとまったく同じようにローダーを動かします。その結果（`SimRun`）には、終了のしかた、送ったすべてのコマンドとそのパイプ（`commands[i].bulk`）、すべてのコントロール転送、最後のブートローダーモード（`bootloader`）、タイマーが切れた回数が入っています。バイトの組み立てや読み取りには、`sim::command_complete`、`command_status`、`vendor_event`、`event`（任意のコード）、`opcode`、`params` を使えます。バルクのコマンドには `bulk_command` で応答します。

サンプルと同じく、少なくとも次の点をテストします。

- ダウンロード全体と、送ったコマンド（数、順番、サイズ、パイプ）
- ローダーがブートローダーモードを使う場合は、すべてのケースでの最後のブートローダーモード
- ファームウェアがすでに動いている場合
- ファイルがない場合と、ファイルが壊れている場合（何も送らないこと）
- 拒否されたコマンド（エラーのステータス）、応答しないコントローラー（タイムアウト）、タイムアウトしうる各待ち状態
- ローダーが USB 転送を使う場合は、その失敗

テストデータには、キャプチャからコピーしたバイトを使うのが最適です。テストの実行方法:

```bat
cargo test -p bt-host
```

## 手順 6: アプリに組み込む

変更するファイルをすべて挙げます。すでに未対応として登録されているベンダー（MediaTek、Qualcomm）でも、新しいベンダーでも同じです。新しいベンダーの場合、バリアントが必要な `match` はコンパイラーが教えてくれます。それ以外の箇所と、登録済みのベンダーの場合の変更はすべて、この一覧で確認するしかありません。

**一部のチップか、ベンダー全体か**: ローダーは通常、ベンダーのチップの一部だけに対応します（MT7921 には対応しても MT7925 には対応しない、など）。その場合、"unsupported" から外してよいのはそのチップだけです。その VID:PID を `app/src/crates/bt-host/src/chips.rs` の `LOADER_CHIPS` に並べ、ベンダーは `listed_chips_only()` に残します。すると、`chips::chip_supported(vendor, vid, pid)` と `chip_support_tag()` がアダプターごとに判定し、アダプター名のタグ、アダプター画面の説明、そこで WinUSB を勧めるかどうか、"not implemented" の警告がその結果に従います。ベンダーを `listed_chips_only()` から外すのは、（Intel のように）ベンダーのすべてのチップに対応するローダーの場合だけです。

{: .note }
各ファイルの改行コードは変えないでください。ソースファイルには CRLF のものも LF のものもあります。エディターや Git Bash の `sed -i` の中には改行コードを黙って書き換えるものがあり、`cargo fmt` では元に戻りません。`git diff --stat` で確認できます。数か所しか変えていないファイルで全行が変更扱いになっていれば、改行コードが変わっています。その場合は、コミットする前に（たとえば VS Code のステータスバーにある改行コードの切り替えで）元に戻してください。

**検出**（`app/src/crates/bt-host/src/chips.rs`）:

1. `ChipVendor`: 新しいベンダーの場合は、バリアント（doc の行付き）と `name()` のアームを追加します。どのベンダーでも、enum の doc コメント（ローダーのあるベンダー）とバリアントの doc の行（"not supported"）を現在の状態に合わせます
2. 一部のチップの場合: その VID:PID を `LOADER_CHIPS` に追加します。さらに、ローダー自身のチップの確認（チップ ID、バージョン）がそれらのエントリーと過不足なく一致することを確かめるテストを、ローダーのモジュールに追加します。ベンダー全体の場合: `listed_chips_only()` から外します
3. `vendor_for_usb()`: ベンダーの USB VID。ほかのメーカーも使う VID は、VID 全体ではなく、必ず `VID if pid == PID` で一致させます。たとえば、モジュールメーカーの VID で出ているノート PC 用モジュール（AzureWave 0x13D3、Foxconn 0x0489）や、pid.codes 0x1209 のような共有の VID です。ここに挙げた VID はどれも、`is_bluetooth_vendor_vid` がそのメーカーのデバイスを Bluetooth アダプターとして扱う原因にもなります。BTstack の Realtek の表か `OEM_TABLE`（どちらもこのファイルの `resolve_realtek_pid` にあります）にある VID:PID は、`vendor_for_usb` が呼ばれる前に Realtek と判定されます。追加する VID:PID がどちらにもないことを確認してください
4. `company_name()`: Read Local Version で得られる Bluetooth SIG の company ID を、まだなければ追加します
5. テスト: `app/src/crates/bt-host/tests/chips.rs`（VID:PID とタグ）

**アダプターを開く**: Bluetooth クラスのデバイス（クラス E0/01/01）は、変更なしで開けます。ROM でベンダークラス（FF）を名乗るチップは、`app/src/crates/core/src/adapter.rs` の `BUILTIN_VENDOR_CLASS` にもその VID:PID を追加する必要があります。クラスは、プローブのレポートの `usb.interfaces` の行で確認できます。

**ローダーを起動する**（Intel と同じように。`intel_loader` で検索してください）:

6. `app/src/crates/bt-host/src/lib.rs`: `intel_loader` のような `HostConfig` のフィールド（`HostConfig::new` での既定値、`BtHost::launch` での分割代入、debug の行も）と、`BtHost::launch` の中の分岐。この分岐は、H4 でないとき（Intel と同じく `&& !tcp`。エミュレーターのコントローラーは実際のチップではないため）だけ、トランスポートを設定し、`setup_hci_and_power_on` の代わりにアプリの起動関数を呼びます。1 回の起動で動くローダーは 1 つだけです。2 つのベンダーのフラグが立っているとき（両方のアダプターが WinUSB）は最初の分岐が優先されるので、自分のアダプターだけを WinUSB にしてテストしてください
7. `app/src/crates/bt-host/src/app.rs`: `start_intel_loader` のような `start_<vendor>_loader`（`use crate::<vendor>;` 付き）。ローダーに `read_firmware_file(&dir, "<vendor>", name)`（サブフォルダー名は linux-firmware と同じ）を渡して、`start_loader` を呼びます
8. `app/src/crates/core/src/adapter.rs` の `configure()`: そのベンダーのアダプターのうち `chips::chip_supported(a.vendor, a.device.vid, a.device.pid)` を満たすものが WinUSB にあり、使用中でないときにフィールドを設定し（`config.intel_loader` と同じように。それ以外には Intel と同じ "in use by another program" のログ）、そこにある `adapter:` の debug の行にも追加します。その下の "firmware loading for this chip is not implemented" の警告は、すでに `chip_supported` に従っています
9. `app/src/crates/core/src/service.rs` の `host_config()`: debug の行にフィールドを追加します

**ウィンドウに出る内容**:

10. `app/src/core/src/state.rs` の `vendor_note()`: `MediaTek | Qualcomm` のアームから外して、新しいキー `firmware.vendor_<vendor>` のアームを作り、そのテストも追加します。この関数は、`chip_supported` が受け入れないチップにはそのアームより前で `firmware.vendor_unsupported` を返すので、ベンダーのほかのチップの表示は変わりません。アダプター画面は、説明が `firmware.vendor_unsupported` でないすべてのアダプターに WinUSB を勧めます
11. `app/lang/*.json`（全 10 言語、それぞれ同じ行）: 新しいキーを追加します。Intel のキー（「Intel アダプター。…」）にならってください。ボタンやフォルダーの名前を書くときは、同じファイルにあるその言語の表記を使います（ファームウェアフォルダーを開くボタンは `firmware.open_folder`）。ベンダー全体の場合: `firmware.vendor_unsupported` からそのベンダー名を外します。一部のチップの場合: ベンダーのほかのチップがまだその文を表示するので、そのまま残します
12. `app/src/ui/src/lib/links.ts`: ベンダーのファームウェアのページ（`LINKS`、`firmwareUrl()`、`vendorFirmwareUrl()` に。Intel の linux-firmware のフォルダーと同じように）。これで、アダプター画面にファイルの入手先が表示されます。テストは `app/src/ui/src/lib/chips.test.ts` に書きます。アダプター画面がこのページを示すのは、説明が `firmware.vendor_unsupported` でないアダプターだけなので、ベンダーのほかのチップには表示されません

**ドキュメント**:

13. `docs/setup.md` と `docs/*/setup.md`（全言語）のチップの表: Intel と同じ形式の行に、対応するチップ、ファームウェアのファイルとその置き場所（ファームウェアフォルダー `%APPDATA%\BlueCaster\firmware`、またはその `<vendor>` サブフォルダー。古い行にはまだ "config folder" と書かれています）を書きます。ウィンドウのボタンは、各言語の `app/lang/*.json` にあるとおりの名前で書きます（アダプター画面は `ui.nav.adapter`、ファームウェアフォルダーを開くボタンは `firmware.open_folder`）。ベンダー全体の場合: "Not supported" の行から外します。一部のチップの場合: ベンダーはその行に残し、まだ対応していないチップを書きます。Firmware の節の最初の文（"Intel and Broadcom adapters may need firmware files of their own"）にそのベンダーを加えます
14. 誰かが実際に使ったら、[検証済みの機器](hardware)（`docs/hardware.md`）に載せます。このページの冒頭（`docs/chip-support.md`、全言語）も更新します。冒頭では、ローダーのないベンダーを挙げています（一部のチップの場合は "the other <vendor> chips"）

## 手順 7: アダプターで試す

リポジトリのルートでビルドし（`cargo build --release -p bluecaster-core-exe`）、インストール済みのアプリを（トレイアイコンから）終了します。そのうえで、HCI キャプチャを取りながら、コアをコマンドラインで実行します。

```bat
target\release\bluecaster-core.exe --cli -u 0e8d:7961 --scan 5 --hci-capture loader.pklg
```

ログに各手順が記録されます。`loader.pklg` は Wireshark で開け、コマンド、イベント、コントロール転送がすべて順番に入っています。アダプターを抜いて（またはシャットダウンして）、もう一度試してください。電源が切れていたチップではダウンロードが成功し、ファームウェアがすでに動いているときは何もしないことを確認します。

{: .note }
WinUSB のアダプターが複数あると、`-u` を指定しても Realtek のアダプターが優先され、そのチップの設定が適用されます。**アダプター > Bluetooth チップ** を **Realtek 以外** にするか、自分のアダプターだけを WinUSB にしてテストしてください。

そのあと、ウィンドウからしばらくヘッドホンに配信し、**Bluetooth を再起動** と挿し直しのどちらでもアダプターが復帰することを確認します。アダプターの起動中は、**アダプター** 画面の Bluetooth の状態の下に、ローダーの状態（「読み込み中 42%」や失敗）が表示されます。開発者モードでは、準備が完了したあとも `step()` の名前と結果が表示されたままになります。

## 手順 8: プルリクエストを出す

プルリクエストを出す前に、次のチェックを順に実行します（環境は[ビルド](building)を参照）。まず `app\src\ui` で UI をチェックします。これは手順 6 の変更（項目 12）の確認です。また、Rust のワークスペースにはウィンドウのクレートが含まれていて、そのビルドにはビルド済みの UI が必要です。

```bat
pnpm install
pnpm build
pnpm exec tsc --noEmit
pnpm exec vitest run
```

次に、リポジトリのルートで実行します。

```bat
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python tools/check_lang.py
```

Node.js がない場合は、テストと clippy を上の 3 つのクレートだけで実行し（`-p bt-host -p bluecaster-core -p bluecaster-core-exe`）、UI のチェックはプルリクエストの CI に任せてください。その旨をプルリクエストに書いてください。`pwsh ./build.ps1 -Test` を使うと、すべてのチェックを実行できます。

Clippy は名前もチェックします。`State` のバリアントを `PatchState` にすると `enum_variant_names` で失敗するので、`Patch` にしてください。`check_lang.py` は、変更前からある "long hints left" を報告することがあります。直す必要があるのは、エラーと不足しているキーだけです。

ほかにも、MediaTek や Qualcomm を未対応として簡単に触れている箇所があります（UI のモックデータとテスト、`Adapter.tsx` と `probe.rs` のコメント、"Not Realtek" の選択肢）。これらは変更しません。

プルリクエストの要件:

- 現在の `develop-v*` ブランチをベースにします
- テストしたアダプター（製品、VID:PID、チップ）と、コールドスタートとウォームスタートの結果を書き、プローブのレポートとダウンロード時の HCI キャプチャを添付します
- ローダーに使った事実の出典を書きます

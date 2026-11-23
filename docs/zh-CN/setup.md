---
title: 安装与设置
layout: default
parent: 首页
nav_order: 1
---

# 安装与设置
{: .no_toc }

## 目录
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## 前提条件

- **Windows 10（22H2）/ 11**（x64 或 ARM64）
- **供 BlueCaster 使用的 USB 蓝牙适配器**：建议使用单独的适配器；USB 类型的内置适配器也可以，但这样 Windows 就无法使用自己的蓝牙了（请参阅[第 2 步](#winusb)）

BlueCaster 会自动为适配器分配 WinUSB 驱动程序，因此不再需要 [Zadig](https://zadig.akeo.ie/)（Zadig 仍可使用，请参阅[改用 Zadig](#zadig)）。

## 安装 BlueCaster

每个[发布版本](https://github.com/SeiyaFunaokaJP/BlueCaster/releases)提供两种形式，内含文件相同：

- **安装程序**（`BlueCaster-<version>-win64-setup.exe`，ARM64 为 `-arm64-setup.exe`）：无需管理员权限即可为当前用户安装，添加“开始”菜单项（**BlueCaster**）和卸载程序；如果缺少 Microsoft Edge WebView2 Runtime，还会一并安装（Windows 11 已自带）。更新可直接在应用内安装（**设置 > 更新 > 立即更新**）。程序安装在 `%LOCALAPPDATA%\BlueCaster\` 中（三个程序、卸载程序，以及存放许可证文本的 `licenses\` 文件夹）
- **便携版**（`BlueCaster-<version>-win64-portable.zip`，ARM64 为 `-arm64-portable.zip`）：解压到任意位置，运行 `bluecaster.exe` 即可。请将三个程序（`bluecaster.exe`、`bluecaster-core.exe`、`bluecaster-driver.exe`）放在一起。便携版只会提示有可用更新，并且需要已安装 WebView2 Runtime

两者都将设置、配置文件、链接密钥和固件保存在 `%APPDATA%\BlueCaster` 中，因此可以在两者之间切换。卸载时会删除该文件夹，除非你关闭**删除用户数据**（静默卸载会保留该文件夹）。

### 从 v1 升级 {#from-v1}

直接运行安装程序覆盖 v1 即可。安装程序的**准备安装**页面会列出将先卸载的早期版本（v1 或任何 2.0.x）。如果 v1 正在运行，安装程序会请你关闭它，然后卸载 v1，但保留设置文件夹：配置文件、耳机配对和固件文件都会沿用，耳机无需重新配对。已用 Zadig 分配了 WinUSB 的适配器会继续使用 WinUSB。如果之前开启了**随 Windows 启动**，现在 Windows 启动时运行的将是 v2。

v2 首次启动时，会先将 v1 的设置复制到 `%APPDATA%\BlueCaster\backup\v1\`，然后再读取。

### 从 2.0.x（A2DP Windows Bridge）升级 {#from-2x}

BlueCaster 3.0.0 是 A2DP Windows Bridge 的新名称。直接运行 3.0.0 的安装程序覆盖已安装的 2.0.x 即可：安装程序会自动卸载旧版本，并保留你的设置。“开始”菜单项和**随 Windows 启动**项会改为“BlueCaster”。

3.0.0 首次启动时，会将文件夹 `%APPDATA%\A2DPWB` 重命名为 `%APPDATA%\BlueCaster`，并把其中的文件整理到子文件夹中（见[设置与文件](architecture#settings-and-files)）。`backup\v2\` 保存了 2.0.x 设置和配对的副本，以便你需要时退回 2.0.2：把这些文件复制回 `%APPDATA%\A2DPWB` 即可。

耳机在重新配对之前，可能仍显示旧的蓝牙名称“A2DPWB”。

## 第 1 步：首次运行设置 {#first-run}

插入 USB 蓝牙适配器并启动 BlueCaster。首次启动时（还没有音频设备，或者没有使用 WinUSB 的适配器），BlueCaster 会打开共四步的**设置向导**。完成或跳过向导后，它不会再自动打开；之后请在**主页**或**音频设备**页面添加设备。

1. **选择适配器**：列出找到的 USB 蓝牙适配器。如果没有你的适配器，请插入后单击**刷新**。绿色的**支持**或红色的**不支持**标签表示 BlueCaster 能否使用该适配器；不支持的适配器会列出，但无法选择
2. **驱动程序和固件**：此步骤会列出将要执行的操作，单击**准备此适配器**即可执行：BlueCaster 会将适配器的驱动程序替换为 WinUSB（Windows 会请求管理员权限，请参阅[第 2 步](#winusb)），并下载该适配器所需的 Realtek 固件文件（请参阅[固件](#firmware)）。BlueCaster 无法下载的固件需要手动复制到固件文件夹。如果有设备是 Windows 通过此适配器配对的（例如内置适配器上的键盘或鼠标），该按钮会先列出这些设备：BlueCaster 使用该适配器期间，这些设备在 Windows 中无法使用；单击**取消**可让它们继续工作。自带 USB 接收器的设备不受影响；要保留其他设备，请为 BlueCaster 使用单独的 USB 适配器。**适配器**页面上的**替换为 WinUSB 驱动程序**也会列出这些设备
3. **查找音频设备**：扫描会自动开始。让音频设备进入配对模式，然后单击旁边的**连接**（或输入地址后单击**使用**）：BlueCaster 会以设备支持的最佳编解码器（自动）保存该设备并连接。随后该设备显示在**已配对的设备**下，带有状态圆点，扫描停止。**添加设备**会断开该设备并重新开始扫描，以便添加另一台设备；第一台设备仍会保存，选错的设备可以在这里删除。如果 Windows 曾通过此适配器配对设备，会显示**导入 Windows 的配对**：单击其中的**导入**会列出这些设备，并已全部勾选（请参阅[第 3 步](#headphones)）。没有设备也可以单击**下一步**，之后再在**音频设备**页面添加。如果已安装 **Hi-Fi Cable**，新设备会捕获它，否则捕获 VB-CABLE。Hi-Fi Cable 增加的延迟最小，因此推荐用于声音；VB-CABLE 推荐用于耳机的麦克风。缺少音频线时，此步骤会显示警告，单击**了解详情**会列出两种音频线：VB-CABLE 的**安装**会下载 VB-Audio 的 VB-CABLE 安装包（1.3 MB）并安装（会请求管理员权限；如果没有出现提示，请重启 Windows）；Hi-Fi Cable 的**下载**会打开 VB-Audio 的网页。已安装的音频线会改为显示**卸载**。两种音频线都是 VB-Audio 的捐赠软件（[vb-cable.com](https://vb-cable.com/)），**关于**页面中也有说明：BlueCaster 不附带 VB-CABLE，捐款归 VB-Audio 所有。VB-Audio 要求公司和其他组织每次安装都购买许可证（[许可](https://vb-audio.com/Services/licensing.htm)）。之后 BlueCaster 首次发现某种音频线时，设为**默认设备**的设备会改为捕获该音频线（VB-CABLE 仅在未安装 Hi-Fi Cable 时如此），麦克风没有可用输出的设备会把麦克风送到该音频线。编解码器之后可以在**编解码器**页面更改
4. **常规设置**：主题、**启动**、**通知**和更新检查，与“设置”中的控件相同，会立即保存。首次设置会开启**随 Windows 启动**和**在托盘中启动**

**稍后设置**会跳过剩余步骤；之后可通过**设置 > 运行设置向导**再次打开向导。

## 第 2 步：为适配器分配 WinUSB {#winusb}

BlueCaster 通过 WinUSB（Windows 自带的通用 USB 驱动程序）直接与 USB 蓝牙适配器通信。建议使用**专用适配器**，这样内置蓝牙在 Windows 中仍可正常使用。

1. 打开**适配器**页面（设置向导的**驱动程序和固件**步骤会通过**准备此适配器**完成第 2 至 4 步）
2. 适配器的**驱动程序**显示为**原始驱动程序**（或**无驱动程序**），即 Windows 或制造商安装的驱动程序，WinUSB 将取代它。单击**替换为 WinUSB 驱动程序**
3. 阅读确认信息后继续：Windows 会请求管理员权限（UAC）
4. 驱动程序现在显示为 **WinUSB 驱动程序**

如果 Windows 无法立即加载 WinUSB（因为适配器正在使用中），BlueCaster 会提示你，驱动程序一行会显示**请重新插拔或重启 Windows**。请拔下适配器后重新插入；如果是内置适配器，请重启 Windows。在此之前，连接会失败，日志中会出现 `failed to initialize WinUSB (error 50)`。

{: .warning }
分配 WinUSB 后，Windows 自带的蓝牙将无法识别该适配器：通过它与 Windows 配对的设备在恢复驱动程序之前都无法使用。键盘、鼠标等普通外设请使用内置蓝牙。你也可以把属于 USB 设备的内置适配器（许多笔记本电脑上的 Realtek 适配器）交给 BlueCaster，但在恢复其驱动程序之前，**这台 PC 将完全无法使用 Windows 蓝牙**（蓝牙设置、鼠标、键盘、其他音频设备）。

同一页面上的**恢复原始驱动程序**会把适配器交还给 Windows：Windows 会重新为其选择自己的驱动程序（通常是蓝牙驱动程序），适配器即可配合 Windows 蓝牙使用。在重新分配 WinUSB 之前，BlueCaster 无法通过该适配器串流。Windows 一次只使用一个蓝牙适配器。如果另一个适配器正在使用，BlueCaster 会在恢复后提示你；要在 Windows 中使用此适配器，请在设备管理器中禁用另一个适配器，然后重新插拔此适配器。BlueCaster 重新配对过的耳机会有新的密钥。对于 Windows 曾通过此适配器配对的耳机，恢复时会把新密钥交给 Windows，因此无需重新配对即可在 Windows 中连接。**限制：**只转移经典配对，不转移 LE 密钥。Windows 通过 LE（LE Audio）使用的设备需要在 Windows 中重新配对（开发者模式下可以尝试转移 LE 密钥：**开发者 > 调试**）。卸载程序也提供同样的操作：卸载向导的一个页面上有**恢复原来的蓝牙驱动程序**（BlueCaster 为适配器分配过 WinUSB 时显示；只请求一次管理员权限）和**删除用户数据**（`%APPDATA%\BlueCaster` 中的设置、编解码器、配对和固件，以及窗口记住的内容，例如**仅显示支持的编解码器**），两项默认都已开启。如果使用 WinUSB 的适配器都是用 Zadig 或 v1 设置的（BlueCaster 没有它们的记录），第一项会显示但默认关闭；开启后由 Windows 为这些适配器选择驱动程序。更新和静默卸载（`/S`）都会保留 WinUSB。已安装 VB-CABLE 或 Hi-Fi Cable 时，同一页面还会提供**移除 VB-CABLE** 和**移除 Hi-Fi Cable**；由于其他应用也可能在使用它们，这两项默认关闭。移除时不会显示 VB-Audio 的安装窗口，只会出现 Windows 的管理员权限提示。随后 Windows 会扫描检测硬件改动，让音频线从“声音”列表中消失；只有在仍然列出时才会建议重启。

### 改用 Zadig {#zadig}

使用 [Zadig](https://zadig.akeo.ie/) 安装的 WinUSB 驱动程序同样可用：

1. 打开 Zadig，选择 **Options > List All Devices**
2. 从下拉列表中选择你的 USB 蓝牙适配器
3. 选择 **WinUSB** 作为目标驱动程序，然后单击 **Replace Driver**

### 在设备管理器中手动操作 {#device-manager}

如果 BlueCaster 的列表中没有该适配器、**适配器**页面上的按钮执行失败，或者你的适配器与同类适配器表现不同，可以在 Windows 的设备管理器（右键单击**开始** > **设备管理器**）中手动完成同样的操作。这两项操作都需要管理员权限。菜单名称可能因 Windows 版本而略有不同。

**替换为 WinUSB 驱动程序**

1. 找到适配器：通常位于**蓝牙**下，显示其自身名称（例如“TP-Link Bluetooth USB Adapter”）；如果驱动程序启动失败，会带有黄色标记。**查看 > 依连接排序设备**可以帮你把它与电脑内置的蓝牙适配器区分开；除非要交给 BlueCaster 使用，否则请勿改动内置适配器
2. 右键单击 > **更新驱动程序** > **浏览我的电脑以查找驱动程序** > **让我从计算机上的可用驱动程序列表中选取**
3. 如果 Windows 询问设备类型，请选择**通用串行总线设备**。取消勾选**显示兼容硬件**，选择厂商 **WinUsb 设备** 和型号 **WinUsb 设备**，单击**下一页**并确认警告
4. 适配器现在显示在**通用串行总线设备**下，名为 **WinUsb 设备**。重启 BlueCaster（或重新打开**适配器**页面）后，BlueCaster 就会像使用 Zadig 时一样使用该适配器

**恢复原来的驱动程序**

1. 在**通用串行总线设备**下右键单击该适配器（**WinUsb 设备**）> **卸载设备**。如果 WinUSB 驱动程序来自 Zadig，还要勾选**尝试删除此设备的驱动程序**，否则 Windows 会再次选择 Zadig 的驱动程序
2. 拔出适配器并重新插入（或 **操作 > 扫描检测硬件改动**）。Windows 会安装自己的驱动程序，适配器会出现在**蓝牙**下
3. 如果适配器在那里带有黄色标记（驱动程序启动失败，例如代码 31），说明厂商的驱动程序不适用于该适配器：右键单击 > **更新驱动程序** > **浏览我的电脑以查找驱动程序** > **让我从计算机上的可用驱动程序列表中选取** > **通用蓝牙适配器**；或者按上面的步骤重新分配 WinUSB，以便在 BlueCaster 中使用

## 第 3 步：查找耳机 {#headphones}

**扫描设备**（在**音频设备**页面上）会通过 USB 适配器搜索附近的设备 30 秒，设备一响应就会列出。在设置向导中，扫描会自动开始，直到有设备连接（**添加设备**会断开该设备并继续扫描）、你单击**停止扫描**或关闭设置向导为止。请先让耳机进入配对模式。BlueCaster 之前配对过的设备会标记为**已配对**。

如果扫描找不到耳机，请在**手动：**下输入蓝牙地址并单击**使用**。BlueCaster 会在你输入或粘贴时自动格式化地址：`AA:BB:CC:DD:EE:FF`、`aa-bb-cc-dd-ee-ff`、`AABBCCDDEEFF` 和 `AA BB CC DD EE FF` 都会变为 `AA:BB:CC:DD:EE:FF`。

在分配 WinUSB 之前，Windows 通过此适配器配对过的耳机可以保留这些配对。**导入 Windows 的配对**（在设置向导的设备步骤中，有此类设备时显示）会列出这些设备并全部勾选，然后导入勾选的设备，使其无需进入配对模式即可连接。Windows 会请求管理员权限。此类设备通常不处于配对模式，因此不会出现在扫描结果中。如果它出现了，或者你输入了它的地址，**连接**或**添加**会先提醒你该设备已由 Windows 配对；**仍然添加**会把它作为新设备重新配对。

**通过 Windows 设置**（如果耳机曾与 PC 的内置蓝牙配对）：
1. 打开 **设置 > 蓝牙和其他设备**
2. 单击你的音频设备
3. 单击 **属性**
4. 即可看到蓝牙地址（格式：`AA:BB:CC:DD:EE:FF`）

**通过命令行**（请先退出 BlueCaster：托盘 > **退出**）：
```
bluecaster-core.exe --cli --scan
```

耳机的编解码器（**编解码器**页面）默认为**自动**，输出源（**音频设备**页面）默认为系统音频；这些默认设置的音质最好。其他内容请参阅[使用方法](usage)。

---

## 固件（Realtek 适配器） {#firmware}

基于 Realtek 的 USB 蓝牙适配器（如 TP-Link UB500、RTL8761BU 适配器）需要专有固件才能工作。CSR 适配器无需固件。Intel、MediaTek MT79xx、Qualcomm 和 Broadcom 适配器可能需要各自的固件文件：请参阅[推荐适配器](#adapters)。

### 在应用中（下载引导）

**适配器**页面（以及设置向导的**驱动程序和固件**步骤）会显示适配器的固件状态：**固件正常**、**无需 Realtek 文件**（BlueCaster 无法识别的适配器，且芯片设为**非 Realtek**），或**未找到固件**并列出所需的文件名。单击**打开下载页面**，在浏览器中打开 [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) 页面，然后单击**打开固件文件夹**打开目标文件夹。下载所需的 `.bin` 文件并放入固件文件夹。

其他厂商的适配器没有**蓝牙芯片**选项，而是用标签显示固件的来源：**来自 MediaTek 的驱动程序**或**来自 Qualcomm 的驱动程序**（从厂商的 Windows 驱动程序读取，改用 WinUSB 后该驱动程序仍保留安装），**来自 Wi-Fi 驱动程序**（Marvell；请保持 Wi-Fi 开启），**来自固件文件夹**（Intel，附有**打开下载页面**、**打开固件文件夹**和**设置指南**），**可选**（Broadcom 的 `.hcd`）或**不需要**（CSR、Barrot、Actions）。BlueCaster 通过 WinUSB 加载固件后，标签显示**已加载**；加载失败时显示**未加载**，同时附有**打开下载页面**、**打开固件文件夹**和一行说明固件来源的文字。

如果电脑上安装了 Realtek 的 Windows 驱动程序，RTL8761B 适配器（如 TP-Link UB500）就无需下载固件。BlueCaster 会直接使用该驱动程序附带的固件。适配器改用 WinUSB 后，这些固件仍保留在电脑上。如需加载其他固件（例如 Linux 的固件），请在**适配器**页面中该适配器的固件区域选择**使用自定义固件**。BlueCaster 随后会加载固件文件夹中的文件。选择**使用驱动程序的固件**即可切换回来。

对于**蓝牙芯片**列表中的芯片，**下载固件**可以代你完成：确认后，BlueCaster 会从 linux-firmware（kernel.org）下载文件，逐个校验预期的 SHA-256，然后保存到固件文件夹。如果 linux-firmware 中没有某个芯片的 config 文件，BlueCaster 会保存一个空的 `_config.bin`，芯片将使用自身的设置。这些文件不属于 BlueCaster，只在你要求时才会下载。

BlueCaster 会检测适配器的芯片组，并显示需要哪些固件文件。

没有固件时，Realtek 适配器仍能用 ROM 代码打开并响应，看起来一切正常，但连接或串流会失败，BlueCaster 会提示固件未加载。之前的会话上传的固件会一直保留在适配器中，直到断电为止。因此即使删除了文件，在重启电脑或拔下适配器之前，适配器也可能继续工作。

### 蓝牙芯片设置

**适配器**页面上的**蓝牙芯片**列表决定 BlueCaster 是否对适配器执行 Realtek 初始化（上传固件）：

| 选项 | 含义 |
|:-------|:--------|
| **自动**（默认） | 每次启动 BlueCaster 时根据检测到的适配器决定。已知的 Realtek 适配器会使用对应的芯片；其他适配器不执行 Realtek 初始化 |
| **非 Realtek** | 从不执行 Realtek 初始化。如果“自动”无法识别你的 Intel、Broadcom、CSR 或其他适配器，请选择此项 |
| **RTL8761BU**、**RTL8822CU**…… | 指定的 Realtek 芯片。“自动”无法识别的 Realtek 适配器（使用自有 VID:PID 的 OEM 产品）需要选择此项。状态行会显示其 `.bin` 文件是否已在固件文件夹中 |
| **Custom** | 使用固件文件夹中的 `custom_fw.bin` 和 `custom_config.bin` |

不要为非 Realtek 适配器选择 Realtek 芯片或 Custom：BlueCaster 会尝试向其上传 Realtek 固件，适配器可能因此无法启动。

### 手动下载

1. 从 [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) 下载与芯片组对应的固件和配置 `.bin` 文件（如 `rtl8761bu_fw.bin` 和 `rtl8761bu_config.bin`）
2. 将这两个文件放入 BlueCaster 固件文件夹（`%APPDATA%\BlueCaster\firmware`，也可通过**打开固件文件夹**打开）

{: .note }
这些固件文件是通过 linux-firmware 项目分发的 Realtek 专有二进制文件，未包含在本仓库中。再分发条款请参阅 [WHENCE](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/WHENCE)。

---

## 推荐适配器 {#adapters}

实际与 BlueCaster 配合使用过的适配器和耳机（含编解码器和报告），请参阅[已验证的硬件](hardware)。

| 芯片组 | 产品示例 | 说明 |
|:--------|:-----------------|:------|
| Realtek | TP-Link UB500、RTL8761BU | 已安装 Realtek 的 Windows 驱动程序时使用其中的固件，否则需要下载固件（见上文）。测试最充分 |
| Realtek | RTL8852BD（`0BDA:B853`）、RTL8852BT、RTL8922A | BlueCaster 从 Realtek 的 Windows 蓝牙驱动程序中读取固件（例如 `rtl8852bd_mp_chip_new.dat`），因此请保留该驱动程序，或将日志中列出的文件放入固件文件夹。尚未在实际硬件上测试 |
| CSR | 通用 CSR8510 适配器 | 开箱即用，无需固件 |
| Barrot | 蓝牙 5.4 / 6.0 适配器（`33FA:0010`、`33FA:0012`） | 使用 ROM 固件运行，无需固件。目前仅有一份用户报告 |
| Marvell | Microsoft Surface Pro / Surface Book 内置（88W8897） | 无需固件文件：Wi-Fi 驱动也会加载蓝牙固件，请保持 Wi-Fi 开启。尚未在实际硬件上测试 |
| Actions | ATS2851 适配器（`10D7:B012`） | 使用 ROM 固件运行，无需固件。该适配器会列出一些它并不支持的命令，BlueCaster 会将其隐藏。尚未在实际硬件上测试 |
| Broadcom | ASUS BT400 (BCM20702) 及其他采用 BCM20702 / BCM20703 / BCM43142 的 USB 适配器和笔记本电脑内置模块 | 报告厂商类 `FF/01/01` 而非蓝牙类 `E0/01/01` 的适配器（BCM20702 OEM 适配器）也能被检测到。使用 ROM 固件运行。如果装有 Broadcom 的 Windows 蓝牙驱动程序，BlueCaster 会在启动时上传该驱动程序中的补丁。如果固件文件夹中有 PatchRAM `.hcd` 文件（如 `BCM20702A1-0b05-17cb.hcd`），BlueCaster 会改用该文件。如果驱动程序的补丁上传失败，BlueCaster 会提示你拔下适配器再重新插入。此后 BlueCaster 不再向该适配器上传补丁。驱动程序的补丁尚未在实际硬件上测试 |
| Intel | Intel 7260 / 3160 / 7265 / 3165 / 3168 / 8260 / 8265 / 9260 / AX200 / AX201 / AX210 / AX211 | 处于 bootloader 模式时（如冷启动后），需要将 linux-firmware `intel/` 中对应的 `ibt-*.sfi` + `ibt-*.ddc` 放入固件文件夹，所需文件名会写在日志里（例如 `ibt-0180-0041.sfi`）。7260 / 3160 / 7265 / 3165 / 3168 由 ROM 运行，无需文件；同一文件夹中有对应的 `ibt-hw-*.bseq` 补丁时会应用该补丁 |
| MediaTek | MT7925、MT7927、MT7922、MT7921、MT7902（AMD RZ717、RZ738、RZ616、RZ608 等 Wi-Fi 网卡） | 需要该芯片的 `BT_RAM_CODE_MT*_hdr.bin`（例如 `BT_RAM_CODE_MT7925_1_1_hdr.bin`）：如果装有 MediaTek 的 Windows 蓝牙驱动程序，BlueCaster 会从中读取；否则请将 linux-firmware `mediatek/` 中的该文件放入固件文件夹，所需文件名会写在日志里。MT7927 尚未在实际硬件上测试 |
| Qualcomm | QCA61x4、QCA9377（ROME）、WCN6855（FastConnect 6900）、WCN785x（FastConnect 7800） | 需要该芯片的 rampatch 和 NVM（例如 `AthrBT_0x00000302.dfu` 和 `ramps_0x00000302_48.dfu`）：如果装有 Qualcomm 的 Windows 蓝牙驱动程序，BlueCaster 会从中读取；否则请将 linux-firmware `qca/` 中的文件（`rampatch_usb_*.bin`、`nvm_usb_*.bin`）放入固件文件夹，所需文件名会写在日志里。尚未在实际硬件上测试 |
| MediaTek | MT7663 | 需要该芯片的补丁文件（例如 `mt7663_patch_e2_hdr.bin`）：如果装有 MediaTek 的 Windows 蓝牙驱动程序，BlueCaster 会从中读取；否则请将该文件放入固件文件夹，所需文件名会写在日志里。尚未在实际硬件上测试 |
| Qualcomm Atheros | AR3012 及其他 AR30xx 芯片（较旧笔记本电脑中的蓝牙 4.0） | 需要该芯片的补丁文件和配置文件（例如 `AthrBT_0x31010000.dfu` 和 `ramps_0x31010000_40.dfu`）：如果装有 Qualcomm Atheros 的 Windows 蓝牙驱动程序，BlueCaster 会从中读取；否则请将这些文件放入固件文件夹，所需文件名会写在日志里。尚未在实际硬件上测试 |
| MediaTek、Qualcomm | 更早的 MediaTek 芯片（如 MT7630 和 MT7650）、其他 Qualcomm 芯片（QCA6290） | 不支持：需要 BlueCaster 尚未提供的固件加载程序（[帮助添加支持](chip-support)） |

对于 LDAC 等高比特率编解码器，USB 蓝牙 5.0+ 适配器通常效果最好。

如果有多个适配器使用 WinUSB，BlueCaster 会使用找到的第一个，除非在**适配器**页面另一个适配器的卡片上点了**使用此适配器**。正在使用的适配器标有**使用中**。

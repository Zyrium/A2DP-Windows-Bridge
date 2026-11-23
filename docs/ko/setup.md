---
title: 설치
layout: default
parent: 홈
nav_order: 1
---

# 설치
{: .no_toc }

## 목차
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## 준비 사항

- **Windows 10 (22H2) / 11** (x64 또는 ARM64)
- **BlueCaster용 USB Bluetooth 어댑터**: 별도의 동글을 권장합니다. USB 방식의 내장 어댑터도 쓸 수 있지만, 그동안 Windows에서는 Bluetooth를 쓸 수 없습니다([2단계](#winusb) 참고)

BlueCaster가 어댑터에 WinUSB 드라이버를 자동으로 할당하므로 [Zadig](https://zadig.akeo.ie/)는 더 이상 필요하지 않습니다(계속 써도 됩니다. [Zadig 사용하기](#zadig) 참고).

## BlueCaster 설치

각 [릴리스](https://github.com/SeiyaFunaokaJP/BlueCaster/releases)는 같은 파일을 두 가지 형식으로 제공합니다.

- **설치 프로그램** (`BlueCaster-<version>-win64-setup.exe`, ARM64는 `-arm64-setup.exe`): 관리자 권한 없이 현재 사용자용으로 설치합니다. 시작 메뉴 항목(**BlueCaster**)과 제거 프로그램을 추가하고, Microsoft Edge WebView2 Runtime이 없으면 함께 설치합니다(Windows 11에는 기본 포함). 업데이트는 앱 안에서 설치합니다(**설정 > 업데이트 > 지금 업데이트**). 프로그램은 `%LOCALAPPDATA%\BlueCaster\`에 설치됩니다(세 프로그램, 제거 프로그램, 라이선스 전문이 담긴 `licenses\` 폴더)
- **포터블** (`BlueCaster-<version>-win64-portable.zip`, ARM64는 `-arm64-portable.zip`): 원하는 위치에 압축을 풀고 `bluecaster.exe`를 실행합니다. 세 프로그램(`bluecaster.exe`, `bluecaster-core.exe`, `bluecaster-driver.exe`)은 같은 폴더에 두세요. 포터블 버전은 업데이트를 알려 주기만 하며, WebView2 Runtime이 설치되어 있어야 합니다

두 형식 모두 설정, 프로필, 링크 키, 펌웨어를 `%APPDATA%\BlueCaster`에 저장하므로 서로 바꿔 쓸 수 있습니다. 제거할 때 **사용자 데이터 삭제**를 끄지 않으면 이 폴더도 삭제됩니다(자동 제거에서는 유지됩니다).

### v1에서 업그레이드 {#from-v1}

v1이 설치된 상태에서 설치 프로그램을 실행하세요. **설치 준비 완료** 페이지에 먼저 제거할 이전 버전(v1 또는 모든 2.0.x)이 표시됩니다. v1이 실행 중이면 종료하라는 안내가 나옵니다. 설치 프로그램은 v1을 제거하지만 설정 폴더는 남겨 둡니다. 프로필, 헤드폰 페어링, 펌웨어 파일이 그대로 이어지므로 헤드폰을 다시 페어링할 필요가 없습니다. Zadig로 이미 WinUSB를 설치한 어댑터는 WinUSB를 그대로 유지합니다. **Windows 시작 시 실행**이 켜져 있었다면 이제 v2가 시작됩니다.

v2는 처음 시작할 때 v1 설정을 읽기 전에 `%APPDATA%\BlueCaster\backup\v1\`에 복사해 둡니다.

### 2.0.x(A2DP Windows Bridge)에서 업그레이드 {#from-2x}

BlueCaster 3.0.0은 A2DP Windows Bridge의 새 이름입니다. 2.0.x가 설치된 상태에서 3.0.0 설치 프로그램을 실행하세요. 설치 프로그램이 이전 버전을 자동으로 제거하고 설정은 그대로 유지합니다. 시작 메뉴 항목과 **Windows 시작 시 실행** 항목의 이름은 "BlueCaster"로 바뀝니다.

3.0.0은 처음 시작할 때 `%APPDATA%\A2DPWB` 폴더의 이름을 `%APPDATA%\BlueCaster`로 바꾸고, 안의 파일을 하위 폴더로 정리합니다([설정과 파일](architecture#settings-and-files) 참고). `backup\v2\`에는 2.0.x 설정과 페어링의 사본이 있습니다. 2.0.2로 돌아가려면 이 파일들을 `%APPDATA%\A2DPWB`에 다시 복사하세요.

다시 페어링하기 전까지는 헤드폰에 이전 Bluetooth 이름 "A2DPWB"가 계속 표시될 수 있습니다.

## 1단계: 첫 실행 설정 {#first-run}

USB Bluetooth 어댑터를 꽂고 BlueCaster를 시작합니다. 처음 시작할 때(오디오 장치가 아직 없거나 WinUSB가 설치된 어댑터가 없을 때) BlueCaster는 4단계로 된 **설정 마법사**를 엽니다. 마법사를 완료하거나 건너뛰면 다시 자동으로 열리지 않습니다. 그 뒤에는 **홈**이나 **오디오 장치**에서 장치를 추가하세요.

1. **어댑터 선택**: 찾은 USB Bluetooth 어댑터가 표시됩니다. 원하는 어댑터가 없으면 꽂은 다음 **새로 고침**을 누르세요. 어댑터마다 녹색 **지원됨** 또는 빨간색 **지원되지 않음** 표시로 BlueCaster에서 쓸 수 있는지 알려 줍니다. 지원되지 않는 어댑터는 목록에 나오지만 선택할 수 없습니다
2. **드라이버 및 펌웨어**: 수행할 작업이 표시되고, **이 어댑터 설정**을 누르면 실행됩니다. BlueCaster가 어댑터의 드라이버를 WinUSB로 바꾸고(Windows가 관리자 권한을 요청합니다. [2단계](#winusb) 참고), 어댑터에 필요한 Realtek 펌웨어 파일을 다운로드합니다([펌웨어](#firmware) 참고). BlueCaster가 다운로드할 수 없는 펌웨어는 직접 펌웨어 폴더에 복사해야 합니다. Windows에서 이 어댑터로 페어링한 장치가 있으면(예: 내장 어댑터에 연결된 키보드나 마우스) 버튼을 누를 때 그 목록을 먼저 보여 줍니다. BlueCaster가 어댑터를 쓰는 동안 이 장치들은 Windows에서 작동하지 않습니다. **취소**를 누르면 그대로 쓸 수 있습니다. 전용 USB 수신기를 쓰는 장치는 영향을 받지 않습니다. 나머지 장치를 계속 쓰려면 BlueCaster에는 별도의 USB 어댑터를 사용하세요. **어댑터** 화면의 **WinUSB 드라이버로 교체**를 누를 때도 이 목록이 표시됩니다
3. **오디오 장치 찾기**: 검색이 자동으로 시작됩니다. 오디오 장치를 페어링 모드로 전환하고 목록에서 옆의 **연결**을 누르세요(또는 주소를 입력하고 **사용**). BlueCaster가 장치가 지원하는 최적의 코덱(자동)으로 장치를 저장하고 연결합니다. 연결된 장치는 **페어링된 장치** 아래에 상태 점과 함께 표시되고, 검색은 멈춥니다. **장치 추가**를 누르면 연결을 끊고 다른 장치를 찾도록 검색을 다시 시작합니다. 처음 장치는 저장된 채로 남고, 잘못 추가한 장치는 여기서 지울 수 있습니다. Windows에서 이 어댑터로 페어링한 장치가 있으면 **Windows 페어링 가져오기**가 표시됩니다. **가져오기** 버튼을 누르면 해당 장치가 모두 체크된 상태로 나열됩니다([3단계](#headphones) 참고). 장치가 없어도 **다음**을 누를 수 있으며, 오디오 장치는 나중에 **오디오 장치**에서 추가할 수 있습니다. 새 장치는 **Hi-Fi Cable**이 설치되어 있으면 Hi-Fi Cable을, 없으면 VB-CABLE을 캡처합니다. Hi-Fi Cable은 지연이 가장 적어 소리용으로 권장하고, 헤드폰 마이크에는 VB-CABLE을 권장합니다. 케이블이 없으면 이 단계에 경고가 표시되고, **자세히 알아보기**에서 두 케이블을 모두 볼 수 있습니다. VB-CABLE의 **설치**를 누르면 VB-Audio의 VB-CABLE 패키지(1.3 MB)를 다운로드해 설치합니다(관리자 권한 요청이 나타납니다. 나타나지 않으면 Windows를 다시 시작하세요). Hi-Fi Cable의 **다운로드**를 누르면 VB-Audio 페이지가 열립니다. 이미 설치된 케이블에는 대신 **제거**가 표시됩니다. **정보** 화면에도 나와 있듯이 두 케이블 모두 VB-Audio의 도네이션웨어입니다([vb-cable.com](https://vb-cable.com/)). BlueCaster에는 VB-CABLE이 들어 있지 않으며, 기부금은 VB-Audio에 전달됩니다. 회사 등 조직에서 쓸 때는 VB-Audio가 설치마다 라이선스를 요구합니다([라이선스](https://vb-audio.com/Services/licensing.htm)). 나중에 BlueCaster가 케이블을 처음 찾으면, **기본 장치**로 설정된 장치는 그 케이블을 캡처하도록 바뀝니다(VB-CABLE은 Hi-Fi Cable이 설치되지 않은 경우에만). 마이크 출력이 작동하지 않는 장치는 마이크 소리를 그 케이블로 보냅니다. 코덱은 나중에 **코덱**에서 바꿀 수 있습니다
4. **일반 설정**: 테마, **시작**, **알림**, 업데이트 확인. 설정 화면과 같은 항목이며 바로 저장됩니다. 첫 설정에서는 **Windows 시작 시 실행**과 **트레이에서 시작**이 켜집니다

**나중에 설정**을 누르면 나머지 단계를 건너뜁니다. **설정 > 설정 마법사 실행**으로 다시 열 수 있습니다.

## 2단계: 어댑터에 WinUSB 할당 {#winusb}

BlueCaster는 Windows에 포함된 범용 USB 드라이버인 WinUSB로 USB Bluetooth 어댑터와 직접 통신합니다. **전용 어댑터**를 권장합니다. 그러면 내장 Bluetooth는 Windows에서 그대로 정상 작동합니다.

1. **어댑터** 화면을 엽니다(설정 마법사의 **드라이버 및 펌웨어** 단계에서는 **이 어댑터 설정**이 2~4단계를 대신합니다)
2. 어댑터의 **드라이버**에 **원래 드라이버**(또는 **드라이버 없음**)가 표시됩니다. Windows나 제조사가 설치한 드라이버로, WinUSB로 바뀔 드라이버입니다. **WinUSB 드라이버로 교체**를 클릭합니다
3. 확인 내용을 읽고 계속합니다. Windows가 관리자 권한(UAC)을 요청합니다
4. 이제 드라이버에 **WinUSB 드라이버**가 표시됩니다

어댑터가 사용 중이라 Windows가 WinUSB를 바로 로드하지 못하면 BlueCaster가 알려 주고, 드라이버 줄에 **다시 꽂거나 Windows 다시 시작**이 표시됩니다. 어댑터를 뽑았다가 다시 꽂으세요. 내장 어댑터라면 Windows를 다시 시작하세요. 그 전까지는 연결에 실패하고 로그에 `failed to initialize WinUSB (error 50)`이 표시됩니다.

{: .warning }
WinUSB가 할당되어 있는 동안에는 Windows의 Bluetooth에서 이 어댑터가 보이지 않습니다. 이 어댑터로 Windows에 페어링한 장치는 드라이버를 복원할 때까지 작동하지 않습니다. 일반 주변 장치(키보드, 마우스 등)에는 내장 Bluetooth를 사용하세요. USB 장치로 연결된 내장 어댑터(많은 노트북의 Realtek 어댑터 등)도 BlueCaster에 맡길 수 있습니다. 하지만 그러면 드라이버를 복원할 때까지 **PC에서 Windows Bluetooth를 전혀 쓸 수 없습니다**(Bluetooth 설정, 마우스, 키보드, 다른 오디오 장치).

같은 화면의 **원래 드라이버 복원**을 누르면 어댑터를 Windows에 돌려줍니다. Windows가 다시 자체 드라이버(보통 Bluetooth 드라이버)를 선택하고, 어댑터를 Windows Bluetooth에서 쓸 수 있게 됩니다. WinUSB를 다시 할당하기 전까지는 BlueCaster가 이 어댑터로 스트리밍할 수 없습니다. Windows는 한 번에 Bluetooth 어댑터 하나만 사용합니다. 다른 어댑터가 켜져 있으면 복원 후 BlueCaster가 알려 줍니다. 이 어댑터를 Windows에서 쓰려면 장치 관리자에서 다른 어댑터를 사용 안 함으로 설정하고 이 어댑터를 다시 꽂으세요. BlueCaster에서 다시 페어링한 헤드폰은 키가 새로 바뀝니다. 원래 Windows에서 이 어댑터로 페어링했던 헤드폰이라면 복원할 때 새 키를 Windows에 넘겨주므로, 다시 페어링하지 않아도 Windows에서 연결됩니다. **제한 사항:** 클래식 페어링만 넘겨주며 LE 키는 넘겨주지 않습니다. Windows에서 LE(LE Audio)로 쓰는 장치는 Windows에서 다시 페어링해야 합니다(개발자 모드에서는 LE 키 넘겨주기를 시험해 볼 수 있습니다: **개발자 > 디버그**). 제거 프로그램에서도 같은 작업을 할 수 있습니다. 제거 마법사의 한 페이지에 **원래 블루투스 드라이버 복원**(BlueCaster가 어댑터에 WinUSB를 할당한 경우. 관리자 권한을 한 번 요청합니다)과 **사용자 데이터 삭제**(`%APPDATA%\BlueCaster`의 설정, 코덱, 페어링, 펌웨어와 **지원되는 코덱만 표시**처럼 창이 기억하는 내용)가 있으며, 둘 다 기본으로 켜져 있습니다. WinUSB를 쓰는 어댑터가 모두 Zadig나 v1로 설정한 것이라면(BlueCaster에 기록이 없으므로) 첫 번째 항목은 꺼진 상태로 표시되고, 켜면 Windows가 드라이버를 고릅니다. 업데이트와 자동 제거(`/S`)에서는 WinUSB가 그대로 유지됩니다. 같은 페이지에는 설치된 경우 **VB-CABLE 제거**와 **Hi-Fi Cable 제거**도 표시됩니다. 다른 앱에서도 쓸 수 있으므로 기본값은 꺼짐입니다. VB-Audio 설치 창 없이 제거되며, 그 전에 Windows가 관리자 권한을 요청합니다. 제거 후 Windows가 하드웨어 변경 사항을 검색하므로 소리 장치 목록에서 케이블이 사라집니다. 그래도 목록에 남아 있을 때만 다시 시작을 권합니다.

### Zadig 사용하기 {#zadig}

[Zadig](https://zadig.akeo.ie/)로 설치한 WinUSB 드라이버도 똑같이 작동합니다.

1. Zadig를 열고 **Options > List All Devices**를 선택합니다
2. 드롭다운에서 USB Bluetooth 어댑터를 선택합니다
3. 대상 드라이버로 **WinUSB**를 선택하고 **Replace Driver**를 클릭합니다

### 장치 관리자에서 직접 하기 {#device-manager}

BlueCaster 목록에 어댑터가 없거나, **어댑터** 화면의 버튼이 실패하거나, 같은 종류의 다른 어댑터와 다르게 동작한다면 Windows 장치 관리자(**시작**을 마우스 오른쪽 버튼으로 클릭 > **장치 관리자**)에서 같은 작업을 직접 할 수 있습니다. 두 작업 모두 관리자 권한이 필요합니다. 메뉴 이름은 Windows 버전에 따라 조금 다를 수 있습니다.

**WinUSB 드라이버로 교체**

1. 어댑터를 찾습니다. 보통 **Bluetooth** 아래에 제품 이름(예: "TP-Link Bluetooth USB Adapter")으로 표시되며, 드라이버가 시작되지 못했으면 노란색 표시가 붙어 있습니다. **보기 > 연결별 장치**를 선택하면 PC 내장 Bluetooth와 구별하기 쉽습니다. BlueCaster에 맡길 생각이 아니라면 내장 Bluetooth는 건드리지 마세요
2. 마우스 오른쪽 버튼 > **드라이버 업데이트** > **내 컴퓨터에서 드라이버 찾아보기** > **컴퓨터의 사용 가능한 드라이버 목록에서 직접 선택**
3. 장치 유형을 물으면 **범용 직렬 버스 장치**를 선택합니다. **호환 가능한 하드웨어 표시**의 선택을 해제하고, 제조업체 **WinUsb 장치**와 모델 **WinUsb 장치**를 선택한 다음 **다음**을 누르고 경고를 확인합니다
4. 어댑터가 **범용 직렬 버스 장치** 아래에 **WinUsb 장치**로 표시됩니다. BlueCaster를 다시 시작하면(또는 **어댑터** 화면을 다시 열면) Zadig를 쓸 때와 똑같이 이 어댑터를 사용합니다

**원래 드라이버로 복원**

1. **범용 직렬 버스 장치** 아래의 어댑터(**WinUsb 장치**)를 마우스 오른쪽 버튼으로 클릭 > **디바이스 제거**. WinUSB 드라이버를 Zadig로 설치했다면 **이 장치의 드라이버를 제거하려고 했습니다**도 선택하세요. 선택하지 않으면 Windows가 다시 Zadig의 드라이버를 고릅니다
2. 어댑터를 뽑았다가 다시 꽂습니다(또는 **동작 > 하드웨어 변경 사항 검색**). Windows가 자체 드라이버를 설치하고 어댑터가 **Bluetooth** 아래에 나타납니다
3. 대신 노란색 표시가 붙어 있다면(드라이버 시작 실패, 예: 코드 31) 제조사 드라이버가 이 어댑터에서 작동하지 않는 것입니다. 마우스 오른쪽 버튼 > **드라이버 업데이트** > **내 컴퓨터에서 드라이버 찾아보기** > **컴퓨터의 사용 가능한 드라이버 목록에서 직접 선택** > **일반 Bluetooth 어댑터**를 선택하세요. BlueCaster에서 쓰려면 위의 방법으로 WinUSB를 다시 할당하세요

## 3단계: 헤드폰 찾기 {#headphones}

**장치 검색**(**오디오 장치** 화면)을 누르면 USB 어댑터로 30초 동안 주변 장치를 찾고, 응답하는 장치부터 목록에 표시합니다. 설정 마법사에서는 검색이 자동으로 시작되어, 장치가 연결되거나(**장치 추가**를 누르면 연결을 끊고 검색을 다시 시작합니다) **검색 중지**를 누르거나 설정 마법사를 닫을 때까지 계속됩니다. 먼저 헤드폰을 페어링 모드로 전환하세요. BlueCaster와 페어링한 적이 있는 장치는 **페어링됨**으로 표시됩니다.

검색으로 찾지 못하면 **직접 입력:**에 Bluetooth 주소를 입력하고 **사용**을 클릭하세요. 입력하거나 붙여 넣은 주소는 바로 형식이 맞춰집니다. `AA:BB:CC:DD:EE:FF`, `aa-bb-cc-dd-ee-ff`, `AABBCCDDEEFF`, `AA BB CC DD EE FF`는 모두 `AA:BB:CC:DD:EE:FF`가 됩니다.

WinUSB를 할당하기 전에 Windows에서 이 어댑터로 페어링한 헤드폰은 그 페어링을 그대로 쓸 수 있습니다. **Windows 페어링 가져오기**(설정 마법사의 장치 단계에서, 해당 장치가 있을 때 표시됨)를 누르면 이런 장치가 모두 체크된 상태로 나열되고, 체크한 장치를 가져와 페어링 모드 없이 연결할 수 있습니다. Windows가 관리자 권한을 요청합니다. 이런 장치는 보통 페어링 모드가 아니라서 검색에 나타나지 않습니다. 검색에 나타나거나 주소를 직접 입력하면 **연결**이나 **추가**를 누를 때 Windows에서 페어링한 장치라는 경고가 먼저 나옵니다. **그래도 추가**를 누르면 새 장치로 다시 페어링합니다.

**Windows 설정에서** (헤드폰을 PC 내장 Bluetooth와 페어링한 경우):
1. **설정 > Bluetooth 및 장치**를 엽니다
2. 오디오 장치를 클릭합니다
3. **속성**을 클릭합니다
4. Bluetooth 주소가 표시됩니다 (형식: `AA:BB:CC:DD:EE:FF`)

**명령줄에서** (먼저 트레이 > **종료**로 BlueCaster를 종료하세요):
```
bluecaster-core.exe --cli --scan
```

헤드폰의 코덱(**코덱** 화면)은 **자동**, 출력 소스(**오디오 장치** 화면)는 시스템 오디오로 시작합니다. 이 기본값으로 최상의 음질을 얻을 수 있습니다. 그 밖의 내용은 [사용법](usage)을 참고하세요.

---

## 펌웨어 (Realtek 어댑터) {#firmware}

Realtek 기반 USB Bluetooth 어댑터(예: TP-Link UB500, RTL8761BU 동글)가 작동하려면 독점 펌웨어가 필요합니다. CSR 어댑터는 펌웨어가 필요 없습니다. Intel, MediaTek MT79xx, Qualcomm, Broadcom 어댑터는 각자의 펌웨어 파일이 필요할 수 있습니다. [권장 어댑터](#adapters)를 참고하세요.

### 앱에서 (안내형 다운로드)

**어댑터** 화면(과 설정 마법사의 **드라이버 및 펌웨어** 단계)에 어댑터의 펌웨어 상태가 표시됩니다. **펌웨어 정상**, **Realtek 파일이 필요 없습니다**(BlueCaster가 인식하지 못하는 어댑터에서 칩이 **Realtek 아님**으로 설정된 경우), 또는 필요한 파일 이름과 함께 **펌웨어를 찾을 수 없음**이 표시됩니다. **다운로드 페이지 열기** 버튼으로 브라우저에서 [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt) 페이지를 열고, **펌웨어 폴더 열기**로 저장할 폴더를 엽니다. 필요한 `.bin` 파일을 다운로드해 펌웨어 폴더에 넣으세요.

다른 제조사의 어댑터에는 **Bluetooth 칩** 선택이 없고, 대신 펌웨어를 어디서 가져오는지 표시됩니다. **MediaTek 드라이버에서** 또는 **Qualcomm 드라이버에서**(제조사의 Windows 드라이버에서 읽음. 이 드라이버는 WinUSB로 바꾼 뒤에도 설치된 채로 남습니다), **Wi-Fi 드라이버에서**(Marvell. Wi-Fi를 켜 두세요), **펌웨어 폴더에서**(Intel. **다운로드 페이지 열기**, **펌웨어 폴더 열기**, **설정 안내** 포함), **선택 사항**(Broadcom의 `.hcd`), **필요 없음**(CSR, Barrot, Actions). BlueCaster가 WinUSB로 펌웨어를 로드하면 **로드됨**, 로드하지 못하면 **로드 안 됨**으로 바뀌고, **다운로드 페이지 열기**, **펌웨어 폴더 열기**와 펌웨어 출처를 알려 주는 줄이 함께 표시됩니다.

RTL8761B 어댑터(예: TP-Link UB500)는 PC에 Realtek Windows 드라이버가 설치되어 있으면 다운로드할 필요가 없습니다. 이 경우 BlueCaster는 그 드라이버의 펌웨어를 사용합니다. 이 펌웨어는 어댑터를 WinUSB로 바꾼 뒤에도 PC에 남아 있습니다. Linux용 등 다른 펌웨어를 불러오려면 **어댑터** 화면에서 어댑터의 펌웨어 영역에 있는 **사용자 지정 펌웨어 사용**을 선택하세요. 그러면 BlueCaster가 펌웨어 폴더의 파일을 불러옵니다. **드라이버 펌웨어 사용**을 선택하면 다시 드라이버의 펌웨어로 돌아갑니다.

**Bluetooth 칩** 목록에 있는 칩이라면 **펌웨어 다운로드**로 이 작업을 자동으로 할 수 있습니다. 확인을 누르면 BlueCaster가 linux-firmware(kernel.org)에서 파일을 다운로드하고, 각 파일을 예상 SHA-256 값과 대조한 뒤 펌웨어 폴더에 저장합니다. linux-firmware에 config 파일이 없는 칩이면 빈 `_config.bin`을 저장하고, 칩은 자체 설정을 사용합니다. 이 파일들은 BlueCaster에 포함되어 있지 않으며, 요청할 때만 다운로드됩니다.

BlueCaster는 어댑터의 칩셋을 감지하고 필요한 펌웨어 파일을 알려 줍니다.

펌웨어가 없어도 Realtek 어댑터는 ROM 코드로 열리고 응답하므로 정상처럼 보입니다. 하지만 연결이나 스트리밍은 실패하고, BlueCaster는 펌웨어가 로드되지 않았다고 알립니다. 또한 이전 세션에서 업로드한 펌웨어는 어댑터의 전원이 꺼질 때까지 남아 있습니다. 그래서 파일을 지워도 PC를 다시 시작하거나 어댑터를 뽑기 전까지는 계속 작동할 수 있습니다.

### Bluetooth 칩 설정

**어댑터** 화면의 **Bluetooth 칩** 목록에서 BlueCaster가 어댑터에 Realtek 초기화(펌웨어 업로드)를 할지 정합니다.

| 선택 | 의미 |
|:-----|:-----|
| **자동** (기본값) | BlueCaster를 시작할 때마다 감지된 어댑터를 보고 정합니다. 알려진 Realtek 어댑터에는 해당 칩이 선택되고, 그 밖의 어댑터에는 Realtek 초기화를 하지 않습니다 |
| **Realtek 아님** | Realtek 초기화를 하지 않습니다. 자동이 어댑터를 인식하지 못할 때 Intel, Broadcom, CSR 등의 어댑터에 사용합니다 |
| **RTL8761BU**, **RTL8822CU**, ... | 해당 Realtek 칩입니다. 자동이 인식하지 못하는 Realtek 기반 어댑터(자체 VID:PID를 쓰는 OEM 제품)에 필요합니다. 상태 줄에 해당 `.bin` 파일이 펌웨어 폴더에 있는지 표시됩니다 |
| **Custom** | 펌웨어 폴더의 `custom_fw.bin`과 `custom_config.bin`을 사용합니다 |

Realtek이 아닌 어댑터에는 Realtek 칩이나 Custom을 선택하지 마세요. BlueCaster가 Realtek 펌웨어를 업로드하려 해서 어댑터가 시작되지 않을 수 있습니다.

### 수동 다운로드

1. [linux-firmware/rtl_bt](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/rtl_bt)에서 칩셋에 맞는 펌웨어 및 config `.bin` 파일을 다운로드합니다 (예: `rtl8761bu_fw.bin`과 `rtl8761bu_config.bin`)
2. 두 파일을 BlueCaster 펌웨어 폴더(`%APPDATA%\BlueCaster\firmware`, **펌웨어 폴더 열기**로도 열 수 있음)에 넣습니다

{: .note }
이 펌웨어 파일은 linux-firmware 프로젝트에서 배포하는 Realtek 독점 바이너리이며, 이 저장소에는 포함되어 있지 않습니다. 재배포 조건은 [WHENCE](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/WHENCE)를 참고하세요.

---

## 권장 어댑터 {#adapters}

BlueCaster에서 실제로 사용한 어댑터와 헤드폰은 코덱, 보고 내용과 함께 [검증된 하드웨어](hardware)에 정리되어 있습니다.

| 칩셋 | 제품 예 | 참고 |
|:-----|:--------|:-----|
| Realtek | TP-Link UB500, RTL8761BU | Realtek Windows 드라이버가 설치되어 있으면 그 펌웨어 사용, 없으면 펌웨어 다운로드 필요(위 참고). 가장 많이 테스트됨 |
| Realtek | RTL8852BD(`0BDA:B853`), RTL8852BT, RTL8922A | BlueCaster가 Realtek의 Windows Bluetooth 드라이버에서 펌웨어(예: `rtl8852bd_mp_chip_new.dat`)를 읽으므로 그 드라이버를 제거하지 마세요. 또는 로그에 표시된 파일을 펌웨어 폴더에 넣으세요. 아직 실제 하드웨어에서 테스트하지 않았습니다 |
| CSR | 일반 CSR8510 동글 | 바로 작동하며 펌웨어 불필요 |
| Barrot | Bluetooth 5.4 / 6.0 동글(`33FA:0010`, `33FA:0012`) | ROM 펌웨어로 작동하며 펌웨어 불필요. 지금까지 사용자 보고 1건 |
| Marvell | Microsoft Surface Pro / Surface Book 내장(88W8897) | 펌웨어 파일 불필요: Wi-Fi 드라이버가 Bluetooth 펌웨어도 로드하므로 Wi-Fi를 켜 두세요. 아직 실제 하드웨어에서 테스트하지 않았습니다 |
| Actions | ATS2851 동글 (`10D7:B012`) | ROM 펌웨어로 작동하며 펌웨어 불필요. 실제로는 지원하지 않는 명령을 지원 목록에 올리므로 BlueCaster가 숨깁니다. 아직 실제 하드웨어에서 테스트하지 않았습니다 |
| Broadcom | ASUS BT400 (BCM20702), 그 밖의 BCM20702 / BCM20703 / BCM43142 동글 및 노트북 내장 모듈 | Bluetooth 클래스 `E0/01/01` 대신 벤더 클래스 `FF/01/01`을 보고하는 어댑터(BCM20702 OEM 동글)도 감지합니다. ROM 펌웨어로 작동합니다. Broadcom의 Windows Bluetooth 드라이버가 설치되어 있으면 BlueCaster가 시작할 때 이 드라이버의 패치를 업로드합니다. 펌웨어 폴더에 PatchRAM `.hcd`(예: `BCM20702A1-0b05-17cb.hcd`)가 있으면 BlueCaster는 드라이버의 패치 대신 이 파일을 사용합니다. 드라이버의 패치 적용에 실패하면 BlueCaster가 어댑터를 뽑았다가 다시 꽂으라고 안내합니다. 그 뒤로 BlueCaster는 해당 어댑터에 패치를 업로드하지 않습니다. 드라이버의 패치는 아직 실제 하드웨어에서 테스트하지 않았습니다 |
| Intel | Intel 7260 / 3160 / 7265 / 3165 / 3168 / 8260 / 8265 / 9260 / AX200 / AX201 / AX210 / AX211 | 부트로더 모드(예: 콜드 부팅 직후)에서는 linux-firmware `intel/`의 해당 `ibt-*.sfi` + `ibt-*.ddc`를 펌웨어 폴더에 넣어야 합니다. 필요한 파일 이름은 로그에 표시됩니다(예: `ibt-0180-0041.sfi`). 7260 / 3160 / 7265 / 3165 / 3168은 ROM으로 작동해 파일이 필요 없으며, 같은 폴더에 해당 `ibt-hw-*.bseq` 패치가 있으면 적용합니다 |
| MediaTek | MT7925, MT7927, MT7922, MT7921, MT7902(AMD RZ717, RZ738, RZ616, RZ608 등 Wi-Fi 카드) | 칩에 맞는 `BT_RAM_CODE_MT*_hdr.bin`(예: `BT_RAM_CODE_MT7925_1_1_hdr.bin`)이 필요합니다. MediaTek의 Windows Bluetooth 드라이버가 설치되어 있으면 BlueCaster가 거기서 읽습니다. 없으면 linux-firmware `mediatek/`의 파일을 펌웨어 폴더에 넣으세요. 필요한 파일 이름은 로그에 표시됩니다. MT7927은 아직 실제 하드웨어에서 테스트하지 않았습니다 |
| Qualcomm | QCA61x4, QCA9377(ROME), WCN6855(FastConnect 6900), WCN785x(FastConnect 7800) | 칩의 rampatch와 NVM(예: `AthrBT_0x00000302.dfu`, `ramps_0x00000302_48.dfu`)이 필요합니다. Qualcomm의 Windows Bluetooth 드라이버가 설치되어 있으면 BlueCaster가 거기서 읽습니다. 없으면 linux-firmware `qca/`의 파일(`rampatch_usb_*.bin`, `nvm_usb_*.bin`)을 펌웨어 폴더에 넣으세요. 필요한 파일 이름은 로그에 표시됩니다. 아직 실제 하드웨어에서 테스트하지 않았습니다 |
| MediaTek | MT7663 | 칩의 패치 파일(예: `mt7663_patch_e2_hdr.bin`)이 필요합니다. MediaTek의 Windows Bluetooth 드라이버가 설치되어 있으면 BlueCaster가 거기서 읽습니다. 없으면 이 파일을 펌웨어 폴더에 넣으세요. 필요한 파일 이름은 로그에 표시됩니다. 아직 실제 하드웨어에서 테스트하지 않았습니다 |
| Qualcomm Atheros | AR3012 등 AR30xx 칩(구형 노트북의 Bluetooth 4.0) | 칩의 패치 파일과 구성 파일(예: `AthrBT_0x31010000.dfu`, `ramps_0x31010000_40.dfu`)이 필요합니다. Qualcomm Atheros의 Windows Bluetooth 드라이버가 설치되어 있으면 BlueCaster가 거기서 읽습니다. 없으면 이 파일들을 펌웨어 폴더에 넣으세요. 필요한 파일 이름은 로그에 표시됩니다. 아직 실제 하드웨어에서 테스트하지 않았습니다 |
| MediaTek, Qualcomm | 이전 MediaTek 칩(MT7630, MT7650 등), 기타 Qualcomm 칩(QCA6290) | 미지원: BlueCaster에 없는 펌웨어 로더가 필요합니다 ([지원 추가 돕기](chip-support)) |

LDAC 같은 고비트레이트 코덱에는 대체로 USB Bluetooth 5.0 이상 어댑터가 가장 좋습니다.

여러 어댑터에 WinUSB가 설치되어 있으면 BlueCaster는 처음 찾은 어댑터를 사용합니다. **어댑터** 화면에서 다른 어댑터 카드의 **이 어댑터 사용**을 누르면 그 어댑터를 사용합니다. 사용 중인 어댑터에는 **사용 중**이 표시됩니다.

# arTerm: 연결이 끊겨도 유지되는 Windows 원격 터미널

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

클라이언트나 VPN 연결이 끊겨도 빌드와 코딩 에이전트를 계속 실행하고, 같은 대화형 셸·변수·작업 디렉터리에 다시 연결하며, 보관된 출력을 확인하세요. 재생 범위는 제한되며 완전한 로그가 아닙니다. 양쪽 모두 Windows이고 호스트는 실행 중이며 로그인 상태여야 합니다. 호스트 재부팅·로그아웃 후에는 복원하지 않습니다.

[설정](#native-setup) · [다운로드와 신뢰 확인](../../DEVELOPMENT-INSTALL.md) · [같은 셸 확인](../../README.md#prove-that-you-returned-to-the-same-shell) · [기술 참조 (영어)](../../README.md#get-connected)

<img src="../../.github/arterm-before-after.png" width="700" alt="전후 비교: 클라이언트 종료, VPN 단절 또는 노트북 재시작 후 같은 원격 셸과 변수, 작업으로 돌아갑니다. 호스트는 실행 중이며 사용자가 로그인한 상태여야 합니다.">

*이 그림은 작업 흐름 설명용이며 실제 세션 캡처나 테스트 증거가 아닙니다. 그림의 표시는 영어입니다. 핵심은 클라이언트 연결이 끊겨도 계속 실행되고 사용자가 로그인한 호스트의 같은 셸, 변수, 작업으로 돌아온다는 것입니다. 호스트 재부팅 후 작업 복원은 지원하지 않습니다.*

- **연결이 끊겨도 작업은 계속됩니다.** 빌드, 코딩 에이전트와 오래 걸리는 명령은 클라이언트 종료, VPN 단절이나 노트북 재시작 중에도 원격 Windows 호스트에서 계속 실행됩니다.
- **빈 셸이 아닌 원래 작업으로 돌아오세요.** 같은 대화형 셸 프로세스에 다시 연결하고 변수와 작업 디렉터리를 유지합니다.
- **자리를 비운 동안의 출력을 확인하세요.** 다시 연결하면 보관된 터미널 출력을 재생합니다. 보관 범위가 제한되어 누락이 있을 수 있으며 완전한 로그가 아닙니다.

일반적인 비대화형 작업에는 기존에 승인된 원격 명령 채널만으로 충분할 수 있습니다. 클라이언트 종료, VPN 끊김 또는 노트북 재시작 뒤 **호스트에서 실행 중인 동일한 대화형 Windows 셸과 환경**에 다시 연결해야 할 때, 문서화된 연결 조건과 로컬 제어 제한을 확인하고 arTerm을 선택하세요. 양쪽 모두 Windows여야 하며 호스트는 실행 중이고 로그인 상태여야 합니다. 호스트 재부팅 후 복구가 아닙니다.

<a id="native-setup"></a>

## 요구 사항과 신뢰

조건: 양쪽 Windows, 터널에 같은 GitHub 계정, 허용된 아웃바운드 연결. 호스트는 실행 중이고 사용자가 로그인한 상태여야 합니다. 호스트 재부팅, 로그아웃, 충돌, 종료 또는 셸 exit 후에는 복원하지 않습니다. 현재 다운로드는 개발 서명이며 공개적으로 신뢰되는 운영 빌드가 아닙니다.

[설치 전에 다운로드와 신뢰 결정을 확인](../../DEVELOPMENT-INSTALL.md). OS 경고나 조직 정책을 우회하지 마세요.

클라이언트와 호스트 모두 Windows가 필요합니다. 호스트는 실행 중이고 사용자가 로그인한 상태여야 합니다. 클라이언트는 Microsoft devtunnel CLI, 호스트는 code-tunnel.exe 같은 호환 네이티브 VS Code tunnel CLI를 사용합니다. 전체 편집기는 선택 사항입니다. 두 터널에 같은 GitHub 계정으로 로그인해야 하며 gh 인증과는 별개입니다. 아웃바운드 연결이 허용되어야 하고 종속 프로그램 다운로드에 동의하며 VS Code server 라이선스를 수락해야 합니다.

[Windows 다운로드](https://github.com/yeelam/arterm/releases/latest)에서 x64 또는 ARM64를 선택하세요. 공개된 v0.7.1 패키지는 개발 인증서로 서명되어 있으며, 공개적으로 신뢰되는 운영 인증서로 서명된 배포본은 아닙니다. 먼저 [ZIP 체크섬 비교](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation)에서 Get-FileHash로 해당 릴리스의 공개 체크섬과 비교합니다. ZIP 해시, CER 인증서 식별, 실행 파일 신뢰 검사는 서로 다릅니다. 실행 전 [읽기 전용 서명 검사](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them)의 Get-AuthenticodeSignature로 압축 해제한 설치 프로그램과 클라이언트를 확인하세요. 고정된 개발 서명자와 일치하고 상태가 Valid여야 합니다. 체크섬 누락·불일치, 알 수 없는 서명자 또는 Valid가 아닌 상태면 중지하세요. 인증서 신뢰는 사용자 명시적 승인과 조직 정책 허용이 별도로 필요하며 이후 다시 검사합니다. OS 경고, Authenticode, SmartScreen, 앱 제어를 우회하지 마세요.

호스트 설정에는 정책이 허용하는 Windows Script Host/VBScript도 필요합니다. 신뢰 추가나 설치 전에 확인하고 차단되면 중지하세요. 정책을 우회하지 마세요.

## 설정

보존 출력, 일반 셸 기록, 복구 파일에는 명령, 경로, 코드 또는 비밀이 있을 수 있습니다. 양쪽에서 보호하고 로그, 캡처, 지원 자료를 공유하기 전 민감한 정보를 가리세요. 자격 증명이 DPAPI로 보호된다고 해서 모든 보관 파일이 암호화되거나 민감한 내용이 없는 것은 아닙니다. 준비 상태 진단의 내용 제외는 더 좁은 범위입니다.

검사에 통과하고 정책이 허용한 뒤 원격에서 arTerm-Host-Setup.exe를 실행하고 새 PowerShell을 엽니다:

```powershell
arterm-host setup --name my-devbox
```

터널 로그인을 마치고 호스트가 출력한 등록 명령을 보관하세요. 로컬에서 arTerm-Client-Setup.exe를 설치하고 새 PowerShell에서 같은 GitHub 계정으로 정확히 그 등록 명령을 실행합니다. 호스트 경로는 실제 출력값을 사용하세요. 설치 프로그램이 초기화하므로 arterm setup은 필요 없습니다. 새 세션 이름으로 로컬에서 연결합니다:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## 같은 셸 확인

연결된 원격 PowerShell에서 실행하고 PID와 폴더를 기록합니다:

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Ctrl+]로 클라이언트를 분리합니다. 단축키가 차단되면 로컬 클라이언트 탭만 닫으세요. 호스트를 실행 및 로그인 상태로 유지하고 로컬에서 같은 명령을 반복합니다:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

다시 연결된 원격 PowerShell에서:

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

같은 PID, kept-on-host, 같은 폴더와 True가 나오면 성공입니다. 다음에도 같은 명령으로 작업을 이어갑니다. 테스트를 마치려면 원격 셸에서 exit를 입력하세요. 프로세스가 종료되어 실행 상태를 복원할 수 없습니다.

## 제한

호스트 재부팅, 로그아웃, 충돌, 종료 또는 셸 종료 후 프로세스를 되살리지 않습니다. 사용자 로그인 시 시작하며 자동 로그인이나 사용자 없는 서비스를 구성하지 않습니다. 출력 재생에는 보존 한도가 있습니다. 개발 브랜치 전체가 릴리스 승인된 것은 아닙니다. 파일 전송 아카이브의 독립 검토와 직접 입력 검증 조건은 미완료이며 임의의 TUI는 인증되지 않았습니다. 파일 자동 차단 해제는 안전 보장이 아닙니다.

재생량은 제한됩니다. 전체 출력이 필요하면 원격 빌드 로그를 보관하세요. 일반 arterm-host stop은 자동 재시작될 수 있습니다. 정지 유지에는 arterm-host stop --disable을 사용하세요.

## 복구 및 되돌리기

실패하면 arterm doctor my-devbox로 계정과 네트워크를 확인하고 필요 시 arterm --login으로 클라이언트 인증을 복구하세요. 복구 기록을 삭제하거나 실행 중인 호스트를 재설치하지 마세요. 업데이트는 기본적으로 활성 세션 중단을 거부합니다. 호스트의 arterm-host stop --disable은 정지 상태를 유지하며 arterm-host start로 다시 시작하지만 잃은 셸은 복원하지 않습니다. 제거 후 사용자 데이터는 남습니다. 인증서 신뢰 철회는 설치 가이드를 참조하세요.

## 고급 로컬 제어

관리형 send/read는 새로 만든 지원 PowerShell/pwsh 세션과 일치하는 신뢰된 로컬 연결 주체가 필요합니다. 기존 셸에는 통합을 추가하지 않습니다. 배포본이 개발 브랜치의 직접 입력/전송 아카이브 검토 조건을 완료하지 않으며 알 수 없는 버전은 미확인입니다.

Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi, Qwen CLI 사용자는 도구가 허용하는 일반 Windows 명령으로 사용할 수 있지만 여섯 제품의 네이티브 통합 인증은 아닙니다. 다른 로컬 제어 터미널은 같은 사용자, 로그인 세션, 무결성·권한 상승 상태와 바이트 단위로 동일한 신뢰된 서명 클라이언트가 필요합니다. 연결 프로세스를 유지하세요. 임의의 다른 컴퓨터 에이전트 공유가 아닙니다.

[English README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [문제 해결](../../README.md#installation-and-troubleshooting-details) · [업데이트 및 중지](../../QUICKSTART.md#upgrade-without-registering-again) · [인증서 신뢰 철회](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

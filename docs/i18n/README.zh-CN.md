# arTerm：断线后继续工作的 Windows 远程终端

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

客户端或 VPN 断线后，构建和编程代理继续运行；重连同一个交互式 shell、变量与工作目录；补看保留的输出——回放有界，不是完整日志。两端须为 Windows，主机持续运行且用户保持登录；不能跨主机重启或注销恢复。

[设置](../../README.md#get-connected) · [核验下载与信任](../../DEVELOPMENT-INSTALL.md) · [验证同一 shell](../../README.md#prove-that-you-returned-to-the-same-shell)

<img src="../../.github/arterm-before-after.png" width="700" alt="前后对比：客户端关闭、VPN 断开或笔记本重启后，重新连接同一个远程 shell，保留变量和工作；主机须保持运行且已登录。">

*这是流程示意图，不是实际会话截图，也不是测试证明。图片标签为英文；要点是客户端断线后可回到同一主机上的 shell、变量和工作，前提是主机持续运行且用户已登录。它不能让工作跨越主机重启。*

- **断线，工作继续。** 构建、编程代理和长时间命令留在远程 Windows 主机运行，不因客户端关闭、VPN 断开或笔记本重启而停止。
- **回到原来的工作，不是空白 shell。** 重连同一个交互式 shell 进程，保留变量和工作目录。
- **补看离开期间的输出。** 重连会回放保留的终端输出；保留量有界，可能存在缺口，不是完整日志。

普通的非交互式任务，用现有且获准的远程命令通道可能已足够。需要在客户端关闭、VPN 断开或笔记本重启后，重新连接到**同一个由主机运行的交互式 Windows shell 及其环境**时，再选择 arTerm，并遵循其连接要求和本地控制限制。两端都须为 Windows，主机须持续运行且用户保持登录；这不是主机重启恢复。


连接断了，工作不用重来。构建、长时间命令和 AI 编程代理留在远程 Windows 主机运行。客户端关闭、VPN 断开或笔记本重启后，回到同一个 shell。

远程主机拥有实际运行的 shell；笔记本只是连接到它。

适用条件：两端 Windows、隧道使用同一 GitHub 账户、允许出站联网；主机须保持运行且已登录。主机重启、注销、崩溃、关机或 shell exit 后不会恢复进程。当前下载为开发签名版本，不是公共受信任的生产版本。

[先核验下载并审阅信任决定](../../DEVELOPMENT-INSTALL.md). 不要绕过系统警告或组织政策。

[下载 x64 / ARM64](https://github.com/yeelam/arterm/releases/latest) · [设置](../../README.md#get-connected) → [验证同一 shell 重连](../../README.md#prove-that-you-returned-to-the-same-shell)

## 要求与信任

客户端和主机都需要 Windows；主机必须保持运行且用户已登录。客户端需要 Microsoft devtunnel CLI，主机需要兼容的原生 VS Code tunnel CLI（如 code-tunnel.exe），完整编辑器不是必需的。两端隧道必须使用同一个 GitHub 账户登录；gh 登录不能代替。需要允许出站联网；依赖下载需同意，主机配置需接受 VS Code server 许可。

从[Windows 下载页](https://github.com/yeelam/arterm/releases/latest)选择 x64 或 ARM64。已核实该链接转到 v0.7.1。包是开发证书签名版本，不是公共受信任的生产签名。先按[安装指南](../../DEVELOPMENT-INSTALL.md)核验校验和与公开证书指纹；仅在明确同意且组织政策允许时添加当前用户信任。不要绕过 Authenticode、SmartScreen 或应用控制。

**信任或安装前：**按[下载 ZIP 校验步骤](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation)用 Get-FileHash 与公开发布的归档校验和比较。CER 指纹不能验证 ZIP 或程序。公开校验和缺失或不匹配时停止；匹配不等于安全或信任批准。



主机设置还需政策允许的 Windows Script Host/VBScript；在安装或信任前确认，受阻时停止，不绕过政策。

## 设置

**保护会话隐私：**保留的终端输出、正常 shell 历史和恢复文件可能包含命令、路径、代码或秘密。两端都应妥善保护，分享日志、截图或支持材料前先脱敏。凭据由 DPAPI 保护，不表示所有文件都加密或不含内容；就绪诊断的有限内容排除不适用于全部会话数据。

运行前按[只读签名检查](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them)用 Get-AuthenticodeSignature 检查解压后的安装器和客户端。ZIP 校验和、CER 身份与可执行文件的 Windows 信任是不同检查；须匹配固定的开发签名者且状态为 Valid。未知签名者或非 Valid 状态必须停止，不能绕过警告。是否信任仍需单独明确批准并符合政策；批准后重新检查。控制仅限同一本地用户、登录会话、完整性/提权上下文和相同签名客户端，不是任意跨机器代理共享。

远程运行 arTerm-Host-Setup.exe，打开新的 PowerShell：

```powershell
arterm-host setup --name my-devbox
```

完成隧道登录，保存主机输出的注册命令。本地运行 arTerm-Client-Setup.exe，打开新的 PowerShell，用同一个 GitHub 账户，原样执行主机打印的注册命令（主机路径以实际输出为准）。安装器已初始化客户端，无需 arterm setup。用新会话名在本地连接：

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## 验证同一 shell

在远程 PowerShell 中执行，记下 PID 和目录：

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

按 Ctrl+] 分离客户端；若快捷键被拦截，只关闭本地客户端标签页。保持主机运行且已登录。在本地重复同一连接命令：

```powershell
arterm connect my-devbox MyProof --shell powershell
```

在重连后的远程 PowerShell 中执行：

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

预期：相同 PID、kept-on-host、相同目录和 True。下次用同一连接命令继续工作。测试结束在远程 shell 输入 exit，会结束进程，不能恢复运行状态。

## 限制

主机重启、注销、崩溃、关闭或 shell 退出后不会复活进程；启动依赖用户登录，不配置自动登录或无用户登录服务。回放输出有保留上限。整个开发分支并未获得发布资格：文件传输独立归档审查和直接输入门槛仍未完成，任意 TUI 未获认证。文件自动解除阻止不代表安全。



回放有限；完整输出请保留远程构建日志。普通 arterm-host stop 可能自动重启；持续停止用 arterm-host stop --disable。

## 故障恢复与撤销

失败先运行 arterm doctor my-devbox，检查两端账户和网络；需要时用 arterm --login 显式恢复客户端登录。不要删除恢复记录或重装运行中的主机。更新默认拒绝中断活动会话。主机上 arterm-host stop --disable 可持续停止，arterm-host start 可重新启动，但不会恢复丢失的 shell。卸载保留用户数据；撤销证书信任见安装指南。

## 高级本地控制

托管 send/read 需新建受支持的 PowerShell/pwsh 会话及匹配的受信任本地连接身份；旧会话不补装集成。发布包并不关闭当前分支的直接输入/传输归档审查门槛，未知版本仍未知。



这些公共命令可供 Copilot CLI、Claude Code、Codex、Gemini CLI、Kimi、Qwen CLI 用户在工具允许时使用，不是六客户端原生集成认证。第二个本地控制终端必须与连接进程具有相同用户、登录会话、完整性/提权上下文，使用字节完全相同的受信任签名客户端；连接进程须保持运行。

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [故障排查](../../README.md#installation-and-troubleshooting-details) · [升级与停止](../../QUICKSTART.md#upgrade-without-registering-again) · [撤销证书信任](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

# arTerm：斷線後仍能繼續執行的 Windows 遠端終端機

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

連線中斷，工作不必重來。讓建置、長時間指令與 AI 程式設計代理留在遠端 Windows 電腦執行。關閉用戶端、VPN 斷線或重新啟動筆電後，仍可回到同一個 shell、變數與工作目錄。

用戶端與主機都必須使用 Windows；主機須保持運作且使用者已登入。用戶端需要 Microsoft devtunnel CLI，主機需要相容的原生 VS Code tunnel CLI（例如 code-tunnel.exe），不一定要安裝完整編輯器。兩端通道須登入同一個 GitHub 帳號；gh 登入不能取代通道驗證。必須允許對外連線；下載相依元件需經同意，主機設定需接受 VS Code server 授權條款。

從 [Windows 下載頁](https://github.com/yeelam/arterm/releases/latest)選擇 x64 或 ARM64。已發布的 v0.7.1 套件採開發憑證簽署，不是公眾信任的正式憑證。先按[公開 ZIP 檢查碼比對](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation)以 Get-FileHash 比對該版本發布的清單。ZIP 檢查碼、CER 憑證身分與執行檔簽章是不同檢查；CER 雜湊不能驗證程式。執行前依[唯讀簽章檢查](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them)用 Get-AuthenticodeSignature 檢查解壓縮後的安裝程式與用戶端，須符合固定的開發簽署者且狀態為 Valid。檢查碼缺失或不符、未知簽署者、非 Valid 狀態都必須停止。信任憑證須另經使用者明確核准且符合組織政策，核准後再檢查；不可略過系統警告、Authenticode、SmartScreen 或應用程式管制。

通過上述檢查且政策允許後，在遠端執行 arTerm-Host-Setup.exe，開啟新的 PowerShell：

```powershell
arterm-host setup --name my-devbox
```

完成通道登入並保留主機印出的用戶端註冊指令。本機安裝 arTerm-Client-Setup.exe，開啟新的 PowerShell，使用同一 GitHub 帳號並原樣執行註冊指令，主機路徑以實際輸出為準。安裝程式已初始化用戶端，不需 arterm setup。使用新的工作階段名稱在本機連線：

```powershell
arterm connect my-devbox MyProof --shell powershell
```

在連線後的遠端 PowerShell 執行，記下 PID 與目錄：

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

按 Ctrl+] 分離用戶端；若終端機攔截快捷鍵，只關閉本機用戶端分頁。保持主機運作且已登入，在本機重複相同指令：

```powershell
arterm connect my-devbox MyProof --shell powershell
```

在重新連線的遠端 PowerShell 執行：

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

預期看到相同 PID、kept-on-host、相同目錄與 True。下次使用相同指令繼續工作。測試結束時在遠端 shell 輸入 exit；這會結束程序，無法恢復執行狀態。



只保留用戶端斷線期間的工作，無法在主機重新啟動、登出、當機、關機或 shell 結束後復活程序。主機在使用者登入時啟動，不設定自動登入或無人登入服務。輸出回放有保留上限。整個開發分支尚未通過發布資格：檔案傳輸的獨立封存審查與直接輸入門檻仍未完成，任意 TUI 未獲認證；自動解除檔案封鎖也不代表安全。

請保護兩端保留的終端機輸出、一般 shell 歷程與復原檔案，可能含指令、路徑、程式碼或機密。分享記錄、截圖或支援資料前先遮蔽敏感內容。DPAPI 保護認證資訊，不表示所有檔案皆加密或不含內容；就緒診斷的內容排除範圍較窄。

Copilot CLI、Claude Code、Codex、Gemini CLI、Kimi、Qwen CLI 使用者可在工具允許時執行一般 Windows 指令，不代表六者皆有原生整合。另一個本機控制終端須與連線程序使用相同使用者、登入工作階段、完整性／提升權限狀態與位元組完全相同的受信任簽署用戶端；連線程序須持續執行。不是任意跨電腦代理共用。

失敗時先執行 arterm doctor my-devbox，檢查帳號與網路，必要時用 arterm --login 恢復用戶端驗證。不要刪除復原記錄或重新安裝運作中的主機。更新預設拒絕中斷活動工作階段。主機上的 arterm-host stop --disable 可持續停止，arterm-host start 可再啟動，但不會復原已失去的 shell。解除安裝保留使用者資料；撤銷憑證信任請見安裝指南。

[English README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [疑難排解](../../README.md#installation-and-troubleshooting-details) · [升級與停止](../../QUICKSTART.md#upgrade-without-registering-again) · [撤銷憑證信任](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

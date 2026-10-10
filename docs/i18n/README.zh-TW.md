# arTerm：斷線後仍能繼續執行的 Windows 遠端終端機

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

用戶端或 VPN 斷線後，建置和程式設計代理繼續執行；重新連線到同一互動式 shell、變數與工作目錄；補看保留的輸出——回放有上限，不是完整日誌。兩端須為 Windows，主機持續運作且使用者保持登入；不能跨主機重新啟動或登出復原。

[設定](#native-setup) · [驗證下載與信任](../../DEVELOPMENT-INSTALL.md) · [驗證同一 shell](../../README.md#prove-that-you-returned-to-the-same-shell) · [技術參考（英文）](../../README.md#get-connected)

<img src="../../.github/arterm-before-after.png" width="700" alt="前後對照：用戶端關閉、VPN 斷線或筆電重新啟動後，回到同一遠端 shell、變數與工作；主機須持續運作且已登入。">

*這是流程示意圖，不是實際工作階段截圖或測試證明。圖中標籤為英文；重點是用戶端斷線後仍能回到同一主機上的 shell、變數與工作，前提是主機持續運作且使用者已登入。主機重新啟動後無法保留執行中的工作。*

- **斷線，工作繼續。** 建置、程式設計代理和長時間指令留在遠端 Windows 主機執行，不因用戶端關閉、VPN 斷線或筆電重新啟動而停止。
- **回到原本的工作，不是空白 shell。** 重新連線到同一互動式 shell 程序，保留變數與工作目錄。
- **補看離開期間的輸出。** 重新連線會回放保留的終端輸出；保留量有上限，可能有缺漏，不是完整日誌。

一般非互動式工作，現有且獲准的遠端命令管道可能已足夠。需要在用戶端關閉、VPN 中斷或筆電重新啟動後，重新連線到**同一個由主機執行的互動式 Windows shell 及其環境**時，再選擇 arTerm，並遵循其連線要求與本機控制限制。兩端都須為 Windows，主機須持續執行且使用者保持登入；這不是主機重新啟動後的復原。

<a id="native-setup"></a>

## 需求與信任

適用條件：兩端 Windows、通道使用同一 GitHub 帳號、允許對外連線；主機須持續運作且已登入。主機重新啟動、登出、當機、關機或 shell exit 後不會復原程序。目前下載採開發憑證簽署，不是公眾信任的正式版本。

[先驗證下載並審閱信任決定](../../DEVELOPMENT-INSTALL.md). 不可略過系統警告或組織政策。

用戶端與主機都必須使用 Windows；主機須保持運作且使用者已登入。用戶端需要 Microsoft devtunnel CLI，主機需要相容的原生 VS Code tunnel CLI（例如 code-tunnel.exe），不一定要安裝完整編輯器。兩端通道須登入同一個 GitHub 帳號；gh 登入不能取代通道驗證。必須允許對外連線；下載相依元件需經同意，主機設定需接受 VS Code server 授權條款。

從 [Windows 下載頁](https://github.com/yeelam/arterm/releases/latest)選擇 x64 或 ARM64。已發布的 v0.7.1 套件採開發憑證簽署，不是公眾信任的正式憑證。先按[公開 ZIP 檢查碼比對](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation)以 Get-FileHash 比對該版本發布的清單。ZIP 檢查碼、CER 憑證身分與執行檔簽章是不同檢查；CER 雜湊不能驗證程式。執行前依[唯讀簽章檢查](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them)用 Get-AuthenticodeSignature 檢查解壓縮後的安裝程式與用戶端，須符合固定的開發簽署者且狀態為 Valid。檢查碼缺失或不符、未知簽署者、非 Valid 狀態都必須停止。信任憑證須另經使用者明確核准且符合組織政策，核准後再檢查；不可略過系統警告、Authenticode、SmartScreen 或應用程式管制。

主機設定還需政策允許的 Windows Script Host/VBScript；安裝或信任前確認，受阻時停止，不略過政策。

## 設定

請保護兩端保留的終端機輸出、一般 shell 歷程與復原檔案，可能含指令、路徑、程式碼或機密。分享記錄、截圖或支援資料前先遮蔽敏感內容。DPAPI 保護認證資訊，不表示所有檔案皆加密或不含內容；就緒診斷的內容排除範圍較窄。

通過上述檢查且政策允許後，在遠端執行 arTerm-Host-Setup.exe，開啟新的 PowerShell：

```powershell
arterm-host setup --name my-devbox
```

完成通道登入並保留主機印出的用戶端註冊指令。本機安裝 arTerm-Client-Setup.exe，開啟新的 PowerShell，使用同一 GitHub 帳號並原樣執行註冊指令，主機路徑以實際輸出為準。安裝程式已初始化用戶端，不需 arterm setup。使用新的工作階段名稱在本機連線：

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## 驗證同一 shell

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

## 限制

只保留用戶端斷線期間的工作，無法在主機重新啟動、登出、當機、關機或 shell 結束後復活程序。主機在使用者登入時啟動，不設定自動登入或無人登入服務。輸出回放有保留上限。整個開發分支尚未通過發布資格：檔案傳輸 ZIP 封裝與解壓縮的獨立審查與直接輸入門檻仍未完成，任意 TUI 未獲認證；自動解除檔案封鎖也不代表安全。

回放有限；完整輸出請保留遠端建置記錄。一般 arterm-host stop 可能自動重啟；持續停止用 arterm-host stop --disable。

## 復原與撤銷

失敗時先執行 arterm doctor my-devbox，檢查帳號與網路，必要時用 arterm --login 恢復用戶端驗證。不要刪除復原記錄或重新安裝運作中的主機。更新預設拒絕中斷活動工作階段。主機上的 arterm-host stop --disable 可持續停止，arterm-host start 可再啟動，但不會復原已失去的 shell。解除安裝保留使用者資料；撤銷憑證信任請見安裝指南。

## 進階本機控制

受管理的 send/read 需新建受支援的 PowerShell/pwsh 工作階段及相符的受信任本機連線身分；舊工作階段不會補上整合。發布套件不會關閉目前分支的直接輸入／傳輸 ZIP 封裝與解壓縮審查門檻，未知版本仍未知。

Copilot CLI、Claude Code、Codex、Gemini CLI、Kimi、Qwen CLI 使用者可在工具允許時執行一般 Windows 指令，不代表六者皆有原生整合。另一個本機控制終端須與連線程序使用相同使用者、登入工作階段、完整性／提升權限狀態與位元組完全相同的受信任簽署用戶端；連線程序須持續執行。不是任意跨電腦代理共用。

[English README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [疑難排解](../../README.md#installation-and-troubleshooting-details) · [升級與停止](../../QUICKSTART.md#upgrade-without-registering-again) · [撤銷憑證信任](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

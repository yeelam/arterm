# arTerm：切断後も作業を続けられる Windows リモート端末

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

接続が切れても、作業はやり直さなくて済みます。ビルド、長時間のコマンド、AI コーディングエージェントをリモート Windows に残し、クライアント終了、VPN 切断、ノート PC 再起動後も同じシェルに戻れます。

両側に Windows が必要です。ホストは稼働し、ユーザーがログインした状態を保ちます。クライアントは Microsoft devtunnel CLI、ホストは互換性のあるネイティブ VS Code tunnel CLI（code-tunnel.exe など）を使います。エディター全体は任意です。両側のトンネルに同じ GitHub アカウントでログインします。gh 認証とは別です。外向き通信、依存ソフトのダウンロードへの同意、VS Code server ライセンスの同意が必要です。

[Windows ダウンロード](https://github.com/yeelam/arterm/releases/latest)で x64 または ARM64 を選びます。確認済みリンクは v0.7.1 に転送されます。パッケージは開発用証明書で署名されており、一般に信頼される本番用証明書による署名ではありません。[インストールガイド](../../DEVELOPMENT-INSTALL.md)でチェックサムと公開証明書の指紋を確認し、明示的同意と組織の許可がある場合のみ現在のユーザーに信頼を追加します。Authenticode、SmartScreen、アプリ制御を回避しないでください。

**信頼追加・インストールの前に：**[ZIP の検証手順](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation)で Get-FileHash を使い、公開リリースのアーカイブチェックサムと比較します。CER の指紋だけでは ZIP や実行ファイルを検証できません。公開チェックサムがない場合や不一致なら停止します。一致は安全性や信頼承認ではありません。

**セッションデータは非公開に：**保持された端末出力、通常のシェル履歴、復旧ファイルにはコマンド、パス、コード、秘密情報が含まれる可能性があります。両側で保護し、ログ、画像、サポート資料を共有する前に機密情報を除去してください。資格情報の DPAPI 保護は全ファイルの暗号化や内容の不在を意味しません。準備状態の診断に限った内容除外は全セッションデータには適用されません。

実行前に[読み取り専用の署名確認](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them)で Get-AuthenticodeSignature を使い、展開したインストーラーとクライアントを調べます。ZIP のハッシュ、CER の識別、実行ファイルの Windows 信頼は別の確認です。固定された開発署名者との一致と Valid が必要です。不明な署名者や非 Valid は停止し、警告を回避しません。信頼は別途、明示的承認とポリシー許可が必要で、その後も再確認します。制御は同じローカルユーザー、ログオン、整合性・昇格状態、同一署名クライアントに限定され、任意の別マシンのエージェント共有ではありません。

ホストで arTerm-Host-Setup.exe を実行し、新しい PowerShell を開きます：

```powershell
arterm-host setup --name my-devbox
```

トンネル認証を完了し、表示された登録コマンドを保存します。ローカルで arTerm-Client-Setup.exe を実行し、新しい PowerShell を開いて同じ GitHub アカウントを使い、ホストの登録コマンドをそのまま実行します。パスは実際の出力に従います。クライアントの初期化は済んでおり arterm setup は不要です。新しいセッション名でローカルから接続します：

```powershell
arterm connect my-devbox MyProof --shell powershell
```

リモート PowerShell で実行し、PID とディレクトリを控えます：

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Ctrl+] で切り離します。効かなければローカルのクライアントタブだけを閉じます。ホストの稼働とログインを保ち、ローカルで同じコマンドを繰り返します：

```powershell
arterm connect my-devbox MyProof --shell powershell
```

再接続したリモート PowerShell で：

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

同じ PID、kept-on-host、同じディレクトリ、True が出れば成功です。次回も同じ接続コマンドで続けます。テストを終えるにはリモートで exit を入力します。プロセスが終了し実行状態は復元できません。

ホストの再起動、ログオフ、クラッシュ、停止、シェル終了後は復活しません。起動はユーザーログイン時で、自動ログインや無人サービスにはしません。出力の再生量は有限です。開発ブランチ全体がリリース認定済みではなく、転送アーカイブの独立審査と直接入力の条件は未完了です。任意の TUI は未認定で、ファイルの自動ブロック解除も安全性を保証しません。

失敗時は arterm doctor my-devbox で診断し、両側のアカウントと通信を確認します。必要なら arterm --login でクライアント認証を回復します。復旧記録を削除したり稼働中のホストを再インストールしたりしないでください。更新は通常、活動中のセッションがあると拒否されます。ホストで arterm-host stop --disable を使うと停止状態を維持し、arterm-host start で再開できますが失われたシェルは復活しません。アンインストールはデータを残します。信頼解除はインストールガイドにあります。

Copilot CLI、Claude Code、Codex、Gemini CLI、Kimi、Qwen CLI の利用者も、ツールが許す通常の Windows コマンドとして使えます。6 製品のネイティブ連携認定ではありません。別のローカル制御端末には同じユーザー、ログオンセッション、整合性・昇格状態、バイト単位で同じ信頼済み署名クライアントが必要です。接続プロセスを動かしたままにします。

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [トラブルシューティング](../../README.md#installation-and-troubleshooting-details) · [更新と停止](../../QUICKSTART.md#upgrade-without-registering-again) · [証明書の信頼解除](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

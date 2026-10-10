# arTerm：切断後も作業を続けられる Windows リモート端末

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

クライアントや VPN が切れてもビルドやコーディングエージェントを動かし続け、同じ対話型シェル・変数・作業ディレクトリへ再接続し、保持された出力を確認できます。再生範囲は有限で、完全なログではありません。両端は Windows、ホストは稼働・ログインを維持する必要があります。ホスト再起動・ログオフ後の復元はできません。

[設定](#native-setup) · [ダウンロードと信頼を確認](../../DEVELOPMENT-INSTALL.md) · [同じシェルを確認](../../README.md#prove-that-you-returned-to-the-same-shell) · [技術リファレンス（英語）](../../README.md#get-connected)

<img src="../../.github/arterm-before-after.png" width="700" alt="接続前後の比較：クライアント終了、VPN 切断、ノート PC 再起動後も同じリモートシェル、変数、作業に戻る。ホストは稼働とログインを維持する必要があります。">

*これはワークフローの模式図で、実際のセッションのスクリーンショットやテスト証拠ではありません。図のラベルは英語です。クライアント側の切断後も、稼働しユーザーがログインしたままのホスト上で、同じシェル・変数・作業に戻れることを示しています。ホスト再起動後の復元はできません。*

- **接続が切れても作業を継続。** ビルド、コーディングエージェント、長時間のコマンドはリモート Windows ホストで動き続け、クライアント終了・VPN 切断・ノート PC 再起動に左右されません。
- **空のシェルではなく、元の作業へ。** 同じ対話型シェルのプロセスに再接続し、変数と作業ディレクトリを引き継ぎます。
- **離席中の出力を確認。** 再接続時に保持された端末出力を再生します。保持範囲は有限で欠落もあり、完全なログではありません。

通常の非対話型ジョブには、既存の承認済みリモートコマンド経路で十分な場合があります。クライアント終了、VPN 切断、ノート PC 再起動後に、**ホスト上で動き続ける同じ対話型 Windows シェルと環境**へ再接続したい場合に、文書化された接続条件とローカル制御の制約を確認して arTerm を選んでください。両端は Windows、ホストは稼働中かつログイン中である必要があり、ホスト再起動後の復元ではありません。

<a id="native-setup"></a>

## 要件と信頼

対象：両側 Windows、同じ GitHub アカウントでのトンネル認証、許可された外向き通信。ホストは稼働しユーザーがログインした状態が必要です。ホスト再起動、ログオフ、クラッシュ、停止、シェルの exit 後は復元しません。現在の配布は開発用署名で、一般に信頼される本番用ビルドではありません。

[インストール前にダウンロードと信頼判断を確認](../../DEVELOPMENT-INSTALL.md). OS 警告や組織のポリシーを回避しないでください。

両側に Windows が必要です。ホストは稼働し、ユーザーがログインした状態を保ちます。クライアントは Microsoft devtunnel CLI、ホストは互換性のあるネイティブ VS Code tunnel CLI（code-tunnel.exe など）を使います。エディター全体は任意です。両側のトンネルに同じ GitHub アカウントでログインします。gh 認証とは別です。外向き通信、依存ソフトのダウンロードへの同意、VS Code server ライセンスの同意が必要です。

[Windows ダウンロード](https://github.com/yeelam/arterm/releases/latest)で x64 または ARM64 を選びます。確認済みリンクは v0.7.1 に転送されます。パッケージは開発用証明書で署名されており、一般に信頼される本番用証明書による署名ではありません。[インストールガイド](../../DEVELOPMENT-INSTALL.md)でチェックサムと公開証明書の指紋を確認し、明示的同意と組織の許可がある場合のみ現在のユーザーに信頼を追加します。Authenticode、SmartScreen、アプリ制御を回避しないでください。

**信頼追加・インストールの前に：**[ZIP の検証手順](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation)で Get-FileHash を使い、公開リリースのアーカイブチェックサムと比較します。CER の指紋だけでは ZIP や実行ファイルを検証できません。公開チェックサムがない場合や不一致なら停止します。一致は安全性や信頼承認ではありません。

ホスト設定にはポリシーで許可された Windows Script Host/VBScript も必要です。信頼追加・インストール前に確認し、禁止なら停止してポリシーを回避しません。

## セットアップ

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

## 同じシェルを確認

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

## 制限

ホストの再起動、ログオフ、クラッシュ、停止、シェル終了後は復活しません。起動はユーザーログイン時で、自動ログインや無人サービスにはしません。出力の再生量は有限です。開発ブランチ全体がリリース認定済みではなく、転送アーカイブの独立審査と直接入力の条件は未完了です。任意の TUI は未認定で、ファイルの自動ブロック解除も安全性を保証しません。

再生量は有限です。完全な出力が必要ならリモートにビルドログを残します。通常の arterm-host stop は自動再起動される場合があり、停止維持は arterm-host stop --disable です。

## 復旧と元に戻す方法

失敗時は arterm doctor my-devbox で診断し、両側のアカウントと通信を確認します。必要なら arterm --login でクライアント認証を回復します。復旧記録を削除したり稼働中のホストを再インストールしたりしないでください。更新は通常、実行中のセッションがあると拒否されます。ホストで arterm-host stop --disable を使うと停止状態を維持し、arterm-host start で再開できますが失われたシェルは復活しません。アンインストールはデータを残します。信頼解除はインストールガイドにあります。

## 高度なローカル制御

管理された send/read は新しい対応 PowerShell/pwsh セッションと一致する信頼済みローカル接続元が必要です。既存シェルには後付けしません。配布版で開発ブランチの直接入力・転送アーカイブ審査が完了するわけではなく、不明な版は不明です。

Copilot CLI、Claude Code、Codex、Gemini CLI、Kimi、Qwen CLI の利用者も、ツールが許す通常の Windows コマンドとして使えます。6 製品のネイティブ連携認定ではありません。別のローカル制御端末には同じユーザー、ログオンセッション、整合性・昇格状態、バイト単位で同じ信頼済み署名クライアントが必要です。接続プロセスを動かしたままにします。

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [トラブルシューティング](../../README.md#installation-and-troubleshooting-details) · [更新と停止](../../QUICKSTART.md#upgrade-without-registering-again) · [証明書の信頼解除](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

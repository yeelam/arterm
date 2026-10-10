# arTerm: bağlantı kesilince devam eden Windows uzak terminali

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

İstemci/VPN bağlantısı kesilse de derlemeler ve kodlama ajanları çalışır; aynı etkileşimli kabuğa, değişkenlere ve çalışma dizinine dönün; saklanan çıktıyı okuyun: sınırlı yeniden oynatma, tam günlük değil. İki taraf da Windows; uzak bilgisayar çalışır ve kullanıcı oturumu açık kalır. Uzak bilgisayar yeniden başlatılırsa veya kullanıcı oturumu kapatılırsa kurtarma yoktur.

[Kur](#native-setup) · [İndirmeyi ve güveni doğrula](../../DEVELOPMENT-INSTALL.md) · [Aynı kabuğu doğrula](../../README.md#prove-that-you-returned-to-the-same-shell) · [Teknik başvuru (İngilizce)](../../README.md#get-connected)

<img src="../../.github/arterm-before-after.png" width="700" alt="Önce ve sonra: istemciyi kapattıktan, VPN kesildikten veya dizüstünü yeniden başlattıktan sonra aynı uzak kabuğa, değişkenlere ve çalışmaya dönün; uzak bilgisayar çalışır ve kullanıcı oturumu açık kalır.">

*Bu bir iş akışı çizimidir; gerçek oturum ekran görüntüsü veya test kanıtı değildir. Görseldeki etiketler İngilizcedir. Anlatılan fayda, istemci bağlantısı kesildikten sonra aynı kabuğa, değişkenlere ve çalışmaya dönmektir; uzak bilgisayar çalışmalı ve kullanıcı oturumu açık kalmalıdır. Uzak bilgisayar yeniden başlatılırsa süreçler korunmaz.*

- **İstemci olmadan da iş sürer.** Derlemeler, ajanlar ve uzun komutlar istemci kapanması, VPN kesintisi veya dizüstü yeniden başlatması sırasında uzak Windows bilgisayarında çalışmayı sürdürür.
- **Boş bir kabuğa değil, işinize dönün.** Aynı etkileşimli kabuk sürecine, değişkenleri ve çalışma dizini korunarak yeniden bağlanın.
- **Siz yokken olanları okuyun.** Yeniden bağlantıda saklanan terminal çıktısı oynatılır; saklama sınırlıdır, boşluklar olabilir ve bu tam bir günlük değildir.

Sıradan etkileşimsiz işler için mevcut, onaylanmış uzak komut kanalınız yeterli olabilir. İstemci kapanması, VPN kesintisi veya dizüstü bilgisayarın yeniden başlatılmasından sonra **sunucuda çalışan aynı etkileşimli Windows kabuğuna ve ortamına** yeniden bağlanmanız gerektiğinde, belgelenen bağlantı koşullarına ve yerel denetim sınırlarına uyarak arTerm seçin. İki uç da Windows olmalı, sunucu çalışır ve kullanıcı oturum açmış durumda kalmalıdır; sunucunun yeniden başlatılmasından sonra kurtarma sağlamaz.

<a id="native-setup"></a>

## Gereksinimler ve güven

İki tarafta Windows, tünellerde aynı GitHub hesabı ve izin verilen giden bağlantılar gerekir. Uzak bilgisayar çalışmalı, kullanıcı oturumu açık kalmalıdır. Uzak bilgisayarın yeniden başlaması, kullanıcı çıkışı, çökmesi, kapanması veya kabukta exit sonrası süreçler kurtarılmaz. Güncel indirmeler geliştirme imzalıdır; genel olarak güvenilen bir imzaya sahip üretim derlemeleri değildir.

[Kurulumdan önce indirmeyi doğrulayın ve güven kararını değerlendirin](../../DEVELOPMENT-INSTALL.md). OS uyarılarını veya kuruluş politikasını atlatmayın.

Hem istemci bilgisayarı hem de uzak ana makine Windows kullanmalıdır. Uzak bilgisayar çalışır durumda ve kullanıcı oturumu açık kalmalıdır. İstemci Microsoft devtunnel CLI; uzak bilgisayar ise code-tunnel.exe gibi uyumlu bir VS Code tunnel CLI yürütülebilir dosyası kullanır. Tam düzenleyici isteğe bağlıdır. İki tünelde aynı GitHub hesabıyla oturum açın; gh kimlik doğrulaması ayrı bir işlemdir. Giden bağlantılara izin, bağımlılık indirmelerine onay ve VS Code server lisansının kabulü gerekir.

[Windows indirmelerinden](https://github.com/yeelam/arterm/releases/latest) x64 veya ARM64 seçin. Yayımlanan v0.7.1, genel olarak güvenilen bir üretim sertifikasıyla değil, bir geliştirme sertifikasıyla imzalanmıştır. Önce Get-FileHash ile [ZIP sağlama toplamını](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) aynı sürümün yayımlanmış listesiyle karşılaştırın. ZIP özeti, CER kimliği ve yürütülebilir dosyanın Windows güveni ayrı denetimlerdir. Çalıştırmadan önce çıkarılmış kurucuları ve istemcileri Get-AuthenticodeSignature ile [salt okunur imza denetiminden](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them) geçirin. İmzalayanın kimliği, kılavuzda belirtilen geliştirme sertifikasının kimliğiyle eşleşmeli ve imza durumu `Valid` olmalıdır. Eksik veya farklı sağlama toplamı, bilinmeyen imzalayan ya da Valid olmayan durumda durun. Sertifikaya güven için ayrıca açık kullanıcı onayı ve kuruluş politikası izni gerekir; ardından yeniden denetleyin. İşletim sistemi uyarılarını, Authenticode, SmartScreen veya uygulama denetimlerini atlatmayın.

Sunucu ayrıca politikanın izin verdiği Windows Script Host/VBScript gerektirir. Güven/kurulum öncesi kontrol edin; engelliyse durun, politikayı atlatmayın.

## Kurulum

Saklanan çıktı, normal kabuk geçmişi ve kurtarma dosyalarında komutlar, yollar, kod veya sırlar olabilir. İki bilgisayarda koruyun; günlükleri, görüntüleri ve destek dosyalarını paylaşmadan önce hassas bilgileri gizleyin. Kimlik bilgilerinin DPAPI koruması tüm dosyaların şifreli veya içeriksiz olduğu anlamına gelmez. Hazır olma tanılarının içerik dışlamaları daha dar kapsamlıdır.

Denetimler başarılı ve politika izin veriyorsa uzak bilgisayarda arTerm-Host-Setup.exe çalıştırıp yeni PowerShell açın:

```powershell
arterm-host setup --name my-devbox
```

Tünel girişini tamamlayıp sunucunun yazdırdığı kayıt komutunu saklayın. Yerelde arTerm-Client-Setup.exe kurun, yeni PowerShell açın ve aynı GitHub hesabıyla bu kayıt komutunu gerçek sunucu yoluyla aynen çalıştırın. İstemci zaten hazırlanmıştır; arterm setup gerekmez. Yeni oturum adıyla yerelden bağlanın:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## Aynı kabuğu doğrulama

Bağlı uzak PowerShell içinde çalıştırıp PID ve dizini not edin:

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Ctrl+] ile yalnızca istemciyi ayırın. Kısayol engelleniyorsa sadece yerel istemci sekmesini kapatın. Uzak bilgisayar ve kullanıcı oturumu açık kalsın. Aynı komutu yerelde tekrarlayın:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

Yeniden bağlanan uzak PowerShell içinde:

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

Aynı PID, kept-on-host, aynı dizin ve True beklenir. Daha sonra da aynı bağlantı komutuyla devam edin. Denemeyi bitirmek için uzak kabukta exit yazın: süreç biter ve çalışma durumu geri getirilemez.

## Sınırlar

Uzak bilgisayar yeniden başlatılır, kullanıcı çıkış yapar, sistem çöker veya kapanır ya da kabuktan çıkılırsa süreçler geri getirilmez. Sunucu kullanıcı girişinde başlar; otomatik giriş veya oturum açılmamış hizmet kurmaz. Çıktı tekrarının saklama sınırı vardır. Geliştirme dalının tamamı yayıma uygun olarak doğrulanmamıştır: dosya aktarım arşivlerinin bağımsız incelemesi ve doğrudan giriş koşulları tamamlanmamıştır. Rastgele TUI davranışları için sertifikasyon yapılmamıştır; dosya engelinin otomatik kaldırılması güvenlik garantisi değildir.

Çıktı tekrarı sınırlıdır; tüm çıktı için uzak derleme günlüğünü saklayın. arterm-host stop otomatik başlayabilir; arterm-host stop --disable durdurulmuş durumda kalmasını sağlar.

## Kurtarma ve geri alma

Hatada arterm doctor my-devbox ile hesapları ve ağı kontrol edin; gerekiyorsa arterm --login ile istemci girişini düzeltin. Kurtarma kayıtlarını silmeyin veya çalışan sunucuyu yeniden kurmayın. Güncellemeler varsayılan olarak etkin oturumları kesmez. Sunucuda arterm-host stop --disable durdurulmuş durumda kalmasını sağlar; arterm-host start yeniden başlatır ama kayıp kabukları geri getirmez. Kaldırma kullanıcı verilerini korur. Sertifika güvenini kaldırmak için kılavuza bakın.

## Gelişmiş yerel kontrol

Yönetilen send/read yeni desteklenen PowerShell/pwsh oturumu ve bağlantıyla eşleşen güvenilir yerel kimlik gerektirir. Eski kabuklara entegrasyon eklenmez. Yayın, dalın doğrudan giriş/aktarım arşivi incelemesi koşullarını kapatmaz; bilinmeyen sürümler doğrulanmamıştır.

Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi ve Qwen CLI kullanıcıları araçları izin verirse normal Windows komutları kullanabilir; bu altı istemci için ürünlere özgü entegrasyon sertifikası değildir. Başka yerel denetleyici aynı kullanıcı, giriş oturumu, bütünlük/yükseltme bağlamı ve bayt düzeyinde aynı güvenilir imzalı istemci gerektirir. Bağlantı süreci çalışır kalsın. Farklı makinelerdeki ajanlara gelişigüzel paylaşım değildir.

[English README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Sorun giderme](../../README.md#installation-and-troubleshooting-details) · [Güncelleme ve durdurma](../../QUICKSTART.md#upgrade-without-registering-again) · [Sertifika güvenini kaldırma](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

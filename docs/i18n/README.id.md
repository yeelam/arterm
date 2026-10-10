# arTerm: terminal Windows jarak jauh tetap berjalan setelah koneksi putus

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

Build dan agen pemrograman tetap berjalan saat klien/VPN terputus; hubungkan kembali ke shell interaktif, variabel dan direktori kerja yang sama; lihat keluaran tersimpan: pemutaran ulang terbatas, bukan log lengkap. Kedua sisi Windows; host tetap berjalan dan pengguna tetap masuk. Tidak ada pemulihan setelah host dimulai ulang atau pengguna keluar dari sesi Windows.

[Siapkan](#native-setup) · [Periksa unduhan dan kepercayaan](../../DEVELOPMENT-INSTALL.md) · [Buktikan shell yang sama](../../README.md#prove-that-you-returned-to-the-same-shell) · [Referensi teknis (Inggris)](../../README.md#get-connected)

<img src="../../.github/arterm-before-after.png" width="700" alt="Sebelum dan sesudah: setelah menutup klien, kehilangan VPN atau memulai ulang laptop, kembali ke shell jarak jauh, variabel dan pekerjaan yang sama; host tetap berjalan dan pengguna masih masuk.">

*Ilustrasi alur, bukan tangkapan sesi nyata atau bukti pengujian. Label gambar berbahasa Inggris. Intinya: setelah klien terputus, kembali ke shell, variabel dan pekerjaan yang sama pada host yang tetap berjalan dengan pengguna masih masuk. Proses tidak bertahan setelah host dimulai ulang.*

- **Pekerjaan berlanjut tanpa klien.** Build, agen dan perintah berdurasi panjang tetap berjalan pada host Windows jarak jauh saat klien ditutup, VPN putus atau laptop dimulai ulang.
- **Kembali ke pekerjaan, bukan shell kosong.** Hubungkan kembali ke proses shell interaktif yang sama, beserta variabel dan direktori kerjanya.
- **Lihat keluaran selama Anda pergi.** Saat terhubung kembali, keluaran terminal tersimpan diputar ulang; penyimpanannya terbatas, mungkin ada celah dan ini bukan log lengkap.

Untuk pekerjaan noninteraktif biasa, saluran perintah jarak jauh yang sudah disetujui mungkin cukup. Pilih arTerm ketika perlu terhubung kembali ke **shell Windows interaktif yang sama beserta lingkungannya, yang berjalan di host**, setelah klien ditutup, VPN terputus, atau laptop dimulai ulang, sesuai persyaratan koneksi dan batas kontrol lokal yang didokumentasikan. Kedua sisi harus Windows; host harus tetap berjalan dan pengguna tetap masuk. Ini bukan pemulihan setelah host dimulai ulang.

<a id="native-setup"></a>

## Persyaratan dan kepercayaan

Cocok dengan Windows di kedua sisi, akun GitHub yang sama untuk terowongan dan koneksi keluar yang diizinkan. Host harus tetap berjalan dan pengguna masuk. Mulai ulang, logout, kegagalan atau matinya host, maupun exit shell tidak memulihkan proses. Unduhan saat ini bertanda tangan pengembangan, bukan build produksi yang dipercaya publik.

[Periksa unduhan dan tinjau keputusan kepercayaan sebelum instalasi](../../DEVELOPMENT-INSTALL.md). Jangan lewati peringatan OS atau kebijakan organisasi.

Klien dan host harus memakai Windows. Host harus tetap menyala dengan pengguna masih masuk. Klien memakai Microsoft devtunnel CLI; host memakai VS Code tunnel CLI native yang kompatibel, misalnya code-tunnel.exe. Editor lengkap opsional. Kedua terowongan harus masuk dengan akun GitHub yang sama; autentikasi gh terpisah. Koneksi keluar harus diizinkan, unduhan dependensi membutuhkan persetujuan, dan lisensi VS Code server harus diterima.

Pilih x64 atau ARM64 dari [unduhan Windows](https://github.com/yeelam/arterm/releases/latest). v0.7.1 yang telah diterbitkan memakai tanda tangan pengembangan, bukan sertifikat produksi yang dipercaya publik. Sebelum instalasi atau penambahan kepercayaan, gunakan Get-FileHash untuk [membandingkan ZIP](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) dengan checksum publik versi tersebut. Hash ZIP, identitas CER, dan kepercayaan Windows terhadap tanda tangan executable adalah pemeriksaan berbeda. Sebelum menjalankan, periksa installer dan klien yang diekstrak dengan Get-AuthenticodeSignature melalui [pemeriksaan hanya-baca](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them): penanda tangan pengembangan harus cocok dengan identitas yang ditetapkan dan statusnya Valid. Checksum hilang/tidak cocok, penanda tangan tak dikenal, atau status selain Valid berarti berhenti. Kepercayaan sertifikat membutuhkan persetujuan eksplisit pengguna dan izin kebijakan organisasi secara terpisah, lalu pemeriksaan ulang. Jangan melewati peringatan OS, Authenticode, SmartScreen, atau kontrol aplikasi.

Host juga memerlukan Windows Script Host/VBScript yang diizinkan kebijakan. Periksa sebelum kepercayaan/instalasi; jika diblokir, berhenti tanpa melewati kebijakan.

## Penyiapan

Output yang disimpan, riwayat shell biasa, dan berkas pemulihan dapat berisi perintah, jalur, kode, atau rahasia. Lindungi di kedua komputer dan samarkan informasi sensitif sebelum berbagi log, tangkapan layar, atau paket dukungan. Perlindungan kredensial DPAPI tidak berarti semua berkas terenkripsi atau tanpa konten. Pengecualian konten diagnostik kesiapan cakupannya lebih sempit.

Setelah pemeriksaan lulus dan kebijakan mengizinkan, jalankan arTerm-Host-Setup.exe pada host jarak jauh, lalu buka PowerShell baru:

```powershell
arterm-host setup --name my-devbox
```

Selesaikan login terowongan dan simpan perintah pendaftaran yang dicetak host. Instal arTerm-Client-Setup.exe secara lokal, buka PowerShell baru dengan akun GitHub yang sama, lalu jalankan persis perintah pendaftaran dengan jalur host sebenarnya. Klien sudah diinisialisasi; arterm setup tidak diperlukan. Hubungkan dari komputer lokal dengan nama sesi baru:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## Membuktikan shell yang sama

Di PowerShell jarak jauh yang terhubung, jalankan dan catat PID serta direktori:

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Tekan Ctrl+] untuk melepaskan klien. Jika terminal menangkap pintasan itu, tutup hanya tab klien lokal. Biarkan host menyala dan pengguna masuk. Ulangi perintah yang sama secara lokal:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

Di PowerShell jarak jauh setelah tersambung kembali:

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

Hasil yang diharapkan: PID yang sama, kept-on-host, direktori yang sama, dan True. Gunakan perintah yang sama untuk melanjutkan di kemudian hari. Akhiri pengujian dengan exit di shell jarak jauh: proses berakhir dan status berjalannya tidak dapat dipulihkan.

## Batasan

Proses tidak dihidupkan kembali setelah host dimulai ulang, pengguna keluar, host macet atau dimatikan, maupun shell keluar. Host dimulai saat pengguna masuk; tidak mengatur login otomatis atau layanan tanpa pengguna masuk. Pemutaran ulang output memiliki batas retensi. Seluruh cabang pengembangan belum memenuhi kualifikasi rilis: tinjauan independen arsip transfer berkas dan persyaratan input langsung belum selesai. TUI sembarang tidak tersertifikasi; membuka blokir berkas otomatis bukan jaminan keamanan.

Pemutaran ulang terbatas; simpan log build jarak jauh untuk seluruh output. Host yang dihentikan dengan arterm-host stop dapat berjalan kembali secara otomatis; arterm-host stop --disable mempertahankan keadaan berhenti.

## Pemulihan dan pembatalan

Jika gagal, periksa akun dan jaringan dengan arterm doctor my-devbox; gunakan arterm --login untuk memulihkan login klien bila perlu. Jangan hapus catatan pemulihan atau instal ulang host yang berjalan. Pembaruan secara default menolak memutus sesi aktif. Pada host, arterm-host stop --disable mempertahankan keadaan berhenti; arterm-host start memulai kembali tetapi tidak memulihkan shell yang hilang. Penghapusan instalasi mempertahankan data pengguna; lihat panduan untuk mencabut kepercayaan sertifikat.

## Kontrol lokal lanjutan

send/read terkelola memerlukan sesi PowerShell/pwsh baru yang didukung dan identitas koneksi lokal tepercaya yang cocok. Integrasi tidak dipasang secara retroaktif pada sesi shell yang sudah ada. Rilis tidak menutup persyaratan input langsung/tinjauan arsip transfer cabang; versi tak dikenal tetap belum diverifikasi.

Pengguna Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi, dan Qwen CLI dapat memakai perintah Windows biasa jika alatnya mengizinkan; bukan sertifikasi integrasi native keenam klien. Terminal kontrol lokal lain harus memakai pengguna, sesi login, konteks integritas/elevasi yang sama dan klien bertanda tangan tepercaya yang identik per byte. Pertahankan proses koneksi. Ini bukan berbagi agen sembarang antar komputer.

[English README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Pemecahan masalah](../../README.md#installation-and-troubleshooting-details) · [Pembaruan dan penghentian](../../QUICKSTART.md#upgrade-without-registering-again) · [Mencabut kepercayaan sertifikat](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

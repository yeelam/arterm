# arTerm: terminal Windows từ xa tiếp tục chạy sau khi mất kết nối

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

Giữ các tác vụ biên dịch và tác nhân lập trình tiếp tục chạy khi mất kết nối máy khách/VPN; nối lại đúng shell tương tác, biến và thư mục làm việc; xem lại đầu ra được giữ: phát lại có giới hạn, không phải nhật ký đầy đủ. Hai đầu đều là Windows; máy chủ vẫn chạy và người dùng vẫn đăng nhập. Không phục hồi sau khi máy chủ khởi động lại hoặc người dùng đăng xuất.

[Thiết lập](#native-setup) · [Kiểm tra bản tải và tin cậy](../../DEVELOPMENT-INSTALL.md) · [Kiểm tra cùng shell](../../README.md#prove-that-you-returned-to-the-same-shell) · [Tài liệu kỹ thuật (tiếng Anh)](../../README.md#get-connected)

<img src="../../.github/arterm-before-after.png" width="700" alt="Trước và sau: sau khi đóng máy khách, mất VPN hoặc khởi động lại laptop, quay lại cùng shell từ xa, biến và công việc; máy chủ vẫn chạy và người dùng vẫn đăng nhập.">

*Đây là hình minh họa quy trình, không phải ảnh chụp phiên thực tế hay bằng chứng kiểm thử. Nhãn trong hình bằng tiếng Anh. Ý chính: sau khi máy khách mất kết nối, bạn trở lại cùng shell, biến và công việc nếu máy chủ vẫn chạy và người dùng vẫn đăng nhập. Không giữ tiến trình qua lần khởi động lại máy chủ.*

- **Công việc tiếp tục khi máy khách ngắt kết nối.** Các tác vụ biên dịch, tác nhân và lệnh chạy lâu vẫn tiếp tục chạy trên máy chủ Windows từ xa khi đóng máy khách, mất VPN hoặc khởi động lại laptop.
- **Trở lại công việc, không phải shell trống.** Nối lại đúng tiến trình shell tương tác, giữ nguyên biến và thư mục làm việc.
- **Xem đầu ra trong lúc bạn vắng mặt.** Khi nối lại, đầu ra terminal được giữ sẽ được phát lại; lượng giữ có giới hạn, có thể thiếu đoạn và không phải nhật ký đầy đủ.

Với công việc thông thường không cần tương tác, kênh lệnh từ xa hiện có đã được phê duyệt có thể là đủ. Chọn arTerm khi cần kết nối lại với **chính shell Windows tương tác và môi trường đang chạy trên máy chủ** sau khi đóng máy khách, mất VPN hoặc khởi động lại máy tính xách tay, tuân thủ điều kiện kết nối và giới hạn điều khiển cục bộ đã ghi rõ. Cả hai đầu phải là Windows; máy chủ phải tiếp tục chạy và người dùng vẫn đăng nhập. Đây không phải khôi phục sau khi máy chủ khởi động lại.

<a id="native-setup"></a>

## Yêu cầu và tin cậy

Phù hợp khi cả hai dùng Windows, đường hầm dùng cùng tài khoản GitHub và cho phép kết nối ra ngoài. Máy chủ phải chạy và người dùng vẫn đăng nhập. Không phục hồi tiến trình sau khởi động lại, đăng xuất, sự cố, tắt máy chủ hoặc exit shell. Bản tải hiện tại được ký cho phát triển, không phải bản sản xuất được tin cậy công khai.

[Kiểm tra bản tải và cân nhắc việc tin cậy trước khi cài](../../DEVELOPMENT-INSTALL.md). Không bỏ qua cảnh báo hệ điều hành hoặc chính sách tổ chức.

Cả máy khách và máy chủ đều cần Windows. Máy chủ phải tiếp tục chạy và người dùng vẫn đăng nhập. Máy khách dùng Microsoft devtunnel CLI; máy chủ dùng VS Code tunnel CLI gốc tương thích, chẳng hạn code-tunnel.exe. Không bắt buộc cài toàn bộ trình soạn thảo. Đăng nhập hai đường hầm bằng cùng tài khoản GitHub; xác thực gh là riêng biệt. Cần cho phép kết nối ra ngoài, đồng ý tải thành phần phụ thuộc và chấp nhận giấy phép VS Code server.

Chọn x64 hoặc ARM64 tại [trang tải Windows](https://github.com/yeelam/arterm/releases/latest). Bản v0.7.1 đã phát hành được ký bằng chứng chỉ phát triển, không phải chứng chỉ sản xuất được tin cậy công khai. Trước hết dùng Get-FileHash [đối chiếu ZIP](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) với tổng kiểm tra công bố của đúng bản phát hành. Hàm băm ZIP, danh tính CER và độ tin cậy chữ ký tệp thực thi là các kiểm tra khác nhau. Trước khi chạy, dùng Get-AuthenticodeSignature theo [kiểm tra chỉ đọc](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them) cho bộ cài và máy khách đã giải nén. Phải khớp người ký phát triển được xác định bằng dấu vân tay SHA-256 của chứng chỉ ghi trong hướng dẫn và có trạng thái Valid. Thiếu hoặc sai tổng kiểm tra, người ký không rõ hoặc trạng thái khác Valid thì dừng. Tin cậy chứng chỉ cần người dùng chấp thuận rõ ràng và chính sách tổ chức cho phép riêng, rồi kiểm tra lại. Không bỏ qua cảnh báo hệ điều hành, Authenticode, SmartScreen hoặc kiểm soát ứng dụng.

Máy chủ còn cần Windows Script Host/VBScript được chính sách cho phép. Kiểm tra trước khi tin cậy/cài đặt; nếu bị chặn thì dừng, không vượt chính sách.

## Thiết lập

Đầu ra lưu lại, lịch sử shell thông thường và tệp khôi phục có thể chứa lệnh, đường dẫn, mã hoặc bí mật. Bảo vệ trên cả hai máy và che thông tin nhạy cảm trước khi chia sẻ nhật ký, ảnh chụp hay gói hỗ trợ. DPAPI bảo vệ thông tin xác thực không có nghĩa mọi tệp được mã hóa hoặc không có nội dung. Việc loại trừ nội dung trong chẩn đoán sẵn sàng có phạm vi hẹp hơn.

Sau khi kiểm tra đạt và chính sách cho phép, chạy arTerm-Host-Setup.exe từ xa rồi mở PowerShell mới:

```powershell
arterm-host setup --name my-devbox
```

Hoàn tất đăng nhập đường hầm và giữ lệnh đăng ký do máy chủ in ra. Cài arTerm-Client-Setup.exe trên máy cục bộ, mở PowerShell mới với cùng tài khoản GitHub, rồi chạy đúng lệnh đăng ký và đường dẫn máy chủ thực tế. Máy khách đã được khởi tạo; không cần arterm setup. Kết nối cục bộ bằng tên phiên mới:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## Kiểm tra cùng shell

Trong PowerShell từ xa vừa kết nối, chạy và ghi lại PID cùng thư mục:

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Nhấn Ctrl+] để tách máy khách. Nếu terminal chặn phím tắt, chỉ đóng tab máy khách cục bộ. Giữ máy chủ chạy và người dùng đăng nhập. Lặp lại cùng lệnh ở máy cục bộ:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

Trong PowerShell từ xa sau khi kết nối lại:

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

Kết quả mong đợi là cùng PID, kept-on-host, cùng thư mục và True. Những lần sau dùng cùng lệnh để tiếp tục. Để kết thúc thử nghiệm, nhập exit trong shell từ xa: tiến trình kết thúc và không thể khôi phục trạng thái chạy.

## Giới hạn

Không phục hồi tiến trình sau khi máy chủ khởi động lại, đăng xuất, gặp sự cố, tắt máy hoặc thoát shell. Máy chủ khởi chạy khi người dùng đăng nhập; không thiết lập đăng nhập tự động hay dịch vụ không có người dùng đăng nhập. Lượng đầu ra phát lại có giới hạn. Toàn bộ nhánh phát triển chưa đủ điều kiện phát hành: đánh giá độc lập các tệp lưu trữ được truyền và các điều kiện nhập trực tiếp vẫn chưa hoàn tất. TUI tùy ý không được chứng nhận; tự động bỏ chặn tệp không bảo đảm an toàn.

Phát lại có giới hạn; giữ nhật ký biên dịch từ xa nếu cần toàn bộ đầu ra. arterm-host stop có thể tự khởi chạy lại; arterm-host stop --disable giữ trạng thái dừng.

## Khôi phục và hoàn tác

Nếu lỗi, chạy arterm doctor my-devbox để kiểm tra tài khoản và mạng; dùng arterm --login khôi phục đăng nhập máy khách khi cần. Không xóa bản ghi khôi phục hoặc cài lại máy chủ đang chạy. Cập nhật mặc định từ chối ngắt phiên hoạt động. Trên máy chủ, arterm-host stop --disable giữ trạng thái dừng; arterm-host start khởi chạy lại nhưng không phục hồi shell đã mất. Gỡ cài đặt giữ dữ liệu người dùng; xem hướng dẫn để thu hồi tin cậy chứng chỉ.

## Điều khiển cục bộ nâng cao

send/read được quản lý cần phiên PowerShell/pwsh mới được hỗ trợ và danh tính kết nối cục bộ đáng tin cậy khớp nhau. Không bổ sung tích hợp vào shell cũ. Các yêu cầu về nhập trực tiếp và việc đánh giá độc lập quy trình đóng gói, giải nén ZIP khi truyền tệp của nhánh vẫn chưa được đáp ứng; phiên bản chưa rõ vẫn chưa được kiểm chứng.

Người dùng Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi và Qwen CLI có thể dùng lệnh Windows thông thường nếu công cụ cho phép; không phải chứng nhận tích hợp gốc cho sáu máy khách. Terminal điều khiển cục bộ khác phải chạy dưới cùng người dùng và phiên đăng nhập, có cùng mức toàn vẹn của token bảo mật Windows và trạng thái nâng quyền, đồng thời dùng máy khách được ký, được tin cậy và giống từng byte. Giữ tiến trình kết nối chạy. Không phải chia sẻ tùy ý giữa tác nhân trên nhiều máy.

[English README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Khắc phục sự cố](../../README.md#installation-and-troubleshooting-details) · [Cập nhật và dừng](../../QUICKSTART.md#upgrade-without-registering-again) · [Thu hồi tin cậy chứng chỉ](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

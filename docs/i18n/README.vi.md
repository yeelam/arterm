# arTerm: terminal Windows từ xa tiếp tục chạy sau khi mất kết nối

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

Mất kết nối, không mất công việc. Để bản dựng, lệnh dài và tác nhân lập trình chạy trên Windows từ xa. Sau khi đóng máy khách, mất VPN hoặc khởi động lại laptop, bạn quay lại cùng shell, biến và thư mục làm việc.

Cả máy khách và máy chủ đều cần Windows. Máy chủ phải tiếp tục chạy và người dùng vẫn đăng nhập. Máy khách dùng Microsoft devtunnel CLI; máy chủ dùng VS Code tunnel CLI gốc tương thích, chẳng hạn code-tunnel.exe. Không bắt buộc cài toàn bộ trình soạn thảo. Đăng nhập hai đường hầm bằng cùng tài khoản GitHub; xác thực gh là riêng biệt. Cần cho phép kết nối ra ngoài, đồng ý tải thành phần phụ thuộc và chấp nhận giấy phép VS Code server.

Chọn x64 hoặc ARM64 tại [trang tải Windows](https://github.com/yeelam/arterm/releases/latest). Bản v0.7.1 đã phát hành được ký bằng chứng chỉ phát triển, không phải chứng chỉ sản xuất được tin cậy công khai. Trước hết dùng Get-FileHash [đối chiếu ZIP](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) với tổng kiểm tra công bố của đúng bản phát hành. Hàm băm ZIP, danh tính CER và độ tin cậy chữ ký tệp thực thi là các kiểm tra khác nhau. Trước khi chạy, dùng Get-AuthenticodeSignature theo [kiểm tra chỉ đọc](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them) cho bộ cài và máy khách đã giải nén. Phải khớp người ký phát triển đã cố định và có trạng thái Valid. Thiếu hoặc sai tổng kiểm tra, người ký không rõ hoặc trạng thái khác Valid thì dừng. Tin cậy chứng chỉ cần người dùng chấp thuận rõ ràng và chính sách tổ chức cho phép riêng, rồi kiểm tra lại. Không bỏ qua cảnh báo hệ điều hành, Authenticode, SmartScreen hoặc kiểm soát ứng dụng.

Sau khi kiểm tra đạt và chính sách cho phép, chạy arTerm-Host-Setup.exe từ xa rồi mở PowerShell mới:

```powershell
arterm-host setup --name my-devbox
```

Hoàn tất đăng nhập đường hầm và giữ lệnh đăng ký do máy chủ in ra. Cài arTerm-Client-Setup.exe trên máy cục bộ, mở PowerShell mới với cùng tài khoản GitHub, rồi chạy đúng lệnh đăng ký và đường dẫn máy chủ thực tế. Máy khách đã được khởi tạo; không cần arterm setup. Kết nối cục bộ bằng tên phiên mới:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

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

Không phục hồi tiến trình sau khi máy chủ khởi động lại, đăng xuất, gặp sự cố, tắt máy hoặc thoát shell. Máy chủ khởi chạy khi người dùng đăng nhập; không thiết lập đăng nhập tự động hay dịch vụ không có người dùng đăng nhập. Lượng đầu ra phát lại có giới hạn. Toàn bộ nhánh phát triển chưa đủ điều kiện phát hành: đánh giá độc lập các tệp lưu trữ được truyền và các điều kiện nhập trực tiếp vẫn chưa hoàn tất. TUI tùy ý không được chứng nhận; tự động bỏ chặn tệp không bảo đảm an toàn.

Đầu ra lưu lại, lịch sử shell thông thường và tệp khôi phục có thể chứa lệnh, đường dẫn, mã hoặc bí mật. Bảo vệ trên cả hai máy và che thông tin nhạy cảm trước khi chia sẻ nhật ký, ảnh chụp hay gói hỗ trợ. DPAPI bảo vệ thông tin xác thực không có nghĩa mọi tệp được mã hóa hoặc không có nội dung. Việc loại trừ nội dung trong chẩn đoán sẵn sàng có phạm vi hẹp hơn.

Người dùng Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi và Qwen CLI có thể dùng lệnh Windows thông thường nếu công cụ cho phép; không phải chứng nhận tích hợp gốc cho sáu máy khách. Terminal điều khiển cục bộ khác phải chạy dưới cùng người dùng và phiên đăng nhập, có cùng mức toàn vẹn của token bảo mật Windows và trạng thái nâng quyền, đồng thời dùng máy khách được ký, được tin cậy và giống từng byte. Giữ tiến trình kết nối chạy. Không phải chia sẻ tùy ý giữa tác nhân trên nhiều máy.

Nếu lỗi, chạy arterm doctor my-devbox để kiểm tra tài khoản và mạng; dùng arterm --login khôi phục đăng nhập máy khách khi cần. Không xóa bản ghi khôi phục hoặc cài lại máy chủ đang chạy. Cập nhật mặc định từ chối ngắt phiên hoạt động. Trên máy chủ, arterm-host stop --disable giữ trạng thái dừng; arterm-host start khởi chạy lại nhưng không phục hồi shell đã mất. Gỡ cài đặt giữ dữ liệu người dùng; xem hướng dẫn để thu hồi tin cậy chứng chỉ.

[English README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Khắc phục sự cố](../../README.md#installation-and-troubleshooting-details) · [Cập nhật và dừng](../../QUICKSTART.md#upgrade-without-registering-again) · [Thu hồi tin cậy chứng chỉ](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)

<p align="center">
 <img src="public/pet-logo.png?raw=true" alt="OpenCapX" height="100px"/>
<h1 align="center">OpenCapX</h1>
<div align="center">
 <strong>
    Chặn đứng yêu cầu từ AI agent của bạn, giữ mọi quyền hệ thống an toàn và trong tầm kiểm soát, trao cho mọi model những năng lực đa phương thức còn thiếu, và đưa agent tiến xa hơn với capability do bạn định nghĩa qua plugin cùng quy tắc.
 </strong>
</div>
<br/>
<p align="center">
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="macOS" src="https://img.shields.io/badge/-macOS-black?style=for-the-badge&logo=apple&logoColor=white" />
</a>
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="Windows" src="https://img.shields.io/badge/Windows-0078D6?style=for-the-badge&logo=windows&logoColor=green" />
</a>
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="Linux" src="https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" />
</a>
</p>

<p align="center">
    <a href="./README.md">English</a> | <a href="./README-CN.md">简体中文</a> | Tiếng Việt
</p>

Claude Code, Codex và OpenCode đã biết cách viết code. Nhưng chúng không thể nhìn thấy màn hình của bạn, nói thành tiếng, theo dõi một thư mục, hay hiển thị trạng thái trên desktop. OpenCapX chính là phần thân desktop và lớp capability lấp vào khoảng trống đó, đồng thời là cánh cổng chắn ngay trước nó. Các provider đa phương thức có sẵn trao cho mọi model con mắt, tiếng nói, clipboard, trình duyệt và media; phần còn lại đến dưới dạng plugin được ký và chạy trong sandbox. Agent kết nối tới nó qua MCP, và mọi yêu cầu chúng đưa ra đều bị chặn lại và quyết định ngay trong lõi Rust trước khi bất cứ thứ gì chạy: capability, plugin lẫn lệnh shell, Quy tắc lệnh cho phép bạn viết lại những gì agent thực thi.

<p align="center">
  <img src="public/diagram.jpeg?raw=true" alt="Sơ đồ kiến trúc OpenCapX: các agent CLI (Claude Code, Codex, OpenCode, Gemini CLI, Cursor, Copilot CLI, Factory Droid, Oh My Pi) kết nối qua cổng MCP và hook PreToolUse; lõi Rust định tuyến capability tới provider tích hợp và plugin đã ký" width="100%" />
</p>

## Tính năng

**Pet và UI**
- **Pet 2D và 3D**: sprite sheet động hoặc model glTF/VRM, với các trạng thái đang làm việc / đang chờ bạn / đã xong / idle.
- **Bong bóng trạng thái**: một bong bóng theo chủ đề hiển thị agent đang làm gì, và biến thành các nút lựa chọn khi agent hỏi bạn điều gì đó.
- **Menu khay hệ thống**: một chấm trạng thái, nhóm phiên theo từng dự án, và tooltip ghi rõ tên agent đang cần bạn.

**Bề mặt agent**
- **Cổng MCP**: sáu công cụ (`say`, `notify`, `set_state`, `ask`, `list_capabilities`, `execute`) cùng các đăng ký sự kiện, để bất kỳ MCP host nào cũng có thể điều khiển desktop.
- **Kết nối bằng một lệnh**: `opencapx connect claude|codex|opencode|omp` nối các hook và mục MCP của agent, theo cách idempotent.
- **Thông báo**: toast của hệ điều hành khi agent hoàn tất hoặc chờ nhập liệu, được tập hợp trong Trung tâm thông báo.

**Plugin**
- **Tiến trình tách biệt**: plugin là những tiến trình bình thường giao tiếp bằng JSON-RPC 2.0 qua stdio, không bao giờ là plugin của Tauri.
- **Được ký và xét duyệt**: Ed25519 để phân phối, HMAC cho dùng nội bộ/nhóm; các domain capability của bên thứ ba phải được khai báo và xét duyệt trước khi được tin cậy.
- **Cài đặt khai báo**: khối `settings[]` trong manifest hiển thị một biểu mẫu đồ họa, với các secret được lưu trong OS keychain.

**Quản trị**
- **Quyền hai lớp**: mọi thao tác nguy hiểm đều đi qua Permission Manager của Rust trước khi bất kỳ plugin nào thấy nó.
- **Quy tắc lệnh**: hook `PreToolUse` chặn các lệnh shell của agent và viết lại chúng thành một executor do bạn chọn.
- **Kiểm toán và kiểm soát**: các lần cấp quyền, từ chối và lần khớp quy tắc đều được ghi vào Activity Timeline, và một kill switch dừng mọi plugin cùng lúc.

**Capability**
- **Provider tích hợp sẵn**: thị giác, giọng nói, clipboard, đọc trình duyệt, media, và macOS PIM (ảnh, danh bạ, lịch, vị trí, ghi chú, nhắc nhở, mail).

**Tự động hóa và cảnh báo**
- **Event → Rule → Action**: biến hoạt động của agent thành các hành động trên desktop bằng các file quy tắc.
- **Webhook**: Slack, Discord hoặc endpoint tùy chỉnh, với định tuyến theo mức độ nghiêm trọng, khử trùng lặp, thử lại và hàng đợi dead-letter.

**Vận hành**
- **Hồ sơ workspace**: giữ các bộ plugin và quyền riêng biệt và chuyển đổi giữa chúng.
- **Sao lưu và khôi phục**: chụp nhanh trạng thái workspace vào một file và khôi phục chỉ bằng một cú nhấp.
- **Phím tắt và command palette**: phím tắt toàn cục cùng một palette có thể tìm kiếm.

**Phân phối**
- **Marketplace và registry**: cài đặt từ một index đã ký, với việc thu hồi nhà phát hành và các kênh stable / beta / dev.

**Nền tảng**
- **Đa nền tảng**: các bản dựng macOS, Windows và Linux.
- **Ba ngôn ngữ giao diện**: tiếng Anh, tiếng Trung giản thể và tiếng Việt.

## Ba trụ cột

### Hệ thống Plugin

Plugin là những tiến trình bình thường, không phải plugin của Tauri. Mỗi plugin giao tiếp bằng JSON-RPC 2.0 qua stdio, khai báo các capability và quyền mà nó cần trong `opencapx-plugin.json`, và luôn tách biệt khỏi lõi. Các gói đều được ký (Ed25519 để phân phối, HMAC cho dùng nội bộ/nhóm), và các domain capability của bên thứ ba phải được khai báo và xét duyệt trước khi được tin cậy.

- Hướng dẫn viết plugin: [docs/plugin-authoring.md](docs/plugin-authoring.md)
- Giao thức: [docs/plugin-protocol.md](docs/plugin-protocol.md)
- Đặc tả manifest: [docs/plugin-manifest.md](docs/plugin-manifest.md)
- Ký và phân phối: [docs/plugin-signing.md](docs/plugin-signing.md)
- Domain quyền: [docs/permission-domains.md](docs/permission-domains.md)

### Cổng MCP

Các agent khởi chạy `opencapx mcp` như một MCP server dùng stdio. Tiến trình đó chuyển tiếp mọi lệnh gọi công cụ tới lõi cục bộ qua HTTP. Bề mặt v1 gồm sáu công cụ: `opencapx.say`, `opencapx.notify`, `opencapx.set_state`, `opencapx.ask`, `opencapx.list_capabilities`, và `opencapx.execute`. Khả năng mới không thêm công cụ: `opencapx.execute` với tới mọi capability đã đăng ký thông qua router. `opencapx.subscribe` / `opencapx.unsubscribe` bao phủ các capability dạng luồng sự kiện.

- Đặc tả: [docs/mcp.md](docs/mcp.md)

### Hệ thống phân quyền

Mọi thao tác nguy hiểm đều đi qua Permission Manager viết bằng Rust trước khi bất kỳ plugin nào nhìn thấy nó. WebView không bao giờ là ranh giới bảo mật; lõi mới là. Các agent tự nhận diện thông qua luồng đăng ký TOFU, plugin khai báo quyền ngay từ đầu, và hai lớp được kiểm tra riêng biệt. Các lần từ chối và cấp quyền đều được ghi lại trong audit trail.

- Mô hình và phạm vi: [docs/permissions.md](docs/permissions.md)

## Quy tắc lệnh

OpenCapX cũng đứng trước chính các lệnh shell của agent. Một hook `PreToolUse` chặn mỗi lệnh gọi công cụ Bash trước khi nó chạy và có thể **viết lại** nó thành dạng của một executor mà bạn chọn: `curl https://x` trở thành `sandbox curl https://x`. Claude Code và Codex hiện được viết lại; các host khác đi qua nguyên trạng.

**OpenCapX chỉ định tuyến; nó không phân xử.** Lệnh được gửi tới executor mà bạn đã cấu hình, và ranh giới (sandbox, proxy, container) thuộc trách nhiệm của executor đó. Đây là một pipeline tách biệt với Hệ thống phân quyền ở trên: các quyền kiểm soát những lệnh gọi capability qua `/rpc`, còn quy tắc lệnh điều chỉnh những lệnh mà chính agent đưa ra.

Các quy tắc được xếp lớp, gồm built-in (mặc định rỗng), global (`~/.opencapx/rules.json`), và cấp dự án (`<project>/.opencapx/rules.json`). Quy tắc cấp dự án là một bề mặt để chèn lệnh, nên chúng vẫn **bị bỏ qua cho tới khi bạn tin cậy dự án một cách rõ ràng** (`opencapx rules trust`). Một lần viết lại phát ra sự kiện audit `rule.applied` vào Activity Timeline, và một tệp quy tắc bị thiếu hoặc hỏng sẽ fail-open: lệnh chạy nguyên trạng và agent không bao giờ bị chặn.

- Đặc tả: [docs/rules.md](docs/rules.md)
- CLI: `opencapx rewrite`, cùng với `opencapx rules list | explain | trust | untrust`
- Trang Cài đặt: tab **Command Rules** cho phép thêm quy tắc, bật/tắt các quy tắc global, và quản lý các dự án đã tin cậy.

## Dành cho người dùng

### Cài đặt

Tải bản dựng mới nhất từ [trang releases](https://github.com/opencapx/OpenCapX/releases/latest): `.dmg` cho macOS (Apple Silicon), `x64-setup.exe` hoặc `.msi` cho Windows, và `.deb` / `.rpm` / `.AppImage` cho Linux. Các bản dựng macOS chưa được ký code hay notarize; nếu Gatekeeper chặn lần khởi chạy đầu tiên, hãy nhấp chuột phải vào ứng dụng và chọn Open.

Cách build từ mã nguồn, các yêu cầu tiên quyết, và cửa sổ phát triển trực tiếp nằm trong [INSTALL.md](INSTALL.md).

### Kết nối agent của bạn

Một lệnh duy nhất thiết lập hooks và mục MCP server của agent (idempotent, không có thông tin xác thực nào lọt vào cấu hình của agent; luồng token được xử lý giữa `opencapx mcp` và lõi lúc khởi động). Nếu `opencapx` chưa có trên `PATH`, hãy cài trước từ **Settings → General → Command line** ([INSTALL.md](INSTALL.md#the-opencapx-command)):

```bash
opencapx connect claude   # or: codex | opencode | omp
```

Khởi động lại agent, sau đó yêu cầu nó gọi `opencapx.list_capabilities` để kiểm tra. Các dạng cấu hình thủ công cho từng host nằm trong [docs/mcp.md](docs/mcp.md).

### Plugin đầu tiên của bạn trong năm phút

Tạo bộ khung plugin TypeScript từ initializer đã phát hành:

```bash
npm create opencapx-plugin -- --id com.acme.hello --name "Hello"
```

Rồi build và test nó:

```bash
cd hello && npm install && npm test && npm run build
```

Kết quả là một plugin có manifest, một capability `image.analyze`, và một bài test. Sửa `src/plugin.ts` để thay đổi hành vi của nó, rồi cài thư mục đó từ cửa sổ Settings. Hướng dẫn đầy đủ, bao gồm cả hướng Python, nằm trong [docs/plugin-authoring.md](docs/plugin-authoring.md).

## Dành cho nhà phát triển

### Kiến trúc

Các agent kết nối tới lõi qua hooks và MCP. Lõi sở hữu event bus, capability registry cùng router, permission manager, và vòng đời plugin; plugin chạy như các tiến trình con tách biệt còn WebView chỉ đảm nhiệm phần hiển thị.

Sơ đồ, các lớp runtime, luồng dữ liệu của một lần gọi capability, và bản đồ module đầy đủ nằm trong [ARCHITECTURE.md](ARCHITECTURE.md). Tài liệu giao thức bắt đầu tại [docs/README.md](docs/README.md).

### Phát triển

Môi trường phát triển, cổng kiểm tra trước khi commit, và quy ước commit nằm trong [CONTRIBUTING.md](CONTRIBUTING.md).

## Công nghệ sử dụng và Ghi công

- [Tauri 2](https://tauri.app/): vỏ desktop (biểu tượng khay hệ thống, API riêng của macOS, hỗ trợ ảnh PNG).
- [Rust](https://www.rust-lang.org/): lõi Core, bao gồm event bus, capability registry/router, permission manager, plugin runtime, và process manager.
- [TypeScript](https://www.typescriptlang.org/) + [Vite](https://vitejs.dev/): giao diện WebView.
- [Three.js](https://threejs.org/) + [@pixiv/three-vrm](https://github.com/pixiv/three-vrm): hiển thị pet 3D (glTF/VRM).
- [rusqlite](https://github.com/rusqlite/rusqlite): lưu trữ SQLite cục bộ.
- [tiny_http](https://github.com/tiny-http/tiny-http): cổng HTTP cục bộ cho các sự kiện hook và chuyển tiếp MCP.
- [ed25519-dalek](https://github.com/dalek-cryptography/ed25519-dalek) / [hmac](https://github.com/RustCrypto/MACs) / [sha2](https://github.com/RustCrypto/hashes): ký và xác minh gói plugin.
- [keyring](https://github.com/hwchen/keyring-rs): keychain của hệ điều hành cho bí mật của plugin.
- [DOMPurify](https://github.com/cure53/DOMPurify) + [marked](https://github.com/markedjs/marked): hiển thị markdown an toàn trong UI.
- [serde](https://serde.rs/) / serde_json / serde_yaml: tuần tự hóa.

Mọi hình thức PR đều được hoan nghênh (tài liệu, UI, code).

## Lộ trình

[ROADMAP.md](ROADMAP.md).

## Giấy phép

Apache-2.0. Xem [LICENSE](LICENSE).

## Bảo mật

Không mở issue công khai cho một lỗ hổng bảo mật. Kênh báo cáo, các phiên bản được hỗ trợ, và tài liệu tham chiếu về key-ceremony nằm trong [SECURITY.md](SECURITY.md).

## Thêm

- [INSTALL.md](INSTALL.md) để biết hướng dẫn tải về và build từ mã nguồn
- [ARCHITECTURE.md](ARCHITECTURE.md) để biết các lớp runtime, luồng dữ liệu, và bản đồ module
- [CONTRIBUTING.md](CONTRIBUTING.md) để biết cách làm việc trong repo này
- [ROADMAP.md](ROADMAP.md) để biết những gì sắp tới
- [CHANGELOG.md](CHANGELOG.md) để biết những gì đã thay đổi
- [docs/](docs/README.md) để có bộ đặc tả đầy đủ

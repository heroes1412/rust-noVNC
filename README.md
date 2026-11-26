# novnc-rs — thay thế `novnc_proxy` + `websockify` bằng một binary Rust

Web server tĩnh + proxy WebSocket→TCP cho [noVNC](https://github.com/novnc/noVNC),
viết bằng Rust. Mục tiêu: **không cần Python, không cần cài thêm gì**, chạy trên
Windows / macOS / Linux với một file thực thi duy nhất.

```
┌─────────────┐   HTTP/WS    ┌──────────────┐    TCP thô    ┌────────────┐
│  Browser    │ ───────────► │   novnc-rs   │ ────────────► │ VNC server │
│ (noVNC JS)  │ ◄─────────── │ (Rust, 1 exe)│ ◄──────────── │  :5900     │
└─────────────┘              └──────────────┘               └────────────┘
```

Client JS của noVNC được **nhúng thẳng vào binary lúc compile** (2.5 MB), nên chỉ
cần copy một file `.exe` là chạy. Vẫn có thể trỏ ra thư mục ngoài bằng `--web`.

## Vì sao chỉ cần thay proxy?

noVNC gốc gồm hai phần:

| Phần | Ngôn ngữ | Ghi chú |
|---|---|---|
| Client (`core/`, `app/`, `vnc.html`) | JavaScript | Chạy trong browser, không cần Python |
| `utils/novnc_proxy` + `websockify` | Bash + **Python** | Static server + cầu nối WebSocket↔TCP |

Trình duyệt không mở được TCP thô tới cổng 5900, nên luôn cần một cầu nối.
Dự án này thay đúng phần Python đó. Kết quả: client gốc chạy **không đổi một dòng
nào**, toàn bộ tính năng (encoding Tight/ZRLE/JPEG/H.264, clipboard, auth, âm
thanh, i18n…) giữ nguyên.

## Build

Cách nhanh nhất trên Windows — một lệnh build cả 4 nền tảng:

```bat
build.bat
```

Kết quả trong `dist\`:

| File | Nền tảng | Ghi chú |
|---|---|---|
| `novnc-<ver>-windows-amd64.exe` | Windows x64 | MSVC, chạy trực tiếp |
| `novnc-<ver>-windows-arm64.exe` | Windows ARM64 | gnullvm qua zig, chạy trên máy Snapdragon |
| `novnc-<ver>-linux-amd64` | Linux x64 | musl static, chạy trên mọi distro |
| `novnc-<ver>-linux-arm64` | Linux ARM64 | musl static, cho server ARM / Raspberry Pi |

Kèm `dist\SHA256SUMS.txt`. Script tự chạy `cargo test` trước khi build.

Tuỳ chọn:

```bat
build.bat --only win64              :: chỉ build Windows x64
build.bat --only "win64,linux64"    :: nhiều target, nhớ để trong ngoặc kép
build.bat --only all
build.bat --skip-tests              :: bỏ qua cargo test
build.bat --update-client           :: git clone / git pull client noVNC
build.bat --help
```

`build.bat` tự cài những gì còn thiếu:

- Rust target qua `rustup target add` (idempotent).
- **zig 0.15.2** tải vào `.tools\zig-x86_64-windows-0.15.2\` (có kiểm tra SHA-256).
- **cargo-zigbuild 0.24.1** qua `cargo install` nếu chưa có.

Không cần Python, WSL, Docker, CMake, nasm, hay Visual Studio ARM64
components. Lần build đầu cần Internet; các lần sau dùng lại cache.

Dọn dẹp:

```bat
clean.bat          :: xoá target\ + .tools\ (cache + toolchain zig), GIỮ dist\ và source
clean.bat target   :: chỉ xoá target\, giữ .tools\ (build lại không cần tải zig)
clean.bat dist     :: xoá riêng dist\ (các binary release)
clean.bat all      :: xoá target\, .tools\, dist\ và novnc-client\
```

`.tools\` chiếm ~420 MB (18k file) sau lần build cross đầu tiên; xoá nó là
đúng, `build.bat` sẽ tự tải lại zig khi cần. `dist\` chỉ ~22 MB và chứa binary
release nên mặc định **không** bị xoá.

Build thủ công trên Linux/macOS (không dùng batch):

```bash
cargo build --release
# target/release/novnc  (Windows: novnc.exe)
```

Yêu cầu: Rust 1.88+ (edition 2024). Linux static:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
# cần zig + cargo-zigbuild nếu cross-compile từ Windows
cargo zigbuild --release --target x86_64-unknown-linux-musl
```

## Cập nhật client noVNC rồi build lại

Client JS được **nhúng lúc compile** từ một thư mục checkout. Thứ tự tìm
(build-time):

1. Biến môi trường `NOVNC_CLIENT_DIR`
2. `.\novnc-client` — nếu tồn tại, được ưu tiên hơn bản bundled
3. `.\original-novnc` — snapshot 1.7.0 kèm repo, dùng khi không có gì khác

`build.rs` copy `app/`, `core/`, `vendor/`, `vnc.html`, `vnc_lite.html`,
`defaults.json`, `mandatory.json`, `package.json` vào `$OUT_DIR/web`, sau đó
`include_dir!` nhúng toàn bộ vào binary. Nhờ vậy `docs/`, `tests/`, `.git/`
không bị mang vào và binary vẫn ~6 MB.

### Trường hợp thường gặp: muốn dùng bản noVNC mới

```bat
cd C:\Users\agy\Desktop\noVNC-rust
build.bat --update-client
```

Lệnh này:

- Nếu chưa có `novnc-client\`: `git clone --depth 1 https://github.com/novnc/noVNC.git novnc-client`
- Nếu đã có: `git fetch --tags` + `git pull --ff-only`

Sau đó script tự build lại và in ra dòng xác nhận, ví dụ:

```text
client: C:\Users\agy\Desktop\noVNC-rust\novnc-client  (noVNC 1.7.0)
```

Khi chạy binary, log cũng ghi `web root: embedded (noVNC 1.7.0)`.

### Clone tay (hoặc ghim phiên bản)

`build.bat --update-client` luôn lấy nhánh mặc định. Muốn ghim tag:

```bat
git clone --depth 1 https://github.com/novnc/noVNC.git novnc-client
git -C novnc-client fetch --depth 1 origin tag v1.7.0
git -C novnc-client checkout v1.7.0
build.bat
```

Hoặc đặt checkout ở bất kỳ đâu rồi trỏ biến môi trường:

```bat
set NOVNC_CLIENT_DIR=D:\src\noVNC-1.8.0
build.bat
```

Lưu ý: `NOVNC_CLIENT_DIR` là biến **lúc build** (quyết định client nào được
nhúng). Biến `NOVNC_WEB` và tham số `--web` là **lúc chạy** (phục vụ thư mục
ngoài, không nhúng).

### Có cần git clone không?

Không bắt buộc. Repo đã có sẵn `original-novnc/` (noVNC 1.7.0) nên `build.bat`
chạy được ngay, kể cả offline. Chỉ cần clone khi muốn lên bản mới hơn. Bạn cũng
có thể tải file zip release của noVNC, giải nén và trỏ `NOVNC_CLIENT_DIR` —
miễn trong thư mục có `vnc.html`.

### Khi nào phải build lại?

- Sau khi cập nhật client (để nhúng bản mới).
- Sau khi sửa mã Rust.
- Không cần build lại nếu chỉ muốn thử client khác: dùng
  `novnc --web D:\noVNC --vnc localhost:5900` (phục vụ trực tiếp từ đĩa, sửa
  file rồi F5 là thấy ngay). Cách này tiện để phát triển, nhưng mất tính
  "một file duy nhất".

Cargo tự phát hiện client thay đổi qua `cargo:rerun-if-changed` trong
`build.rs`, nên chỉ cần chạy lại `build.bat`. Nếu nghi ngờ bản nhúng bị cũ:

```bat
cargo clean -p novnc
build.bat
```

### Nếu noVNC đổi cấu trúc thư mục

`build.rs` chỉ copy các mục trong hằng `CLIENT_ENTRIES` (đầu file `build.rs`).
Nếu bản mới thêm file gốc mà client gọi lúc chạy (ví dụ một file cấu hình mới
ở thư mục gốc), thêm tên file vào danh sách đó rồi build lại. Thiếu file,
`build.rs` báo lỗi rõ ràng ngay khi compile chứ không âm thầm tạo binary hỏng.

### Kiểm tra nhanh sau khi build

```bat
:: terminal 1 - VNC server giả
cargo run --example fake_vnc -- 127.0.0.1:5900
:: terminal 2 - proxy + web server
dist\novnc-<ver>-windows-amd64.exe --vnc 127.0.0.1:5900
:: mở http://127.0.0.1:6080/vnc.html?autoconnect=true
```


## Dùng

```bash
./novnc --vnc localhost:5900
# → http://<hostname>:6080/vnc.html
```

Tương đương `./utils/novnc_proxy --vnc localhost:5900`, nhưng không tải
websockify và không cần Python.

### Tham số

| Tham số | Mặc định | Ý nghĩa |
|---|---|---|
| `--listen [HOST:]PORT` | `0.0.0.0:6080` | Địa chỉ lắng nghe; nhận `6080`, `127.0.0.1:6080`, `[::1]:6080` |
| `--vnc HOST:PORT` | `localhost:5900` | VNC server đích |
| `--web DIR` | nhúng sẵn | Thư mục chứa `vnc.html` (dùng khi muốn sửa client trực tiếp) |
| `--cert FILE` | – | PEM cert; có thể gộp cả private key (`self.pem`) |
| `--key FILE` | – | Private key riêng, khi không gộp vào `--cert` |
| `--ssl-only` | bật khi có `--cert`? không | Từ chối kết nối HTTP thường |
| `--file-only` | – | Tắt directory listing |
| `--record FILE` | – | Ghi phiên ra `FILE.session.js` (xem bên dưới) |
| `--heartbeat SEC` | – | Gửi WebSocket ping mỗi SEC giây |
| `--timeout SEC` | – | Thoát khi không có kết nối nào trong SEC giây |
| `--idle-timeout SEC` | – | Đóng từng kết nối không có traffic trong SEC giây |
| `--auth USER:PASS` | – | Bắt buộc HTTP Basic auth (cả trang lẫn WebSocket); `--web-auth` là tên cũ |
| `--auth-source USER:PASS` | – | Tương thích websockify; dùng cùng `--auth` (không kèm giá trị) |
| `--syslog SERVER` | – | Nhận cho tương thích, chưa hiện thực (có cảnh báo) |

Ví dụ:

```bash
# Chỉ nghe localhost, VNC ở máy khác
novnc --listen 127.0.0.1:6080 --vnc 192.168.1.10:5900

# TLS + chỉ HTTPS, dùng self.pem gộp cert+key
openssl req -new -x509 -nodes -out self.pem -keyout self.pem -days 365
novnc --cert self.pem --ssl-only --vnc localhost:5900

# Có mật khẩu, ghi lại phiên
novnc --auth admin:secret --record demo --vnc localhost:5900

# Dùng client đang sửa trong repo (không cần rebuild)
novnc --web ./original-novnc --vnc localhost:5900
```

Một cổng phục vụ **cả HTTP và HTTPS**: server xem byte đầu tiên của kết nối
(`0x16` = TLS handshake), giống cách websockify làm. Vì vậy URL `http://` và
`https://` cùng hoạt động khi có `--cert`.

### Thử không cần VNC server thật

Repo có kèm một RFB server giả (handshake + raw framebuffer 640×480):

```bash
cargo run --example fake_vnc -- 127.0.0.1:5900   # terminal 1
cargo run -- --vnc 127.0.0.1:5900                # terminal 2
# mở http://127.0.0.1:6080/vnc.html?autoconnect=true
```

## Ghi và phát lại phiên

`--record demo` tạo `demo.session.js` đúng định dạng mà
`original-novnc/tests/vnc_playback.html` đọc được:

```js
var VNC_frame_encoding = 'base64';
var VNC_frame_data = [
'{12{UkZC...',   // '{' server → client, timestamp ms
'}40{AAAA...',   // '}' client → server
'EOF'
];
```

Frame được base64 để tránh vấn đề escape byte nhị phân. Nhiều phiên đồng thời sẽ
tạo `demo.session.2.js`, `demo.session.3.js`…

Phát lại: mở `original-novnc/tests/vnc_playback.html?data=demo.session.js` qua
cùng server `novnc` (trỏ `--web original-novnc`) và bấm Start.

## Kiến trúc

```
src/
  main.rs    # CLI entry (clap + env_logger)
  config.rs  # tham số, parse địa chỉ, HTTP Basic credentials
  server.rs  # accept loop, sniff TLS/HTTP, hyper dispatch, watchdog --timeout
  ws.rs      # bắt tay WebSocket, 2 chiều WS↔TCP, heartbeat, idle, record
  web.rs     # static files: nhúng sẵn hoặc thư mục, MIME, listing, chống traversal
  record.rs  # định dạng .session.js
  tls.rs     # rustls + PEM (cert gộp hoặc tách)
build.rs     # chọn + copy client noVNC vào OUT_DIR để nhúng
build.bat    # build 4 target (Win/Linux, x64/ARM64) + tests, xuất dist\
clean.bat    # dọn target\ (mặc định); dist\/.tools\/novnc-client\ theo tham số
examples/fake_vnc.rs  # RFB server giả để test/demo
tests/proxy.rs        # 14 integration test end-to-end
```

Điểm đáng chú ý:

- **Không dùng `axum`/`tower`**: hyper 1 + hyper-util trực tiếp, ít dependency,
  kiểm soát hoàn toàn bắt tay WebSocket (echo sub-protocol `binary`).
- **Giới hạn frame nâng lên 256 MiB**: framebuffer 4K raw ~33 MB, vượt mức mặc
  định 16 MiB của tungstenite.
- **TCP tới VNC được mở trước khi trả 101**, nên VNC server chết → trình duyệt
  nhận HTTP 502 kèm thông báo, thay vì WebSocket nửa mở.
- **Chống path traversal** bằng cách từ chối `..`, `\`, `:`, NUL và control char
  thay vì chuẩn hoá.
- So sánh mật khẩu Basic auth dạng constant-time.

## Test

```bash
cargo test      # 10 unit + 14 integration
cargo clippy --all-targets
```

Integration test dựng server thật trong process, VNC server giả, client
WebSocket/HTTP thật, kiểm tra: static/embedded client, 404 & traversal, HEAD,
directory listing, proxy 2 chiều, sub-protocol, 502, Basic auth, record, TLS
(cert tự sinh bằng `rcgen`), `--timeout`, heartbeat, idle close.

Ngoài ra đã kiểm tra thủ công bằng Chrome thật: client gốc kết nối qua proxy tới
`fake_vnc`, hiển thị đúng 640×480 và đúng thứ tự màu RGBA.

## Khác biệt so với `websockify`

- `--token-plugin`, `--unix-target`, `--wrap-mode`, `--cgi`, `--daemon`,
  `--run-once`, `--extra-headers`: chưa có (đa số rất ít dùng).
- `--idle-timeout` ở đây đóng **từng kết nối** khi không có traffic; `--timeout`
  mới là thoát **process** khi không còn kết nối.
- `--syslog` chỉ nhận và cảnh báo.
- Ưu điểm: một binary, không phụ thuộc runtime, log qua `RUST_LOG`.

## Bảo mật

Giống websockify, server **không kiểm tra `Origin`** và mặc định nghe mọi
interface. Nếu đưa ra mạng ngoài, hãy bật `--auth USER:PASS`, hoặc dùng TLS, hoặc chỉ
`--listen 127.0.0.1:6080` rồi đặt sau reverse proxy có xác thực. Bản thân giao
thức VNC cũng thường không mã hoá — TLS ở đây chỉ bảo vệ đoạn browser↔proxy.

## Giấy phép

Phần client nhúng trong binary là của dự án noVNC, **MPL-2.0** (xem
`original-novnc/LICENSE.txt`). Mã Rust trong repo này cũng để MPL-2.0 cho nhất
quán và để phân phối binary kèm client hợp lệ. Nếu bạn muốn đổi phần Rust sang
MIT/Apache, hãy tách client ra khỏi binary và phân phối client theo MPL-2.0.

## Bước tiếp theo (nếu muốn port cả client)

Proxy này giải quyết xong vấn đề "không cần Python". Nếu sau này muốn một VNC
client native/WASM thuần Rust: phần RFB protocol + decoder (`core/`, ~9.6k LOC JS)
là phần port được; phần browser glue (`app/`) là viết lại. Giao thức đã có spec
RFC 6143 và bộ test/playback trong `original-novnc/tests/` làm golden test.

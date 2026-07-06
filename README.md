# USB 摄像头实时视频流服务 🎥

Rust + axum + nokhwa，把 USB 摄像头的实时画面推到浏览器。

## 架构

本项目由**两个独立进程**组成,各自职责清晰:

```
┌──────────────────────┐   WebSocket(/ws)    ┌──────────────────────┐
│  cam-stream(Rust)    │ ──── JPEG 帧 ────▶  │  浏览器(demo.html)   │
│  视频流后端            │                     │  Canvas 渲染          │
│  · 抓帧 + 编码 JPEG    │ ◀── /api/health ── │                       │
│  · WebSocket 推流      │                     │                       │
│  · 摄像头掉线自动重连   │                     │                       │
└──────────────────────┘                     └──────────────────────┘
            ▲                                            ▲
            │ 同机默认 localhost:3000                     │
            │                                            │
┌──────────────────────┘                            ┌────┴─────────────────┐
│  serve.py(Python)  ─── 托管 demo.html 静态页 ───▶  打开浏览器
│  · 简单 http.server                               http://localhost:8000
│  · 后台轮询 cam-stream 健康状态并打印
└──────────────────────┘
```

- **cam-stream** (`src/`):视频流后端。连摄像头、编码 JPEG、通过 WebSocket 推给所有订阅的浏览器,**不托管网页文件**。
- **serve.py** (`static/`):轻量网页托管。只把 `static/demo.html` 发给浏览器,并在控制台轮询后端健康状态。可以换成任意静态服务器(nginx、`python -m http.server`、Vite 等)。

## 功能

- 🚀 **零前端依赖**:HTML + `<canvas>` 就能看,不需要 npm / webpack
- 🔌 **WebSocket 模式**:浏览器实时接收 JPEG 帧 + 渲染
- 📡 **多客户端**:`tokio::broadcast` 通道,多个浏览器同时观看不互相拖累
- 🔁 **双向自动重连**:后端检测摄像头掉线会重连;前端检测后端离线也会重连
- ⚙️ **环境变量配置**:分辨率、帧率、设备 index、监听地址都能改
- 📝 **双路日志**:控制台实时滚动 + exe 同级 `logs/` 目录按天滚动

## 使用说明

### 方式一:从源码运行(开发时常用)

需要 Rust 工具链(`rustup` 安装 stable)和 Python 3。

```bash
# 1. 插上 USB 摄像头
# 2. 编译并启动视频流后端(默认监听 0.0.0.0:3000)
cargo run --release

# 3. 另开一个终端,启动网页托管(默认监听 0.0.0.0:8000)
python serve.py
#    或双击 serve.bat

# 4. 浏览器自动打开,或手动访问 http://localhost:8000/demo.html
```

Windows 下也可以直接双击 `run-cam.bat`(`cargo build --release` 后启动 `target\release\cam-stream.exe`)和 `serve.bat`。

### 方式二:运行已编译产物

```bash
# 1. 先编译出 release exe(只需做一次,改了源码再重新编译)
cargo build --release
#    产物在 target/release/cam-stream.exe

# 2. 双击 run-cam.bat 启动后端
# 3. 双击 serve.bat 启动网页托管
# 4. 浏览器访问 http://localhost:8000/demo.html
```

> `run-cam.bat` 会切到项目根目录运行 exe(因为 exe 用相对路径定位 `logs/`)。

### 页面操作

打开 `demo.html` 后:
- **▶ 开始**:连接 WebSocket 拉流
- **■ 停止**:断开连接并清空画面
- 状态栏实时显示连接状态(连接中 / 已连接 / 离线重连)

如果后端没启动,页面会自动重连(前 5 次每 3 秒,之后每 5 秒),不用手动刷新。

### 配置(环境变量)

视频流后端 `cam-stream` 读以下变量:

| 变量         | 默认值       | 说明                              |
| ---------- | --------- | ------------------------------- |
| `CAM_INDEX`   | `0`         | 摄像头设备 index(0、1、2...),`/api/cameras` 可列出 |
| `CAM_WIDTH`   | `640`       | 请求宽度(摄像头可能选最接近的支持值)            |
| `CAM_HEIGHT`  | `480`       | 请求高度                            |
| `CAM_FPS`     | `30`        | 请求帧率                            |
| `BIND_ADDR`   | `0.0.0.0:3000` | 监听地址                          |
| `RUST_LOG`    | `info`      | 日志级别(`debug` / `info` / `warn`) |

> 注:`CAM_WIDTH/HEIGHT/FPS` 是「请求值」。nokhwa 会用 `RequestedFormatType::None` 让 Media Foundation 自选摄像头支持的格式(MJPG/NV12),实际生效分辨率以日志打印的为准。详见 `src/camera.rs` 注释。

示例:

```powershell
# Windows PowerShell:用第 2 个摄像头,720p
$env:CAM_INDEX=1; $env:CAM_WIDTH=1280; $env:CAM_HEIGHT=720; $env:CAM_FPS=60
cargo run --release
```

```bash
# Git Bash / Linux
CAM_INDEX=1 CAM_WIDTH=1280 CAM_HEIGHT=720 cargo run --release
```

```cmd
:: Windows CMD
set CAM_INDEX=1 && set CAM_WIDTH=1280 && set CAM_HEIGHT=720
cargo run --release
```

### HTTP 端点(cam-stream)

| 路径                | 说明                                              |
| ----------------- | ------------------------------------------------ |
| `GET /ws`         | WebSocket:`binary` = JPEG 帧,`text` = JSON `{type:"stats",fps,seq}` |
| `GET /api/cameras` | 列出所有可用摄像头                                        |
| `GET /api/health` | 健康检查,返回 `{status:"ok", camera:"streaming\|connecting\|disconnected"}` |

### 局域网观看

服务默认监听 `0.0.0.0:3000`,防火墙放行后,其他设备把 demo.html 里的 `WS_BACKEND` 改成 `ws://<本机IP>:3000` 即可。

## 开发说明

### 目录结构

```
cam_serve/
├── Cargo.toml            Rust 依赖与 release profile
├── Cargo.lock            依赖锁(已纳入版本库,保证构建一致)
├── src/
│   ├── main.rs           axum 路由、WebSocket handler、摄像头管理循环、日志初始化
│   └── camera.rs         nokhwa 摄像头封装:CallbackCamera 抓帧 → RGB → JPEG
├── static/
│   └── demo.html         前端页面(WebSocket 客户端 + Canvas 渲染)
├── serve.py              Python 静态托管 demo.html + 后端健康轮询
├── run-cam.bat           双击启动 cam-stream.exe(后端)
├── serve.bat             双击启动 serve.py(网页托管)
├── handle_tool/          Sysinternals Handle.exe(调试摄像头被占用时查句柄,带 EULA)
└── logs/                 运行时生成,按天滚动(cam-stream.logYYYYMMDD)
```

### 技术栈 & 关键设计

- **axum 0.7** — Web 框架(tokio 生态),只暴露 API + WebSocket,不托管网页
- **nokhwa 0.10** — 跨平台摄像头捕获(Windows 上走 Media Foundation,系统自带无需额外运行时)。用 `CallbackCamera` 在内部线程抓帧,回调里编码
- **image 0.25** — `decode_image::<RgbFormat>()` 把 MJPG/NV12 等转成 RGB,再用 `JpegEncoder` 编码成 JPEG(quality=80)
- **tokio::broadcast** — 多客户端扇出。容量 8 帧,慢客户端会被 `Lagged` 跳过而非阻塞采集线程
- **tracing + tracing-appender** — 双路日志:stderr 实时滚动 + 文件按天滚动,落在 exe 同级 `logs/`

**摄像头管理循环**(`camera_manager_loop`):连接 → 推流 → 监控掉线(5 秒无新帧判掉线)→ 重连(3 秒间隔)→ 重新枚举设备(15 秒节流,避免刷屏)。整个循环在 `spawn_blocking` 线程里跑,不能用 `tokio::sleep`。

### 构建与运行

```bash
# Debug 构建(快,未优化)
cargo run

# Release 构建(慢,优化,实际使用)
cargo build --release
# 产物:target/release/cam-stream.exe
```

### 调试技巧

- **看摄像头为什么打不开**:后端用 `anyhow!("... {e:#?}")` 保留了 nokhwa 完整错误链。`RUST_LOG=debug` 或看 `logs/` 日志能看到 Media Foundation 协商失败的细节。
- **摄像头被占用**:用 `handle_tool\handle64.exe` 查哪个进程占着摄像头句柄:
  ```bash
  ./handle_tool/handle64.exe -accepteula -p <占用进程名>
  ```
- **枚举不到设备**:确认 Windows 隐私设置(设置 → 隐私和安全 → 摄像头 → 允许桌面应用访问摄像头)和 USB 连接。

### 添加新依赖

编辑 `Cargo.toml`,然后:

```bash
cargo build      # 拉取并编译新依赖,自动更新 Cargo.lock
```

### 常见问题

#### 1. 报 "无法打开摄像头 index=0"
- 摄像头没插好 / 被其他程序占用(浏览器、Zoom、OBS 都会占用)
- 换个 index 试试:`set CAM_INDEX=1`
- 检查 Windows 隐私设置:设置 → 隐私和安全 → 摄像头 → 允许桌面应用访问摄像头

#### 2. 画面卡 / 帧率低
- 把分辨率调小:`set CAM_WIDTH=320 CAM_HEIGHT=240`
- 检查 USB 是 2.0 还是 3.0 口
- 换 WebSocket 模式(默认就是),比 MJPEG 略快一点

#### 3. 切换页面/标签后画面黑了
- 浏览器会让不可见标签页的定时器/网络节流,切回标签页会自动恢复

#### 4. 想在局域网其他设备看
- 服务默认监听 `0.0.0.0:3000`,防火墙放行 3000 端口即可
- 访问 `http://<本机IP>:3000` 即可

## 技术栈

- **axum 0.7** —— Web 框架(tokio 生态)
- **nokhwa 0.10** —— 跨平台摄像头捕获(Windows 上用 Media Foundation)
- **image 0.25** —— RGB → JPEG 编码
- **tokio::broadcast** —— 多客户端扇出

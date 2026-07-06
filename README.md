# USB 摄像头实时视频流服务 🎥

Rust + axum + nokhwa，把 USB 摄像头的实时画面推到浏览器。

## 功能

- 🚀 **零前端依赖**：HTML + `<canvas>` 就能看
- 🔌 **WebSocket 模式**：浏览器实时接收 JPEG 帧 + 渲染
- 📡 **多客户端**：broadcast 通道，多个浏览器同时观看不互相拖累
- 🔁 **自动重连**：前端检测到后端离线会自动重连，后端起来即恢复画面
- ⚙️ **环境变量配置**：分辨率、帧率、设备 index 都能改

## 快速开始

```bash
# 1. 插上 USB 摄像头
# 2. 启动
cargo run --release

# 3. 浏览器打开
# http://localhost:3000
```

## 环境变量

| 变量         | 默认值       | 说明                              |
| ---------- | --------- | ------------------------------- |
| `CAM_INDEX`   | `0`         | 摄像头设备 index（0、1、2...）          |
| `CAM_WIDTH`   | `640`       | 请求宽度（摄像头可能选最接近的支持值）            |
| `CAM_HEIGHT`  | `480`       | 请求高度                            |
| `CAM_FPS`     | `30`        | 请求帧率                            |
| `BIND_ADDR`   | `0.0.0.0:3000` | 监听地址                          |
| `RUST_LOG`    | `info`      | 日志级别（`debug` / `info` / `warn`） |

示例：
```powershell
# Windows PowerShell
$env:CAM_INDEX=1; $env:CAM_WIDTH=1280; $env:CAM_HEIGHT=720; $env:CAM_FPS=60
cargo run --release
```

## 端点

| 路径                | 说明                                              |
| ----------------- | ----------------------------------------------- |
| `GET /ws`         | WebSocket：binary = JPEG 帧，text = JSON `{type:"stats"}` |
| `GET /api/cameras` | 列出所有可用摄像头                                       |
| `GET /api/health` | 健康检查                                            |

> 网页由 `serve.py` 另行托管，连接 `/ws` 端点显示画面。

## 技术栈

- **axum 0.7** —— Web 框架（tokio 生态）
- **nokhwa 0.10** —— 跨平台摄像头捕获（Windows 上用 Media Foundation）
- **image 0.25** —— RGB → JPEG 编码
- **tokio::broadcast** —— 多客户端扇出
## 常见问题

### 1. 报 "无法打开摄像头 index=0"
- 摄像头没插好 / 被其他程序占用（浏览器、Zoom、OBS 都会占用）
- 换个 index 试试：`set CAM_INDEX=1`
- 检查 Windows 隐私设置：设置 → 隐私和安全 → 摄像头 → 允许桌面应用访问摄像头

### 2. 画面卡 / 帧率低
- 把分辨率调小：`set CAM_WIDTH=320 CAM_HEIGHT=240`
- 检查 USB 是 2.0 还是 3.0 口
- 换 WebSocket 模式（`mode=ws`），比 MJPEG 略快一点

### 3. 切换页面/标签后画面黑了
- 浏览器会让不可见标签页的定时器/网络节流,切回标签页会自动恢复

### 4. 想在局域网其他设备看
- 服务默认监听 `0.0.0.0:3000`，防火墙放行 3000 端口即可
- 访问 `http://<本机IP>:3000` 即可
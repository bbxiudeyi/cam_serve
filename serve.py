"""
USB 摄像头 demo 网页托管服务
================================

作用:用 Python 托管 static/ 下的 demo.html 网页。
视频流由独立的 cam-stream.exe 提供(默认 http://localhost:3000)。

用法:
    双击本文件,或命令行:
        python serve.py

    自定义端口:
        python serve.py 9000

启动后会自动打开浏览器访问 demo.html。
关掉本窗口 = 停止服务。
"""

import functools
import http.server
import json
import socketserver
import sys
import threading
import time
import urllib.request
import webbrowser
from pathlib import Path

# 强制 stdout 行缓冲:双击运行(终端)和被重定向(serve.bat / 测试)时都能及时输出。
# 否则 Python 在非 tty 环境默认全缓冲,banner 日志会看不到。
try:
    sys.stdout.reconfigure(line_buffering=True)
    sys.stderr.reconfigure(line_buffering=True)
except AttributeError:
    pass  # 老版本 Python 没有 reconfigure,忽略(serve.bat 用 python -u 兜底)

# ---------- 配置 ----------
DEFAULT_PORT = 8000
# 视频流后端地址(cam-stream.exe)。demo.html 里 WS_BACKEND 也指向这里。
BACKEND_HOST = "localhost"
BACKEND_PORT = 3000
BACKEND_HEALTH = f"http://{BACKEND_HOST}:{BACKEND_PORT}/api/health"
SERVE_DIR = Path(__file__).parent / "static"   # 永远指向本脚本旁的 static/

# 后端状态轮询间隔(秒)。拉长一点避免控制台被刷屏,5 秒足够及时。
BACKEND_POLL_INTERVAL = 5


def check_backend() -> tuple[bool, str]:
    """探测视频流后端 cam-stream 是否在线,并读取它报告的摄像头状态。

    返回 (online, detail):
      online=True  → 后端进程在跑,detail 是摄像头状态描述
      online=False → 后端没响应,detail 是原因
    """
    try:
        with urllib.request.urlopen(BACKEND_HEALTH, timeout=2) as r:
            data = json.loads(r.read().decode("utf-8"))
            cam = data.get("camera", "unknown")
            # camera 状态:disconnected / connecting / streaming
            hint = {
                "disconnected": "未连上摄像头(后台会自动重试)",
                "connecting": "正在连接摄像头…",
                "streaming": "已连上摄像头,正在推流 ✓",
            }.get(cam, f"未知状态: {cam}")
            return True, hint
    except urllib.error.URLError:
        return False, "后端未启动(请先运行 cam-stream.exe / run-cam.bat)"
    except Exception as e:
        return False, f"探测后端出错: {e}"


def poll_backend_status():
    """后台线程:持续轮询后端状态,状态变化时打印一行。

    避免每 5 秒刷屏——只有 online/detail 发生变化时才输出。
    这样用户先开 serve.bat 再开 run-cam.bat 时,能看到后端"上线"的提示。
    """
    last_online = None   # None 表示还没探测过,用来强制打第一条
    last_detail = None
    while True:
        online, detail = check_backend()
        # 状态变化(或首次探测)才打印
        if online != last_online or detail != last_detail:
            if online:
                print(f"  后端连接: ✓ 在线 — {detail}")
            else:
                print(f"  后端连接: ✗ 离线 — {detail}")
            print("-" * 50)
            last_online = online
            last_detail = detail
        time.sleep(BACKEND_POLL_INTERVAL)


def main() -> int:
    # 1. 确认 static 目录存在
    if not SERVE_DIR.is_dir():
        print(f"❌ 找不到 static 目录: {SERVE_DIR}")
        print("   请确认 serve.py 和 static/ 在同一层级。")
        input("按 Enter 退出…")
        return 1

    # 2. 解析端口(命令行第一个参数)
    port = DEFAULT_PORT
    if len(sys.argv) > 1:
        try:
            port = int(sys.argv[1])
        except ValueError:
            print(f"⚠️  无效的端口参数: {sys.argv[1]!r},使用默认 {DEFAULT_PORT}")

    # 3. 确认 demo.html 存在(给出友好提示)
    demo_file = SERVE_DIR / "demo.html"
    if not demo_file.is_file():
        print(f"⚠️  static/ 下没有 demo.html,网页可能打不开。")
        print(f"   期望路径: {demo_file}")

    # 4. 自定义 handler:托管 static 目录 + 静音 HTTP 访问日志
    #    (访问日志会干扰 banner 输出,且对用户无用)
    class QuietHandler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args):
            pass  # 不打印 "GET /demo.html HTTP/1.1" 之类的访问日志

    handler = functools.partial(QuietHandler, directory=str(SERVE_DIR))

    # 5. 用 ThreadingHTTPServer:每个请求一个线程,避免慢请求阻塞其他连接。
    #    allow_reuse_address 在实例化前设,避免重启时 "address in use"。
    socketserver.TCPServer.allow_reuse_address = True
    try:
        httpd = http.server.ThreadingHTTPServer(("0.0.0.0", port), handler)
    except OSError as e:
        print(f"❌ 无法启动服务: {e}")
        print(f"   端口 {port} 可能被占用,换个端口试试: python serve.py {port + 1}")
        input("按 Enter 退出…")
        return 1

    url = f"http://localhost:{port}/demo.html"
    print("=" * 50)
    print("🎬 demo 网页托管服务已启动")
    print("=" * 50)
    print(f"  网页地址:  {url}")
    print(f"  托管目录:  {SERVE_DIR}")
    print(f"  视频流后端: ws://{BACKEND_HOST}:{BACKEND_PORT}/ws  (WebSocket)")
    print(f"  后端连接: ⏳ 检测中…(每 {BACKEND_POLL_INTERVAL} 秒刷新,状态变化时才提示)")
    print("=" * 50)

    # 6. 后台线程持续轮询后端状态,不阻塞 HTTP 服务。
    #    状态变化才打印,所以控制台不会被刷屏。
    threading.Thread(target=poll_backend_status, daemon=True).start()

    # 7. 异步打开浏览器(守护线程,失败/慢都不影响服务)
    def _open_browser():
        try:
            webbrowser.open(url)
        except Exception:
            pass

    browser_thread = threading.Thread(target=_open_browser, daemon=True)
    browser_thread.start()

    # 8. 启动服务(阻塞主线程,直到 Ctrl+C 或窗口关闭)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\n👋 已停止")
    finally:
        httpd.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())

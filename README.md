# 拾像 · SnapDock

纯 Rust 原生 Windows 截屏与桌面贴图工具（零网页、零 WebView、零 Electron、零 Node.js）。界面与画布由 `winit` + `egui` + `wgpu` 原生 GPU 渲染，标注导出由 `tiny-skia` + `ab_glyph` 纯 Rust 高精度光栅化，屏幕捕获基于 Win32 DXGI Desktop Duplication，独立同步服务采用 Rust / Axum + SQLite。交互参考 PixPin，代码和视觉设计独立实现。

## 已实现

- **纯 Rust 原生架构**：单个轻量桌面可执行文件 `SnapDock.exe`（配可选 UIA 控件辅助程序 `snapdock-inspect.exe`），不依赖 WebView2、Chromium 或 Node.js 运行时。
- **单实例与系统托盘**：Win32 互斥锁与跨进程唤醒；启动后静默驻留系统托盘，无多余主窗口；支持系统登录自启。
- **极简双界面架构**：UI 拆分为两个按需创建的独立窗口——**配置界面**（托盘右键菜单「配置」或 `Alt+Shift+S` 进入）与**截图结果编辑界面**（截图选区点击「保存」或 `Ctrl+S` 进入，内置标注、OCR/翻译与底部内联保存栏）。截图及标注仅存于内存，未保存时关闭即释放。
- **多显示器区域截屏**：DXGI 桌面复制（兼容 SDR 与 HDR），多显示器逐屏 1:1 物理对齐，支持窗口吸附、可选原生控件识别、像素放大镜（含坐标与 RGB/HEX 色值，按 `C` 复制色值）、选区移动及 8 向缩放；`Enter` / 双击直接复制到剪贴板，点击「保存」或 `Ctrl+S` 进入结果编辑界面。
- **向下滚动长截图**：框选后可选择自动 / 手动滚动模式，点击开始后采集；采集时使用纯 Win32 空心穿透边框（已排除在截屏之外）和悬浮控制条。支持自动滚动失效回退手动、逐行重叠对齐、固定页眉 / 页脚配置。
- **区域 GIF 动态截图**：独立线程采集、帧率 / 时长 / 最大宽度可调、纯 Rust 编码与内置动画预览。
- **无边框置顶桌面贴图**：全图可拖动、滚轮缩放（25%–200%）、不透明度（20%–100%）、鼠标穿透、全局显示 / 隐藏全部贴图（重新显示时自动解除穿透）、悬停工具栏与 GIF 动画播放。
- **矢量标注与无损重编**：选区与结果编辑界面共用工具栏：几何图形（矩形、线、圆、画笔）、箭头、文本（文本、水印、序号）、马赛克（马赛克、高亮遮罩）、橡皮擦（独立标注层擦除，保留透明原图）与撤销。文本与水印支持拖动、角点等比缩放、顶部旋转手柄（按住 Shift 按 15° 吸附）与二次点击编辑。参数按工具自动记忆。
- **内联保存栏**：结果编辑界面底部直接提供文件名、PNG / JPEG（质量 10–100%）与 GIF 格式、目录浏览及保存按钮，记住上次路径与格式，覆盖前弹出系统确认框，保存后自动关闭窗口并显示悬浮提示。
- **文字提取（OCR）与本地离线翻译**：默认内置免费开源的 **RapidOCR（PP-OCRv5 中英文轻量模型 + ONNX Runtime 动态加载）**，离线开箱即用；**本地翻译**复用同一个 ONNX Runtime 运行库，支持开源 **OPUS-MT INT8 ONNX** 语言包（按需手动下载至程序同级 `models/translate/` 目录，不随安装包内置），支持英语中转多语种桥接，并保留自定义 JSON API 与兼容 OpenAI Chat Completions 的 AI 接口。
- **端到端加密配置同步**：支持自定义 HTTPS 服务地址（默认 `https://127.0.0.1:8899`）、自定义 PEM 公钥证书验证、Argon2id 认证、scrypt + AES-256-GCM 配置加密、版本冲突保护、每 2 分钟自动同步，以及 Windows DPAPI 本机密钥加密存储。

## 开发运行

环境：Windows 10 / 11、Rust stable。此工作区已准备 `.tools` 下的便携 GNU 工具链，脚本会自动使用它，不修改系统 PATH。

直接运行纯 Rust 桌面客户端：

```powershell
.\scripts\Run.ps1
```

或手动加载便携工具链并运行：

```powershell
. .\scripts\Environment.ps1
cargo run -p snapdock
```

启动本机同步服务并开放注册：

```powershell
.\scripts\Start-Server.ps1 -EnableRegistration
```

客户端「设置 → 账号与同步」：

1. 服务地址填写 `https://127.0.0.1:8899`。
2. 导入 `.runtime/server/server.crt` 公钥证书。
3. 点击「检查连接」，填写用户名（3–40 字符）及至少 10 位密码，注册或登录。
4. 点击「上传本机配置」或「下载云端配置」，也可开启自动同步。

不连接同步服务时，本机截屏、贴图、标注和设置完全可用。本地翻译语言包可在「配置 → 文字与翻译 → 翻译」中按需手动下载至程序目录，也可切换为自定义 API 或 AI 模型。

## 后端管理

```powershell
. .\scripts\Environment.ps1
cargo run --manifest-path src/server/Cargo.toml -- --data-dir .runtime/server init
cargo run --manifest-path src/server/Cargo.toml -- --data-dir .runtime/server serve
```

默认关闭注册。管理员可修改数据库设置或创建账号：

```powershell
cargo run --manifest-path src/server/Cargo.toml -- --data-dir .runtime/server settings registration_enabled true
cargo run --manifest-path src/server/Cargo.toml -- --data-dir .runtime/server settings registration_enabled false
cargo run --manifest-path src/server/Cargo.toml -- --data-dir .runtime/server create-user myuser
```

## 快捷键

| 快捷键           | 动作                                    |
| ---------------- | --------------------------------------- |
| Alt + Shift + A  | 区域截图                                |
| Alt + Shift + P  | 剪贴板贴图；无图像时进入截图            |
| Alt + Shift + L  | 长截图                                  |
| Alt + Shift + G  | GIF 动态截图                            |
| Alt + Shift + H  | 显示 / 隐藏全部贴图；重新显示时解除穿透 |
| Alt + Shift + S  | 打开独立配置界面                        |
| Enter / 双击     | 确认选区并复制到剪贴板                  |
| Ctrl + S（选区） | 进入截图结果编辑界面                    |
| Esc / 右键       | 取消选区                                |
| C（未框选时）    | 复制光标处像素 HEX 色值                 |
| Ctrl + C / S / Z | 编辑器中复制 / 保存并关闭 / 撤销        |

## 测试与打包

运行全部纯 Rust 单元测试与构建：

```powershell
.\scripts\Test.ps1
```

发布打包（输出 `dist-app/SnapDock-1.0.0-win.zip` 与 `dist-app/server/snapdock-server.exe`）：

```powershell
.\scripts\Package.ps1
```

若需在交互式桌面上运行真机 DXGI 截屏冒烟测试：

```powershell
. .\scripts\Environment.ps1
cargo test -p snapdock -- --ignored
```

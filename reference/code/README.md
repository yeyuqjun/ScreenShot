# 参考代码（reference/code）

从 GitHub 抓取的截屏类开源项目**核心源码**，用于学习架构与实现思路。
抓取时间：2026-10-16。仅含核心源码，不含构建文件与资源。

## 项目总览

| 目录 | 项目 | Stars | 语言 | 许可证 | 参考价值 |
| --- | --- | --- | --- | --- | --- |
| `flameshot/` | [flameshot-org/flameshot](https://github.com/flameshot-org/flameshot) | 31.2k | C++/Qt | GPL-3.0 | 跨平台屏幕抓取、截图后标注交互 |
| `sharex/` | [ShareX/ShareX](https://github.com/ShareX/ShareX) | 40.1k | C# | GPL-3.0 | Windows 捕获模式设计、捕获后任务流水线 |
| `mss/` | [BoboTiG/python-mss](https://github.com/BoboTiG/python-mss) | 1.3k | Python | MIT | 最小跨平台截图库，各平台底层实现 |

## 1. flameshot — 跨平台截图（C++/Qt）

**阅读顺序：**

1. `src/main.cpp` — 程序入口（GUI / CLI 双模式）
2. `src/core/flameshot.cpp` / `.h` — 应用核心：启动、配置、请求分发
3. `src/utils/screengrabber.cpp` / `.h` — **核心**：多平台屏幕图像获取

**要点：**

- `screengrabber` 用平台宏（`Q_OS_WIN` / `Q_OS_UNIX` / `Q_OS_MACOS`）+ X11 检测，一份代码覆盖 Windows / Linux(X11、Wayland) / macOS
- 职责分离清晰：先抓全屏位图 → 交给上层 `CaptureWidget` 做选区与标注
- 架构：`main → core/flameshot → utils/screengrabber → capture 层`

## 2. ShareX — Windows 截图全家桶（C#）

**阅读顺序：**

1. `ShareX/CaptureHelpers/CaptureBase.cs` — 所有捕获模式的抽象基类
2. `CaptureFullscreen.cs` / `CaptureMonitor.cs` / `CaptureWindow.cs` / `CaptureRegion.cs` / `CaptureCustomRegion.cs` — 五种捕获模式
3. `ShareX.ScreenCaptureLib/RegionCaptureOptions.cs` — 区域截图配置项

**要点：**

- 每种捕获模式一个独立小类（40~180 行），继承 `CaptureBase`，是**策略模式**的典范：新增捕获模式 = 新增一个类
- Windows 底层用 GDI（`CopyFromScreen` / `BitBlt` 思路）
- 设计重心是"捕获后任务流水线"：捕获 → 编辑 → 保存/复制/上传，任务可配置

## 3. mss — 最小跨平台截图库（Python）

**阅读顺序：**

1. `src/mss/__init__.py` — 对外 API（`with mss() as sct: sct.shot()`）
2. `src/mss/factory.py` — 平台分发，按系统 import 对应后端
3. `src/mss/base.py` + `src/mss/screenshot.py` — `MSSBase` 与 `shot()` 核心流程
4. `src/mss/models.py` — `Monitor` / `Monitors` 数据模型（多显示器抽象）
5. `src/mss/windows/gdi.py` — Windows 后端：`BitBlt` + `GetDIBits`
6. `src/mss/linux/__init__.py` + `base.py` — Linux 后端选择（XGetImage / XSHM / XCB）
7. `src/mss/linux/xgetimage.py` / `xshmgetimage.py` / `xlib.py` — 三种 Linux 抓取后端
8. `src/mss/darwin.py` — macOS 后端（CoreGraphics）

**要点：**

- 核心约 3k 行，是理解"跨平台截图最小实现"的最佳样本
- 平台差异全部收敛到各后端模块，上层 API 完全统一
- `models.py` 的 Monitor 抽象（虚拟拼接屏 vs 物理屏）值得借鉴

## ⚠️ 许可证合规提醒

| 项目 | 许可证 | 能否复制代码进自己的项目 |
| --- | --- | --- |
| flameshot | GPL-3.0 | ❌ 有传染性：复制代码将迫使你的项目整体以 GPL 开源 |
| ShareX | GPL-3.0 | ❌ 同上 |
| mss | MIT | ✅ 可自由使用/修改/闭源商用（保留版权声明） |

**结论：参考架构、接口设计和算法思路没有限制；直接复制 GPL 源码需谨慎。**

## 其他可选参考（未抓取）

- [greenshot/greenshot](https://github.com/greenshot/greenshot) — C#，Windows 轻量经典款（5.1k★）
- [ksnip/ksnip](https://github.com/ksnip/ksnip) — C++/Qt，跨平台（3.4k★）
- [dynobo/normcap](https://github.com/dynobo/normcap) — Python，OCR 截图（2.8k★）
- [SnapXL/SnapX](https://github.com/SnapXL/SnapX) — C#，ShareX 的跨平台分支（1k★）

## Git 说明

本目录为第三方源码，尚未提交。是否纳入版本库（或加入 `.gitignore`）由你决定——
建议：仅提交本 README 做索引，第三方代码本身不入库。

# Electron → Tauri 2 迁移进度

> 最后更新：2026-11-08（阶段 6 进行中）
> 计划批准范围：Tauri 2 全量迁移，UI 保持 React 不变，桌面能力全部 Rust 重写。

## 总体状态

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| 0 | 项目清理（关于面板、docs/audits、孤儿脚本、.gitignore、~5.3GB 本地产物） | ✅ 已提交 cb86f04 |
| 1 | Tauri 脚手架 + 桥接对等（bootstrap/settings） | ✅ 已提交 161524d / ecd0a9b |
| 2 | 持久化与系统集成（vault/history/托盘/热键/单实例） | ✅ 已提交 fa5d489 |
| 3 | 截屏管线（DXGI、overlay、native、保存/复制） | ✅ 已提交 ff28bbe（真机验证） |
| 4 | 贴图与编辑（pin 窗口、image.*、snap-media 协议） | ✅ 已提交 ff28bbe（真机验证） |
| 5 | 长截图与 GIF（stitch 移植、录制线程、编码） | ✅ 已提交 495726c（GIF 真机验证） |
| 6 | 网络（同步、OCR/翻译、证书） | 🔨 代码已写，**编译差一步**（见下） |
| 7 | 切换收尾（删 Electron、脚本/CI/文档、打包） | ⬜ 未开始 |

## 阶段 6 当前状态（下次从这里继续）

**已完成（未提交，工作区中有代码）：**

- `Cargo.toml`：已加 reqwest(rustls)/scrypt/aes-gcm/x509-cert/url 依赖
- `src/app/src/network.rs`：endpoint 校验、request_json（自定义 CA、45s/4MB 限制）、service_request、scrypt derive_key（N=16384,r=8,p=1）、AES-256-GCM encrypt/decrypt_config（AAD "snapdock-config-v1"）、process_text（OCR/翻译/AI provider）、parse_certificate（手动 PEM→DER）、utc_iso、6 个单元测试
- `src/app/src/bridge.rs`：已加 account.meta / account.login / account.logout / sync（auto/upload/download 含冲突保护）/ certificate.choose / text.process 处理器（约 780-1010 行）
- `src/app/src/main.rs`：已注册 `mod network`
- 35 个测试全部通过（阶段 5 收尾时）

**卡点（下一步第一件事）：**

- `network.rs:321` 编译错误：`x509_cert::Certificate::from_der` 找不到 —— 原因是 `der::Decode` trait 未导入。修复：在 parse_certificate 里加 `use der::Decode;`（x509-cert 0.2 依赖 der 0.7，trait 来自 `der` crate，需要把它加进 Cargo.toml 或直接 use）。
- 修完跑 `cargo build -p snapdock` 清剩余 2 个警告（大概是未用导入），跑 `cargo test -p snapdock`。
- 然后把 sync 接入 update_settings：在 update_settings 里校验 `network::endpoint(service.url)`（Electron 在这里校验服务地址），错误要能阻止保存。
- 自动同步定时器：120s 间隔（settings.service.autoSync && account && !sync_flight → sync("auto")，失败 emit "syncError"）。放在 main.rs setup 里 tauri::async_runtime::spawn + loop。
- 真机验证：`Start-Server.ps1` 起 Rust 同步服务 → 设置页账号与同步 → 检查连接/注册/登录/上传/下载。可用 `node scripts/smoke-server.cjs` 回归服务端。

## 阶段 7 清单（未开始）

1. 删除 `src/desktop/`、`main` 字段、Electron/koffi/pngjs/gifenc 依赖
2. 删除 tests/*.electron.cjs、fixtures/*.electron、test-ui.cjs；npm scripts：`test` → `cargo test`（snapdock + snapdock-server 各自跑）、删 test:ui/:desktop/:capture/:long、`dev` 语义改为 dev-rs（保留 dev:web 预览）
3. `scripts/Package.ps1` 重写：cargo build --release（-p snapdock + -p snapdock-server）→ zip（snapdock.exe、WebView2Loader.dll 如有、resources/native/snapdock-inspect.exe、server/snapdock-server.exe）；更新 icon 资源引用
4. CI（.github/workflows/ci.yml）：Node build + cargo test 两 crate + smoke-server；去掉 Electron 测试
5. README / docs/architecture.md / CHANGELOG 更新（新架构：Tauri 2 + WebView2；数据目录不变；vault 需重新登录）
6. docs/verification.md 增补手测清单

## 关键技术决策与坑（务必记住）

1. **便携 GNU 工具链**：bash 里 `export RUSTUP_HOME=$PWD/.tools/rustup CARGO_HOME=$PWD/.tools/cargo PATH="$PWD/.tools/cargo/bin:$PWD/.tools/w64devkit/bin:$PATH"`。Tauri/wry 在 x86_64-pc-windows-gnu 下编译通过。
2. **cargo build 前必须先 `taskkill //IM snapdock.exe //F`**，否则 exe 被占用（os error 5/32），会用旧二进制跑出诡异现象（黑图事件就是这么来的）。
3. **泛型闭包桥接 FFI 崩溃**：`EnumWindows` 回调经 `Some(bridged::<F> as extern "system" fn)` 泛型桥接在 GNU 工具链下访问违禁（0xc0000005）。已改为先收集 HWND 到 Vec 再处理（native.rs collect_top_windows）。
4. **windows crate 0.62**：feature 是 `Win32_Graphics_Direct3D11`（模块路径 `Graphics::Direct3D11`），D3D11CreateDevice 需要 `Win32_Graphics_Direct3D`（D3D_DRIVER_TYPE 在 Direct3D 模块）；GetCursorPos/SetCursorPos 在 `Win32_UI_WindowsAndMessaging`；`SetLayeredWindowAttributes` 用 `LAYERED_WINDOW_ATTRIBUTES_FLAGS`。EnumChildWindows 的 hwnd 参数是 `Option<HWND>`；mouse_event 的 dwData 是 i32。
5. **DXGI**：HDR 桌面输出 R16G16B16A16_FLOAT(10)，SDR 是 B8G8R8A8(87)——两条转换路径都在；复制首帧可能是全黑，已跳过第一帧；录制用 ScreenGrabber 复用 duplication。
6. **Tauri 2 注意**：没有 `tauri::dpi`（用 `tauri::{PhysicalPosition, PhysicalSize}`）；WebviewWindow 没有 `set_opacity`（pins.rs 用 native::set_window_opacity → WS_EX_LAYERED + LWA_ALPHA）；json! 宏不支持 `[变量]` 动态键；serde_json 开了 preserve_order（指纹兼容）；tokio 需要 "time" feature。
7. **snap-media 协议**：Windows WebView2 不支持非标准协议，wry 映射 `http://snap-media.localhost/*`，media_url 生成这种 URL（state.rs），协议处理器按 `/capture/<file>` 解析（main.rs snap_media_response）；CSP 已加对应来源。
8. **桥接约定**：命令 `snapdock(method, args)` 返回 `{ok,value,error}` 信封不抛错；事件单通道 `snapdock:event`（payload {type,value}）；未迁移方法返回 Err("此功能尚未迁移到新的桌面端")。
9. **数据目录**：`%APPDATA%/snapdock`（与 Electron userData 一致，历史/截图直接沿用）；vault.bin 兼容读取 Electron safeStorage（"v10" 前缀 + CryptProtectData；本机旧 vault 解密失败已自动备份为 vault.bin.unreadable，用户需重新登录同步）。
10. **测试验证方法**：真机流程用 computer-use 技能（getApp by pid → elements → click/drag）；全局热键用 `agent.computerUse.computer.key`（SendKeys 不触发 RegisterHotKey）。

## 已验证能力清单（Tauri 版）

- ✅ 启动/单实例/托盘/热键（5 个）/主题/开机自启（打包版）
- ✅ 设置保存/重置/导出/导入（DPAPI vault 重启持久化）
- ✅ 历史加载（旧 Electron history.json 15 条沿用）
- ✅ 截屏全链路：冻结画面→窗口吸附→选区→标注工具栏→复制（剪贴板 1000×640 内容正确）/保存/贴图/打开编辑器
- ✅ 贴图窗口渲染（snap-media 协议）、关闭释放
- ✅ GIF：录制→完成→编码→编辑器打开（真机）
- ✅ 长截图：算法层 35 测试通过（真机滚动流程未手测，阶段 7 验收时补）
- ⏳ 同步/账号/OCR：代码就绪，编译修复后验证

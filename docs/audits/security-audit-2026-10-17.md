# 安全漏洞审计报告

- 审计日期：2026-10-17
- 审计对象：SnapDock v1.0.0（提交 `47fe456`，分支 codex/screenshot-desktop）
- 审计范围：Rust 配置同步服务（`src/server`，约 950 行）、Electron 主进程与桌面模块（`src/desktop`，约 3300 行）、React 界面（`src/ui`，约 4400 行）、构建脚本（`scripts`）
- 审计方式：人工代码审查（静态），未做动态渗透测试，未核查第三方依赖 CVE
- 结论摘要：**未发现可直接远程利用的高危漏洞**；发现 2 项中危（渲染进程被攻陷场景下的纵深防御缺口）、8 项低危/加固项

---

## 1. 风险总览

| 编号 | 等级 | 问题 | 位置 |
| ---- | ---- | ---- | ---- |
| M1 | 中 | IPC 可向任意绝对路径写文件（含 UNC 外泄通道） | `src/desktop/export.cjs` `saveOptions`、`src/desktop/main.cjs` `writeExport` |
| M2 | 中 | 渲染进程可借主进程发起任意网络请求（SSRF/唯一外泄出口） | `src/desktop/network.cjs` `endpoint`、`text.process` 等 IPC |
| L1 | 低 | `history.json` 的 `item.poster` 未做路径校验（任意文件读/删） | `src/desktop/main.cjs` `staticImage`、`image.remove` |
| L2 | 低 | 导入配置可静默重定向 OCR/翻译到恶意端点 | `src/desktop/main.cjs` `settings.import` |
| L3 | 低 | CSP 缺 `form-action 'none'` | `src/desktop/main.cjs` CSP 构造处 |
| L4 | 低 | 退出登录仅撤销当前 token，无全量撤销/改密能力 | `src/server/src/lib.rs` `logout` |
| L5 | 低 | 用户名枚举（注册 409 明示冲突） | `src/server/src/lib.rs` `register` |
| L6 | 低 | 限流按 IP 计、内存态，重启清零 | `src/server/src/lib.rs` `AppState::limit` |
| L7 | 低 | Windows 下 `server.key` 无显式 ACL | `src/server/src/main.rs` `ensure_certificate` |
| L8 | 低 | Docker 默认绑定 `0.0.0.0` 且自签证书 SAN 仅 localhost | `src/server/Dockerfile`、`main.rs` |

---

## 2. 中危详情

### M1. IPC 可向任意绝对路径写文件（渲染进程 → 文件系统的提权原语）

**位置**：

- `src/desktop/export.cjs` → `saveOptions(input)`
- `src/desktop/main.cjs` → `writeExport(buffer, options, parent)`、`image.save`、`capture.select`（`selection.saveOptions` 分支）

**描述**：
`saveOptions` 对文件名校验严格（Windows 保留名、非法字符、控制字符、尾部点/空格、长度上限），但保存目录 `input.directory` 仅检查 `path.isAbsolute(input.directory.trim())`，没有目录约束。渲染进程（如被 XSS 或供应链攻击攻陷）可调用 `image.save` 并自造 `options`：

1. 将 PNG/JPG/GIF 字节写入任意当前用户可写的路径（如 `shell:startup` 启动目录——受图片扩展名约束，不能直接执行，但可作为落地载荷的一部分）；
2. 写入 UNC 路径 `\\attacker\share\x.png`，绕过 CSP `connect-src 'self'` 的网络封锁，形成**数据外泄通道**（由主进程发起 SMB 写入）；
3. 覆盖已有文件需用户点击「替换」确认（先以 `flag: "wx"` 探测存在性），此点部分缓解了危害。

**影响**：渲染进程攻陷 → 任意位置文件写入 / 截图内容外泄。扩展名与文件内容受限（图像字节），不能直接改写 .exe/.lnk。

**建议**：

- 保存目录只允许来自 `directory.choose` / 系统保存对话框选择过的路径（主进程维护已批准目录集合），写入前比对；
- 或限定保存目录位于用户 profile / 图片目录之下；
- 显式拒绝 UNC 路径（`\\server\share` 形式）。

---

### M2. 渲染进程可借主进程发起任意网络请求（SSRF / 唯一外泄出口）

**位置**：

- `src/desktop/network.cjs` → `endpoint(value, allowLocalHttp)`、`requestJson`、`processText`
- IPC：`text.process`、`account.meta`、`account.login`

**描述**：
`endpoint()` 允许任意 HTTPS URL 及 localhost 的明文 HTTP。渲染进程在 CSP 限制下无法自行外联，`text.process` 是其唯一网络出口。渲染进程被攻陷后可以：

1. 对 `http://127.0.0.1:*` 的本机 HTTP 服务 POST 任意 JSON（附带 `Authorization: Bearer <apiKey>` 头），并经 `responseText` 读回响应文本——构成 SSRF 探测/调用内网服务的能力；
2. 向攻击者控制的 HTTPS 端点发送包含截图 base64 / 选中文本的请求体——直接外泄数据。

这是 OCR/翻译功能的固有攻击面；目前新增或修改 provider URL 时无任何用户确认。

**建议**：

- 首次使用新 provider URL 时弹窗要求用户确认（信任提示含完整 URL）；
- 或维护用户已批准的 URL 白名单，新增条目需经 `directory.choose` 同级别的显式交互。

---

## 3. 低危 / 加固项

### L1. `history.json` 的 `item.poster` 未做路径校验

**位置**：`src/desktop/main.cjs`（启动加载过滤、`staticImage`、`image.remove`）

加载 `history.json` 时仅以 `/^[a-f0-9-]+\.(png|gif)$/` 过滤 `i.file`，`i.poster` 未校验即拼接使用：

- `staticImage`：`path.join(captureDir, item.poster || item.file)` → `nativeImage.createFromPath`，可读取并显示任意图片文件；
- `image.remove`：`fsp.unlink(path.join(captureDir, item.poster))`，可**删除任意文件**（`..\` 穿越）。

前提：攻击者已具备对 `history.json` 的本地写权限（与受害者同权限级别），因此实际危害有限，属防御纵深缺口。修复成本极低：对 `poster` 套用与 `file` 相同的正则。

### L2. 导入配置可静默重定向 OCR/翻译到恶意端点

**位置**：`src/desktop/main.cjs` `settings.import`

用户主动导入 JSON 属信任动作，但恶意配置文件可改写 `providers.*.*.url` 与 `key`，此后用户使用 OCR/翻译时截图与选中文本即发往攻击者端点。建议导入完成后，对变更过的 provider URL / 服务地址做一次展示确认。

### L3. CSP 缺少 `form-action 'none'`

**位置**：`src/desktop/main.cjs`（`onHeadersReceived` 中的 CSP 字符串）

当前生产 CSP 为 `default-src 'none'` 等严格策略，且代码中无 HTML 注入点（React 全转义、无 `dangerouslySetInnerHTML` / `eval`），故实际风险低。但 `form-action` 未设置且不回退到 `default-src`：若未来引入注入点，`<form>` 提交可绕过 `connect-src` 外传数据。属防御纵深，建议补 `form-action 'none'`。

### L4. 退出登录仅撤销当前 token

**位置**：`src/server/src/lib.rs` `logout`

`DELETE FROM sessions WHERE token_hash=?1` 只删除当前会话；同一用户的其他有效会话（含被窃 token）存活至 TTL（默认 168 小时）。当前无「撤销全部会话」与改密功能，token 泄露后无法止血。建议增加全量撤销接口，或在产品层面补改密流程（改密后强制失效全部旧会话）。

### L5. 用户名枚举

**位置**：`src/server/src/lib.rs` `register`

注册冲突返回 409「此用户名已被使用」，可被用于枚举有效用户名。属产品取舍（注册 UX 常见做法）；若在意可统一返回「注册请求已受理」并在邮件侧提示冲突。

### L6. 限流按 IP 计、内存态

**位置**：`src/server/src/lib.rs` `AppState::limit`

- 计数器存于进程内存，服务重启即清零；
- NAT 后多用户共享同一 IP 限额（login 15/分钟、register 10/小时）；
- 不信任 `X-Forwarded-For`（防伪造）是正确取舍，但 Docker 部署于反向代理之后时，所有客户端会共享单一限额。

建议：持久化或接受现状；部署文档中说明反代场景的限流特性。

### L7. Windows 下 `server.key` 无显式 ACL

**位置**：`src/server/src/main.rs` `ensure_certificate`

私钥文件以 `0o600` 创建的逻辑仅对 Unix 生效（`#[cfg(unix)]`）；Windows 下继承数据目录 ACL。数据目录通常位于用户 profile 内，实际可接受；建议显式收紧 ACL 或注明前提。

### L8. Docker 部署默认值不匹配

**位置**：`src/server/Dockerfile`、`src/server/src/main.rs`

镜像默认 `CMD ["serve", "--bind", "0.0.0.0:8899"]` 对外暴露，而自签证书 SAN 仅含 `localhost` / `127.0.0.1` / `::1`，远程客户端主机名校验必然失败。该失败可能诱导部署者关闭证书校验（代码中并未关闭，是正确行为）。建议文档补充为真实域名单独生成证书的步骤。

---

## 4. 已确认安全的要点（逐项核实实现）

### Electron 边界

- `contextIsolation: true`、`sandbox: true`、`nodeIntegration: false`、`webSecurity: true`
- `setWindowOpenHandler(() => ({ action: "deny" }))`；`will-navigate` 仅允许当前页面；
- `setPermissionRequestHandler` 全部拒绝
- preload 暴露 42 个方法的严格白名单，且每个 IPC 请求先过 `trusted(event.senderFrame.url)` 校验
- 生产 CSP：`default-src 'none'`、`script-src 'self'`（无 unsafe-inline）、`connect-src 'self'`——渲染进程无法自行外联
- `snap-media:` 协议 handler 对文件名执行 `^[a-f0-9-]+\.(png|gif)$` 正则并校验 hostname，无路径穿越
- koffi FFI 仅加载固定系统 DLL（user32/dwmapi），不接受外部 DLL 名；`execFile` 传参数数组 + 1400ms 超时 + maxBuffer，无 shell 注入；inspect 子进程只接收数字参数

### 认证与密码

- 服务端 argon2id（默认参数）；登录对不存在用户也执行假哈希验证，防时序侧信道用户名探测
- 会话 token：`OsRng` 256-bit 随机，base64url 43 字符，存储 SHA-256 摘要；`Bearer` 严格长度校验
- 云配置 E2E：客户端 scrypt（N=16384, r=8, p=1）派生密钥 → AES-256-GCM + AAD `snapdock-config-v1` + 12B 随机 nonce；服务端只见密文
- 服务端对 nonce（须 12 字节）与密文（16 B–128 KB）做长度校验；revision 乐观锁 + Immediate 事务防并发覆盖
- 凭据存于 safeStorage（DPAPI）加密的 vault；配置导出自动剥离 API key

### 输入处理

- 全部 SQL 参数化，无字符串拼接（逐条核对）
- 设置合并 `mergeKnown` 按 `Object.keys(base)` 结构白名单进行——原型污染免疫（`settings.import` / 云端配置 / vault 三条入口均经 `normalize`）；`responseText` 显式拦截 `__proto__` / `prototype` / `constructor` 路径段
- 文件名校验覆盖 Windows 保留名、控制字符、尾部点/空格；标注数据 `validateAnnotations` 逐字段白名单 + 数量上限
- 资源上限齐全：配置 256 KB、导入图片 40 MB、编辑图 50 MB / 80 Mpx、HTTP 响应 4 MB、GIF 300 帧 / 128 MB / 60 秒——解压炸弹有防护
- 注册默认关闭（`registration_enabled=false`）；argon2 并发闸（Semaphore 4）防哈希 DoS
- 响应头：`cache-control: no-store`、`x-content-type-options: nosniff`、HSTS

---

## 5. 建议的后续动作

1. **优先修复 M1**（保存目录约束）与 **L1**（poster 正则）——改动小、收益直接
2. 评估 M2 的 URL 确认交互（涉及产品体验，可与设置页合并设计）
3. 运行依赖审计（本次未覆盖）：`npm audit`、`cargo audit`
4. 视产品规划补「撤销全部会话 / 修改密码」能力（L4）
5. 小项顺手修：CSP 补 `form-action 'none'`（L3）、Docker 部署文档（L8）

---

## 附：审计覆盖清单

| 文件 | 行数 | 审查 |
| ---- | ---- | ---- |
| `src/server/src/lib.rs` | 460 | 全量 |
| `src/server/src/main.rs` | 158 | 全量 |
| `src/server/src/inspect.rs` | 113 | 全量 |
| `src/server/schema.sql` / `Dockerfile` / `Cargo.toml` | - | 全量 |
| `src/desktop/main.cjs` | 1692 | 全量 |
| `src/desktop/network.cjs` | 223 | 全量 |
| `src/desktop/settings.cjs` | 86 | 全量 |
| `src/desktop/export.cjs` / `annotation-data.cjs` / `annotation-store.cjs` / `native.cjs` / `clipboard.cjs` | ~460 | 全量 |
| `src/desktop/preload.cjs` / `window-options.cjs` / worker 系列 | ~300 | 全量 |
| `src/ui/*`（11 个文件） | ~4400 | 重点（IPC 调用面、渲染注入点、凭据处理） |
| `scripts/*`、`tests/*` | - | 抽查（仅开发期使用，不随应用分发） |

*报告由静态代码审查生成；「未发现」不代表不存在漏洞，建议结合动态测试与依赖审计复核。*

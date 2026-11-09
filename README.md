# ClipboardX

一款桌面剪贴板历史管理工具：自动记录复制过的文本与图片，用全局快捷键随时唤出，支持收藏、备注、历史归档与 Telegram 同步。

本分支（`feat/nsh`）用 [Native Shell (NSH)](https://nsh.dev) 以纯 Rust 重写了原 Tauri + React 版本（见 `main` 分支）：界面是 NSH 组件（macOS 上由 AppKit 原生绘制），不再有 WebView、Node 与前端构建链。

---

## 功能

1. **全量历史记录**：每 500ms 读取一次剪贴板，记录文本与图片；内容按哈希去重，重复复制的条目移到最前
2. **快捷键唤出**：默认 `Cmd+Shift+V`（Windows/Linux 为 `Ctrl+Shift+V`），可在设置中修改，保存后立即生效
3. **今日历史 / 收藏夹**：内容按时间倒序排列，支持搜索（文本与备注）和"只看图片"
4. **历史归档**：非当日内容按日期分组折叠，点击展开
5. **收藏与备注**：悬停条目可添加备注、收藏、删除；点击图片缩略图查看大图
6. **快捷复制**：点击条目复制回剪贴板；`Enter` 或 `⌘⇧1`–`⌘⇧9` 复制并收起窗口
7. **设置**：失焦自动隐藏、外观主题（浅色/深色/跟随系统）、历史保留时间（一周/一个月/一年/永久）、清除历史、检查更新
8. **Telegram 同步**：配置 Bot Token 与 Chat ID 后，收藏变化时把收藏列表推送到 Telegram
9. **密码本**：加密保存账号、密码、网址、两步验证码（TOTP）和备注，支持密码生成器、Touch ID 解锁和加密备份（见下文）

### 键盘

| 按键 | 作用 |
|---|---|
| `Esc` | 关闭弹窗；没有弹窗时收起窗口 |
| `Tab`，或 `←` / `→`（搜索框为空时） | 切换今日历史 / 收藏夹 |
| `↑` / `↓` | 移动选中项 |
| `Enter` | 复制选中项并收起窗口 |
| `⌘⇧1`–`⌘⇧9`（Windows/Linux：`Ctrl+Shift+1`–`9`） | 复制第 N 条并收起窗口 |

窗口唤出时焦点在搜索框，直接输入即可过滤。

密码本页面：`↑` / `↓` 选择，`Enter` 复制密码，`⌘N` 新建，`⌘L` 锁定。

---

## 密码本

顶栏钥匙图标进入。首次使用时设置主密码（至少 10 位）；**主密码无法找回**。

- **加密**：随机生成的 256 位密钥以 XChaCha20-Poly1305 加密每个条目（名称在内的所有字段），条目 id 作为附加认证数据；该密钥再由主密码经 Argon2id（64 MiB、3 轮）派生的密钥包装后存入 SQLite。数据库里没有明文。
- **解锁**：主密码；连续输错 3 次后等待时间逐次翻倍（最长 60 秒，重启不清零），修改主密码、开启 Touch ID、导出时的密码校验同样计数。可在设置中开启 Touch ID（失败可用系统密码），只接受属于本密码本的密钥。
- **自动锁定**：无操作 1/5/15/30 分钟后、收起面板时（可关）、系统睡眠或锁屏时。
- **复制**：复制的账号、密码和验证码带有 `org.nspasteboard.ConcealedType` 标记，ClipboardX 和其他遵守约定的剪贴板工具都不会记录；30 秒后若剪贴板内容未变则自动清空。ClipboardX 也不再记录 1Password 等工具复制出的密码。
- **两步验证**：粘贴 `otpauth://` 链接或 Base32 密钥，列表中显示当前验证码与剩余秒数，点击复制。
- **密码生成器**：长度 12/16/20/32，可选大写、小写、数字、符号，可排除易混淆字符。
- **修改主密码**：同时更换数据密钥并重新加密全部条目，旧密码配合旧的数据库备份也无法再解开。
- **加密备份**：设置中导出为 `.cxvault`（需先输入主密码；用备份密码另行派生密钥加密），导入时可选合并（同一条目保留较新的）或替换。
- 密码本内容不会进入 Telegram 同步、日志或错误信息。

**已知限制**：

- 当前 ad-hoc 签名下，Touch ID 采用"先通过系统验证、再读取仅本应用可访问的钥匙串项"的方式。最严格的"仅生物识别可读"钥匙串访问控制需要 Developer ID 签名。
- 每次重新构建（签名变化）后，系统可能询问是否允许访问钥匙串。
- 点击"显示"后，明文密码会进入界面控件，无法保证从内存中清零。

---

## 开发

需要 [Rust](https://rustup.rs)（edition 2024）。如需使用 `nsh` 命令，可通过 `brew install longbridge/tap/nsh` 安装。

```sh
cargo run                      # 完整桌面应用：菜单栏图标 + 全局快捷键 + 浮动面板
CLIPBOARDX_SHOW=1 cargo run    # 启动时直接显示面板
cargo test                     # 单元测试 + 无头 UI 测试（tests/ui.rs）
nsh run                        # NSH 开发运行器：普通窗口，无菜单栏图标与全局快捷键
nsh check
```

环境变量：

| 变量 | 作用 |
|---|---|
| `CLIPBOARDX_DATA_DIR` | 数据目录（数据库与日志），默认为系统应用数据目录下的 `com.clipboardx.app` |
| `CLIPBOARDX_NO_CAPTURE` | 设置后不读取剪贴板（测试用） |
| `CLIPBOARDX_SHOW` | 设置后启动时显示面板 |
| `CLIPBOARDX_NO_KEYCHAIN` | 设置后不访问系统钥匙串，即不使用 Touch ID（测试用） |

数据库仍是 `~/Library/Application Support/com.clipboardx.app/clipboardx.db`，表结构与 Tauri 版相同，旧版的历史、收藏和设置会直接沿用。

## 打包（macOS）

```sh
scripts/bundle-macos.sh                 # dist/ClipboardX.app（仅菜单栏，无 Dock 图标），ad-hoc 签名
scripts/bundle-macos.sh --dmg           # 另外生成 dist/ClipboardX-<版本>.dmg
SIGN_IDENTITY="Developer ID Application: …" scripts/bundle-macos.sh --dmg
```

`nsh package --target macos` 也能打包，但它使用 NSH 生成的通用运行器，没有菜单栏图标、全局快捷键和浮动面板，只适合预览界面。

## 平台支持

- **macOS 14+**：完整支持。应用以浮动面板运行：不抢占前台应用的焦点，在鼠标所在屏幕的右侧显示，所有桌面空间可见。
- **Windows / Linux**：`src/main.rs` 会以 NSH 的普通窗口运行同一套界面和剪贴板记录，但尚未接入托盘图标、全局快捷键和失焦隐藏，也未在这两个平台上编译验证。

---

## 项目结构

```
ClipboardX/
├── Cargo.toml
├── nsh.json               # NSH 应用配置：根组件 App、主题、图标
├── theme.json             # 设计令牌（shadcn cssVars 格式），浅色/深色两套
├── assets/                # 应用图标
├── scripts/bundle-macos.sh
├── src/
│   ├── lib.rs             # 模块声明、NSH 根组件注册、root()
│   ├── main.rs            # 桌面入口：初始化日志与剪贴板监听，启动宿主
│   ├── macos.rs           # macOS 宿主：NSPanel、菜单栏图标、全局快捷键、失焦隐藏
│   ├── model.rs           # 数据类型与纯函数（过滤、按日分组、掩码、标签）
│   ├── db.rs              # SQLite 存取（历史与设置）
│   ├── capture.rs         # 剪贴板轮询、PNG 编码与缩略图、复制回剪贴板
│   ├── services.rs        # UI 与宿主共享的服务（数据库、变更计数、窗口桥接、同步）
│   ├── telegram.rs        # 收藏同步到 Telegram
│   ├── updater.rs         # 通过 GitHub Releases 检查新版本
│   ├── hotkey.rs          # 快捷键字符串解析与校验
│   ├── logger.rs          # 日志文件
│   ├── vault/             # 密码本：加密、存储、会话与锁定、TOTP、生成器、备份、钥匙串
│   └── ui/                # NSH 组件
│       ├── mod.rs         # 根组件 App
│       ├── state.rs       # 界面状态（signals）与操作
│       ├── keys.rs        # 键盘快捷键
│       ├── header.rs  main_panel.rs  item_row.rs  archive.rs  settings.rs  dialogs.rs
│       ├── vault/         # 密码本页面：锁屏、列表、编辑器、对话框
│       └── widgets.rs  icons.rs
├── tests/ui.rs            # 无头 UI 测试（nsh-testing）
└── tests/vault_ui.rs      # 密码本无头 UI 测试
```

### 线程模型

- NSH 组件运行在引擎线程；macOS 宿主（AppKit、托盘、快捷键）运行在主线程；剪贴板监听与 Telegram 同步各自运行在独立线程。
- 它们只通过 `services.rs` 交互：界面每 300ms 读取一次原子计数器（数据是否变化、窗口是否被唤出）；界面通过 `WindowHost` 让宿主隐藏窗口或注册新快捷键，调用会投递到主线程执行。

## 与 Tauri 版的差异

- 快捷键修改后立即生效（原为重启后生效），并会校验格式。
- 搜索同时作用于收藏夹（原仅今日历史），键盘导航也同时支持两个列表。
- 检查更新改为查询 GitHub Releases，发现新版本后打开下载页（原为 Tauri updater 自动安装）。
- 历史保留时间改为分段按钮，不再使用下拉框。
- 时间戳精确到毫秒，保证同一秒内的重复复制也能正确排序；旧数据兼容。

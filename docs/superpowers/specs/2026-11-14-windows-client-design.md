# Windows 客户端设计

- 日期：2026-11-14
- 分支：`feat/windows`（基于 `main`）
- 前置：`2026-11-09-password-vault-design.md`、`2026-11-10-vault-sync-design.md`、`2026-11-12-android-app-design.md`
- 状态：范围与验证方式已由用户确认；其余按推荐方案定稿，用户要求直接进入开发

## 1. 目标

在 Windows 10 1809+ / Windows 11（x86_64）上提供与 macOS 版对等的 ClipboardX：托盘图标、全局快捷键唤出的置顶面板、失焦隐藏、防截屏、锁屏/睡眠时锁定密码本、凭据持久化（云同步的秘密密钥与设备令牌）、Windows Hello 解锁、不记录其他密码管理器的敏感复制、安装包与 CI 发布。

不做（第一版）：

- 代码签名（SmartScreen 会提示，发布说明里写明）
- ARM64 原生包（Windows 11 on ARM 可经 x64 仿真运行）
- 自动更新（沿用"打开发布页"）、自动粘贴
- 用 Windows Hello 密钥真正锁住数据密钥（见 5.2 的后续项）
- 界面内的"开机自启"开关（由安装包的可选项写 `HKCU\…\Run`）

## 2. 用户确认的决定

| 问题 | 决定 |
|---|---|
| 首版范围 | 与 macOS 对等 |
| 编译与验证 | 本机（Mac，磁盘紧张）不装 Windows 工具链；推送 `feat/windows`，由 GitHub Actions 的 `windows-latest` 编译、clippy、测试、打包。托盘/快捷键/Windows Hello 等交互在真机手测（第 9 节清单） |

## 3. 按推荐方案定稿的决定（可改）

| 问题 | 决定 | 理由 |
|---|---|---|
| 宿主形态 | 自建 Win32 宿主包住 NSH 顶层 `AppView`，不用 `run_app` | 需要窗口句柄（防截屏、会话通知、Hello 提示的属主、按鼠标所在屏幕定位）；与 `macos.rs` 同构，`WindowHost` 契约不变；`run_app` 还会强加 F11 全屏 |
| 托盘与快捷键 | 继续用 `tray-icon`、`global-hotkey`（已是依赖），不用 NSH 0.5 自带的 | 与 macOS 宿主同一套代码形态；NSH 的接口只能在引擎线程调用，而 `WindowHost` 要求 `Send + Sync` |
| 默认快捷键 | Windows 上为 `Alt+Shift+V` | 全局快捷键会把按键从所有应用手里拿走，而 `Ctrl+Shift+V` 在浏览器、编辑器、终端里是"粘贴为纯文本" |
| 凭据存储 | DPAPI 密封的文件，放在数据目录 `keys\` | 跟随 `CLIPBOARDX_DATA_DIR`，卸载/便携场景可预期；不引入新依赖；凭据管理器的安全性质相同 |
| Windows Hello | `UserConsentVerifier`（确认本人后读取密钥副本） | 与 macOS 现状（先过系统验证、再读钥匙串项）同一流程；见 5.2 的限制 |
| 分发 | Inno Setup 安装包（按用户安装，免管理员）+ 免安装 zip | 安装包给出开始菜单项、卸载项和可选自启；zip 给不想安装的人 |
| C 运行库 | 静态链接（`.cargo/config.toml`） | 不依赖 Visual C++ 运行库 |

## 4. 架构

同一个 crate、同一套 `ui`。平台差异集中在：

| 文件 | 内容 |
|---|---|
| `src/windows_host.rs`（bin） | 宿主：面板、托盘、快捷键、失焦隐藏、防截屏、会话/电源通知、单实例 |
| `src/vault/windows_store.rs`（lib，仅 Windows） | `KeyStore` 的 Windows 实现：DPAPI 密封文件 + Windows Hello |
| `src/capture/desktop.rs` | Windows 的敏感内容识别、剪贴板序号、敏感复制的排除标记 |
| `src/ui/mobile.rs` 等 | "Touch ID" 文案按系统改称 "Windows Hello" |
| `build.rs` | 图标（资源 1）与清单（Common Controls 6、PerMonitorV2） |

### 4.1 线程

- 宿主线程 `ui`（8 MB 栈，与 NSH 自己的运行器一致）：创建 `AppView`、托盘、快捷键管理器，跑 `GetMessageW` 循环。进程级先做 `SetProcessDpiAwarenessContext(PerMonitorV2)` 和 `OleInitialize`。
- 引擎线程（NSH）、剪贴板监听线程、同步线程不变。
- 其他线程经 `with_host` 访问宿主：闭包入队（`Mutex<Vec<_>>`），再 `PostMessageW(WM_HOST_WAKE)` 唤醒宿主线程执行。消息里不带指针。托盘、菜单、快捷键的事件回调也只入队（`global-hotkey` 的释放事件来自别的线程；三者的回调都是一次性设置，必须在创建托盘和管理器之前设好）。

### 4.2 面板

- 样式走 NSH 自己的窗口属性：`Root::main_window`，`frame = None`、`level = Floating`、`taskbar = false`、不可缩放/最小化/最大化。只在 Windows 宿主里设置，不放进共享的 `root()`。
- 宿主对窗口做 `SetWindowSubclass`，处理：
  - `WM_CLOSE`（Alt+F4）→ 隐藏。否则 NSH 会销毁窗口并结束进程。
  - `WM_ACTIVATE(WA_INACTIVE)`：新前台窗口属于本进程（文件对话框、托盘菜单）则不动；否则按现有的 `services().window_blurred()` 决定 `Hide / Stay / TakeBack`。Windows Hello 提示在别的进程里，由已有的 `SystemPrompts` 覆盖。
  - `WM_WTSSESSION_CHANGE`（锁屏、注销、控制台/远程断开）、`WM_POWERBROADCAST(PBT_APMSUSPEND)` → 锁定密码本。
  - `WM_ENDSESSION` → 退出（注销、关机、安装包升级时由 Restart Manager 触发）。
  - 第二个实例广播的注册消息 → 显示面板。
- 显示：记下当前前台窗口 → 按鼠标所在显示器的工作区和 DPI 计算位置（右侧留 16pt，垂直居中；先移动再定尺寸，避免跨 DPI 时系统按比例二次缩放）→ `view.show()`（先绘制再显示）→ `SetForegroundWindow`。
- 隐藏：`ShowWindow(SW_HIDE)`；隐藏时面板仍是前台窗口的话，把键盘还给记下的窗口，这样"回车复制并收起"后可以直接粘贴。
- Windows 没有"不激活却能接收键盘"的面板，面板显示时必然成为前台窗口。
- 托盘：左键切换，右键菜单（显示 / 退出）。点击托盘会先让面板失焦隐藏，随后的点击事件在 400ms 内忽略；双击的第二下同样忽略。图标用彩色应用图标（任务栏深浅色都可见）。
- 防截屏：`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`，失败（早于 Windows 10 2004）退到 `WDA_MONITOR`。
- 单实例：命名互斥量；第二次启动广播注册消息后退出。第一个实例还在启动、窗口尚未建好时收不到广播，所以第二次启动同时置位一个命名事件，第一个实例在面板就绪后检查它。
- 快捷键：`RegisterHotKey` 只能在宿主线程调用，而且会拒绝已被其他程序占用的组合。设置页保存时等待宿主线程返回结果（最多 2 秒），被拒绝则不保存并提示"该快捷键已被其他程序或系统占用"，原快捷键保持有效。
- `WM_SYSCOMMAND(SC_KEYMENU)` 被吞掉：单按 Alt 或 Alt+空格会进入窗口菜单模式，面板没有菜单可显示，之后的按键会被菜单循环吃掉（默认快捷键里就有 Alt）。
- 交还键盘的对象：每次面板取得键盘前记录当时的前台窗口（包括"面板已显示但没有键盘"时再按快捷键）；由托盘点击唤出时不记录（那时的前台窗口是任务栏），收起后由系统决定。
- 睡眠：除 `WM_POWERBROADCAST` 外还调用 `RegisterSuspendResumeNotification`。使用 Modern Standby 的机器只通知登记过的窗口。

## 5. 密码本

### 5.1 密钥存储

`WindowsKeyStore` 的语义与 `MacKeyStore` 相同：`get` 静默读取任意一项，`get_with_auth` 先过 Windows Hello 再读。因此修改主密码时密钥副本会跟着更新（`rekey` 依赖静默读取），不需要改动共享的密码本代码。

- 每项一个文件 `<数据目录>\keys\<项>.sealed`，内容为 `CryptProtectData` 的输出（`CRYPTPROTECT_UI_FORBIDDEN`，固定附加熵做域隔离），先写临时文件再改名。
- 系统返回的明文缓冲区拷入 `Zeroizing` 后清零再 `LocalFree`。
- 解不开的文件视为不存在（已记日志）。其中数据密钥副本解不开时直接删除（它只是快捷解锁用的副本，主密码仍可解锁），这样"已关闭 Windows Hello"时磁盘上不会留着一份副本；秘密密钥和设备令牌不自动删除，留待系统下次能解开。
- 写入时先 `sync_all` 再改名：改名有日志保护而文件内容没有，断电后可能得到一个空文件。
- 设置页的 Windows Hello 开关在"已开启"时始终显示，即使系统此刻不再提供 Hello（例如 PIN 被移除），否则无法关闭。

### 5.2 Windows Hello

- 可用性：`UserConsentVerifier::CheckAvailabilityAsync` 为 `Available` 或 `DeviceBusy`。只设了 PIN 也算可用；完全没设置 Hello 时设置里不出现该项。结果缓存 10 秒，过期后在后台线程刷新（界面绘制时会反复询问）。
- 提示：优先 `IUserConsentVerifierInterop::RequestVerificationForWindowAsync`（属主为面板，提示显示在面板之上），取不到互操作接口时退回 `RequestVerificationAsync`。在调用方的阻塞线程上等待，120 秒超时后取消。
- **限制（写入 README）**：DPAPI 按 Windows 账户保护而不是按应用保护，同一账户下的其他程序可以不经提示解开 `keys\` 下的文件，包括 Hello 解锁用的数据密钥副本。这弱于 macOS 的钥匙串项，与 Bitwarden 桌面版 CVE-2023-27706 是同一类问题。该功能默认关闭，需主密码开启。
- **后续项**：用 `KeyCredentialManager` 的 Hello 密钥对随机挑战签名、以签名派生包装密钥，使密钥副本只有通过 Hello 才能解开。依赖签名确定性，需要真机验证后再做。

## 6. 剪贴板

- 识别其他应用标记的敏感内容：存在 `ExcludeClipboardContentFromMonitorProcessing` 或 `Clipboard Viewer Ignore` 格式，或 `CanIncludeInClipboardHistory` / `CanUploadToCloudClipboard` 的值为 0（Bitwarden 只设后两者）。打不开剪贴板去读这两个值时按敏感处理。
- 本应用复制的密码：在 `exclude_from_history` 之外再加 `exclude_from_cloud`、`exclude_from_monitoring`（原先只排除了本机历史，不排除云剪贴板）。
- 轮询改为先比 `GetClipboardSequenceNumber`，没变就不读。否则每 500ms 会完整解码一次图片并占住剪贴板。读完再比一次序号：变了就丢弃这次读到的内容，因为"是否敏感"的检查和读取是两次独立的访问，中间剪贴板可能已经换成了别的应用的密码。
- 存在但读不出来的排除格式按敏感处理。

## 7. 界面

- `biometric()` 按系统返回 "Windows Hello"；设置页与开启对话框的文案相应改动。
- 快捷键 `mod+…` 在 Windows 上即 Ctrl，无需改动。
- 文件对话框、打开链接是 NSH 的跨平台接口，Windows 上已有实现。

## 8. 构建与发布

- `scripts/package-windows.ps1`：`cargo build --release` → `dist\ClipboardX-<版本>-windows-x86_64.zip`，再用 Inno Setup 6 编译 `scripts/windows-installer.iss` → `…-setup.exe`，各带 `.sha256`。
- CI：`lint`、`test` 改为 macOS + Windows 矩阵（互不取消）；新增 `windows` 打包任务；`release` 依赖并附带 Windows 的两个包。Windows 任务设置 `AWS_LC_SYS_PREBUILT_NASM=1`（runner 没有 NASM）。
- `.gitattributes` 固定 LF，避免 Windows 检出把编译进应用的文件变成 CRLF。

## 9. 测试

自动（CI，Windows runner）：

- 现有全部测试在 Windows 上通过（含无头 UI 测试、同步端到端）。
- 新增：面板定位计算（多显示器、不同 DPI、左侧副屏、工作区比面板矮）；DPAPI 密封往返、被改动的密文打不开、文件存取与删除；Hello 结果到错误文案的映射；敏感格式数值判定；按系统的文案。

真机手测清单（CI 测不到）：

1. 启动后只有托盘图标，没有任务栏按钮；再次启动只是唤出面板。
2. `Alt+Shift+V` 唤出面板，位置在鼠标所在屏幕右侧、垂直居中；副屏、不同缩放比例下尺寸正确。
3. 唤出后可以直接在搜索框输入；`Esc` 收起；回车复制并收起后，原窗口重新获得焦点，可以直接粘贴。
4. 点击别处面板隐藏；关闭"失焦自动隐藏"后保持显示。
5. 托盘左键切换（面板显示时点托盘是收起而不是闪一下）；双击不抖动；右键菜单"显示""退出"。
6. Alt+F4 只是收起。
7. 设置里改快捷键立即生效；换成被其他程序占用的组合时提示被占用、不保存，原快捷键继续有效，重启后仍是原快捷键。
8. 防截屏开启时，截图工具和屏幕共享里看不到面板；关闭后可见。
9. `Win+L` 锁屏、睡眠后密码本已锁定。
10. 密码本：开启同步后重启应用，不再要求秘密密钥；`keys\` 下出现 `.sealed` 文件。
11. Windows Hello：未设置 Hello 的机器上设置里没有该项；设置了的机器上开启、解锁、取消、超时；提示出现时面板不隐藏，结束后键盘回到面板；修改主密码后仍可用 Hello 解锁。
12. 用 Bitwarden / KeePass 复制密码不进入历史；本应用复制的密码不出现在 `Win+V` 历史里，30 秒后清空。
13. 导出/导入 `.cxvault` 时文件对话框出现期间面板不隐藏。
14. 安装包：安装、开始菜单项、勾选自启后登录即启动、应用运行中覆盖安装（应用被关闭并替换）、卸载。
15. Windows 10 上外观（无圆角）与 Windows 11 上的圆角、阴影。
16. 面板里单按 Alt、Alt+空格后仍能直接在搜索框输入；用 `Alt+Shift+V` 唤出后松开按键，第一个字符没有丢。
17. 一块 100% 缩放、一块 200% 缩放的两台显示器之间来回唤出，面板尺寸正确。
18. 关闭"失焦自动隐藏"：从应用 A 唤出面板，点进应用 B，再按快捷键、回车选一条，键盘回到 B。
19. 应用刚开始启动（例如登录后自启的头一两秒）时再点一次开始菜单里的图标，启动完成后面板自动出现。
20. 免安装 zip 解压后是一个 `ClipboardX` 文件夹。

## 10. 已知风险

- 所有 Win32 代码只经过 CI 编译和单元测试，交互行为未在真机验证，第 9 节清单是合并前的必做项。
- `SetForegroundWindow` 可能被前台锁拒绝（例如别的应用正开着菜单），此时面板显示但没有键盘。
- NSH 在任何一次窗口属性更新时会重写它自己管理的样式位；宿主没有手改这些位，但圆角偏好可能被重置，所以每次显示前重新设置。
- 面板显示期间会出现在 Alt+Tab 中（NSH 主窗口不是工具窗口）。
- 漫游配置文件下 `%APPDATA%` 会漫游，密封的设备令牌会跟着漫游到别的机器（在那里解不开，表现为需要重新加入同步）。
- 单实例只认互斥量的名字：同一会话里占住这个名字的其他进程能让应用启动后立即退出。同账户的进程本来就能结束应用或删除它的文件，所以不另做防护。
- 可执行文件带 `/DEPENDENTLOADFLAG:0x800`，系统库只从系统目录加载；免安装 zip 里应用放在自己的文件夹中。两者都是为了避免应用被解压到下载目录后加载到同目录下同名的 DLL。
- CI 的安装包由 runner 镜像自带的 Inno Setup 编译；镜像里没有时才从 Chocolatey 安装（未固定版本）。

## 11. 审查记录

2026-11-15 由三个只读审查（Win32 宿主运行时逻辑、凭据与剪贴板安全、其他平台回归与 CI/打包）检查了本分支，每条中等及以上的发现另由一个反向核实者确认。没有严重或高危发现。成立并已修复的：被系统拒绝的快捷键仍被保存（中）；另有低严重度的若干项，见 4.2、5.1、第 6 节和上面几条。

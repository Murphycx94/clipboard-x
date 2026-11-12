# 安卓版设计

- 日期：2026-11-12
- 分支：`feat/nsh`
- 前置：`2026-11-09-password-vault-design.md`、`2026-11-10-vault-recovery-design.md`、`2026-11-10-vault-sync-design.md`
- 状态：已确认（第 1 节逐节确认，其余各节按推荐方案定稿，用户要求直接进入开发）

## 1. 目标

在安卓手机上提供 ClipboardX：记录剪贴板历史（只存本机），并提供与桌面端完全互通的密码本（零知识云同步、恢复助记词），支持指纹/面容解锁。

不做（第一版）：

- 剪贴板历史的跨设备同步（下一期单独立项）
- 系统分享菜单收录、快捷设置磁贴、自动填充服务（Autofill）
- 输入法（IME）形态
- Google Play 上架（第一版只出自签名 APK）

## 2. 用户确认的决定

| 问题 | 决定 |
|---|---|
| 核心定位 | 剪贴板历史 + 密码本 |
| 剪贴板历史同步 | 第一版只存本机 |
| 技术路线 | A：同一个 Rust crate 加安卓目标，UI 继续用 NSH；系统能力经运行时加载的 Java 桥（JNI）取得（见 4.4）；先做 spike，走不通再退到"Compose + Rust 核心（UniFFI）" |
| 安卓专属能力 | 只做生物识别解锁密码本 |
| NSH 版本 | 升级到 0.5.0（安卓打包由 nsh CLI 按它自己的版本编译本 crate，桌面与安卓须同版本） |

## 3. 平台限制与对策

Android 10 起，非默认输入法的应用在后台读不到剪贴板。对策：

- **前台读取**：App 每次回到前台（以及冷启动）时读一次剪贴板，内容新就记入历史（沿用按哈希去重、重复条目移到最前）。不在前台期间轮询：Android 12 起，App 每次读取其他应用复制的内容，系统都会提示"已粘贴自剪贴板"，轮询会反复弹出提示。
- 只记录文本：NSH 在安卓上不读写剪贴板图片。图片记录留到以后经 Java 桥实现。
- 用户需要知道这一点：首次打开时在历史为空的占位文案里说明"复制后回到 ClipboardX 即会记录"。

## 4. 架构

同一个 `clipboardx` crate、同一套 `ui`，平台差异用 `cfg(target_os = "android")` 隔离，不新开仓库或分支。

### 4.1 入口

- 安卓没有 `main()`：NSH 安卓宿主（`nsh run/package --target android`）挂载 `clipboardx::root()`（`nsh.json` 已有 `"root": "App"`）。
- `main.rs` 中的启动逻辑（创建数据目录、初始化日志）抽到 `lib.rs` 的 `startup()`，桌面与安卓共用；`App` 组件已负责启动采集与同步线程。
- 移除 `main.rs` 末尾针对非桌面平台的 `compile_error!`，改为 `main.rs` 只在桌面编译（安卓不构建该 bin）。

### 4.2 模块处理

| 模块 | 安卓做法 |
|---|---|
| `db`、`model`、`vault/*`、`cloud`、`telegram`、`crates/sync-proto` | 原样复用 |
| `capture` 读剪贴板 | 拆为 `capture/mod.rs`（哈希、去重 `Recorder`、存储、缩略图、本 App 复制的密码跳过）+ `capture/desktop.rs`（`arboard`，行为不变）+ `capture/android.rs`（NSH 的 `clipboard_string()` / `set_clipboard()`，在 UI 线程调用） |
| `capture` 轮询 | 安卓：不轮询，NSH `lifecycle()` 变为 Active 时读一次；桌面不变 |
| `capture::copy_secret` | 经 Java 桥设置 `ClipDescription.EXTRA_IS_SENSITIVE`；30 秒后内容未变则清空的逻辑保留 |
| `hotkey`、`tray-icon`、`global-hotkey`、`arboard`、`macos` | 依赖移到 `cfg(not(target_os = "android"))`，模块只在桌面编译 |
| `vault/keychain` | 新增 `AndroidKeyStore` 实现现有 `KeyStore` trait（见 4.3） |
| `updater` | 检查到新版本时打开 GitHub Release 页面，由用户下载 APK |
| `services::data_dir()` | App 私有目录（`Context.filesDir`），经 Java 桥取得；失败时按进程名推断 |

### 4.3 密钥存储（AndroidKeyStore）

- `SecretKey`、`DeviceToken`：用 Android Keystore 中一个不需要用户验证的 AES-256-GCM 密钥（`clipboardx.silent`）包装，密文存 App 私有目录。静默读取，对应桌面的"钥匙串静默读取"。
- `DataKey`（生物识别解锁）：用 Keystore 中 `setUserAuthenticationRequired(true)`、仅接受 `BIOMETRIC_STRONG` 的密钥（`clipboardx.biometric`）包装。`get_with_auth` 通过 `BiometricPrompt` + `CryptoObject` 解密，因此只有通过生物识别的那次操作能用该密钥。新增指纹/面容后密钥失效（`setInvalidatedByBiometricEnrollment(true)`），此时删除该项、提示用户重新开启。
- `available()`：`BiometricManager.canAuthenticate(BIOMETRIC_STRONG) == SUCCESS`。
- 只接受属于本密码本的密钥：沿用桌面已有的校验。

### 4.4 Java 桥（`android/java`）

Keystore、BiometricPrompt、敏感剪贴板标记、`filesDir` 只能从 Java 调用。实施时发现 `nsh package` 打出的 APK 只带 NSH 自己的 `classes.dex`，不能加入 App 自己的 Java/Kotlin 类；宿主的 `hostMessage` 扩展点也只在嵌入 `nsh.aar` 时可用。为了继续只用 `nsh package` 构建（不引入 Gradle 工程），改为运行时加载：

- `android/java/com/clipboardx/Bridge.java`：只有静态方法，`filesDir / biometricAvailable / seal / open / deleteKey / copySensitive / clearSensitive`。用 Java 而非 Kotlin，只需 `javac` + `d8`。
- `build.rs`：仅在安卓目标下，用 SDK 的 `android.jar`、`core-lambda-stubs.jar` 和 `d8` 把它编译为 dex，`include_bytes!` 进 `libnsh.so`。
- `src/android/jvm.rs`：经 `JNI_GetCreatedJavaVMs`（libnativehelper，Android 12 起公开；更早版本退回进程符号）取得 JVM，用 `InMemoryDexClassLoader` 加载 dex 并缓存 `Bridge` 类；每次调用在独立的 local frame 中进行，Java 异常转为错误消息；传入和取回的字节数组用后清零，Rust 侧用 `Zeroizing` 持有。
- `src/vault/device_store.rs`：`DeviceStore<S: Sealer>` 实现 `KeyStore`，每项一个密封文件（`<数据目录>/keys/<项>.sealed`，先写临时文件再改名）；`Sealer` 抽象系统密钥，主机上用假实现做单元测试。
- 敏感复制不阻塞：`copySensitive(text, id)` 投递到主线程，记下剪贴板的时间戳；30 秒后 `clearSensitive(id)` 仅当这仍是最后一次敏感复制、且剪贴板时间戳未变时清空。App 在后台读不到剪贴板时直接清空（宁可丢一次复制，也不把密码留在剪贴板）。
- 生物识别密钥需要 Android 11（API 30，`setUserAuthenticationParameters` 与 `BIOMETRIC_STRONG`）；更低版本 `available()` 为假，静默密钥不受影响。
- **未解决：** `BiometricPrompt` 需要 `android.permission.USE_BIOMETRIC`，而 NSH 0.5.0 生成的清单不含它，`nsh.json` 的 `android` 也只支持 `targetSdk`、`usesCleartextTraffic`。没有该权限时 `biometricAvailable()` 返回假，设置里不出现"指纹/面容解锁"，其余功能不受影响。在模拟器上给 APK 清单补上该权限后，开启、解锁、取消、指纹变化后失效均已验证。决定：请 NSH 支持在 `nsh.json` 里声明安卓权限，不在打包后改清单重签；支持之后在 `nsh.json` 加上该权限即可启用，代码不用改。

## 5. 界面

现有 UI 为 420×600 的桌面面板，依赖悬停和键盘。安卓上按屏幕铺满，交互改动如下：

- **导航**：底部 `TabBar`（`nsh_core::mobile`），四个标签：历史、收藏、密码本、设置。历史页内含归档（按日期折叠分组），替代桌面顶栏的标签切换与归档入口。桌面布局不变。
- **条目操作**：桌面"悬停显示 备注/收藏/删除"改为点按复制、长按弹出操作菜单（备注、收藏、删除）。由 `AppState` 新增的平台标志 `is_touch` 选择。
- **键盘快捷键**：安卓上不绑定（`keys::bind` 在安卓跳过）。
- **搜索框**：保留在历史与收藏页顶部；打开 App 时不自动聚焦（避免弹出软键盘）。
- **密码本**：页面结构不变；解锁页在 `available()` 为真且已开启时自动弹出生物识别。
- **设置**：隐藏桌面专属项（快捷键、失焦自动隐藏、收起面板时锁定）；"系统睡眠或锁屏时锁定"在安卓上对应"App 进入后台超过所选时长后锁定"。

## 6. 数据流

- 剪贴板：`lifecycle()` 变为 Active → `capture::read_foreground()` → `Services::record_clipboard`（`Recorder` 去重） → `db` → `services.notify_changed()` → UI 刷新（与桌面同一路径）。
- 密码本、同步：与桌面完全相同（`VaultService` + `cloud` 工作线程 + WebSocket）。App 进入后台时 NSH 宿主可能被系统暂停，同步线程停止无妨；回到前台且已解锁时立即同步一次（复用现有"解锁后立即同步"逻辑）。
- 网络规则不变：只允许以明文 http 访问 Tailscale 地址和本机。手机上需安装并连接 Tailscale App；`AndroidManifest` 的网络安全配置允许 `100.64.0.0/10` 和 `*.ts.net` 的明文流量（Rust 侧的地址校验仍然生效）。

## 7. 错误处理

- 读剪贴板失败（无内容、无权限）：静默跳过本次，写日志，不弹提示。
- Keystore/生物识别失败：用户取消时回到主密码输入；密钥失效时删除 `DataKey`、关闭生物识别开关并提示"生物识别信息已变化，请重新开启"。
- JNI 调用异常：转为 `Err(String)`，沿用现有 UI 的错误提示；不得在错误信息中包含密钥或明文。
- 同步错误沿用现有状态显示。

## 8. 构建与发布

- 本机工具：JDK 17、Android NDK、`aarch64-linux-android` / `armv7-linux-androideabi` Rust 目标、一个 arm64 模拟器镜像。
- 构建：`nsh package --target android`（release keystore 不进仓库，路径与密码从环境变量读取）。
- CI：新增 Android 打包任务（Ubuntu runner），产出 APK 作为 workflow artifact；发布时附到 GitHub Release。

## 9. 测试

- 现有 `cargo test --workspace`（桌面）必须继续通过；`cargo clippy` 对安卓目标（`--target aarch64-linux-android`）也要无警告。
- 新增单元测试：`capture::Recorder` 与 `Services::record_clipboard`（去重、本 App 复制的密码不记录、记录后通知 UI）；触屏交互（长按菜单）的 headless UI 测试（`nsh-testing`）。
- `AndroidKeyStore` 无法在宿主机测试：在模拟器上手工验证，清单写入实施计划。
- 端到端：模拟器上与本地起的同步服务器联调（桌面端改一条 → 手机端出现，反之亦然）。

## 10. 实施顺序

0. **Spike**（不通过就停下重新评估路线）：装好工具链，在 arm64 模拟器上跑起本 crate；验证 `rusqlite`（bundled）、`argon2`、`reqwest`/`tungstenite`（rustls）能交叉编译，界面能显示。
1. 平台隔离：依赖与模块的 `cfg` 拆分、`startup()`、`capture` 拆分，安卓目标编译通过、桌面测试全绿。
2. 前台剪贴板采集。
3. 移动布局：TabBar、长按菜单、设置项裁剪。
4. Java 桥与 `AndroidKeyStore`、生物识别解锁、敏感复制。
5. 同步联调、打包、CI。

# 密码本（Password Vault）设计

- 日期：2026-11-09
- 分支：`feat/nsh`（NSH 纯 Rust 版 ClipboardX）
- 状态：已确认，待编写实现计划

## 1. 目标

在 ClipboardX 中加入密码本：保存账号、密码、网址、两步验证码（TOTP）密钥和备注，支持搜索和一键复制。要求如下：

- 数据库中没有任何密码本明文，名称也加密。
- 用主密码解锁，可选用 Touch ID 快速解锁。
- 复制出去的密码和验证码不会被 ClipboardX 自己（或其他遵守约定的剪贴板工具）记进历史，30 秒后自动从剪贴板清除。
- 可以导出加密备份，在其他机器上导入。

不做：二维码扫描（TOTP 只支持粘贴）、浏览器自动填充、云同步、密码泄露检测。

## 2. 用户确认的决定

| 问题 | 决定 |
|---|---|
| 解锁方式 | 主密码 + Touch ID 快速解锁 |
| 功能范围 | 基本字段，加上密码生成器、网址字段、TOTP、加密导出/导入 |
| 自动锁定 | 在设置里选择：空闲 1/5/15/30 分钟（默认 5），以及"面板收起时锁定"开关（默认开）；系统睡眠或锁屏时一律锁定 |
| 存储方案 | 方案 A：在现有 SQLite 中逐条加密（不选 SQLCipher 整库加密，也不选每条存进钥匙串） |

## 3. 安全架构

### 3.1 密钥体系

```
主密码 ──Argon2id(salt, m=64MiB, t=3, p=1)──▶ KEK（密钥加密密钥）
                                                │ XChaCha20-Poly1305 包装
随机生成 256 位密码本密钥 DEK ◀───────────────────┘   （wrapped_dek 存进 vault_meta）
        │
        ├─ XChaCha20-Poly1305(nonce 随机 24 字节, AAD = 条目 id) ─▶ 每条记录的密文
        └─（开启 Touch ID 时）另存一份到 macOS 钥匙串
```

- 用 DEK 加密条目，KEK 只用来包装 DEK。修改主密码时只需重新包装 DEK，条目不用重新加密。
- 条目 id 作为附加认证数据（AAD）：把一条记录的密文挪到另一条下，解密会失败。
- `vault_meta` 记录格式版本和 Argon2 参数，以后可以提高派生强度并逐步迁移。
- 解锁成功的判断依据是 DEK 能被解包（AEAD 校验通过），不另存密码哈希。
- 主密码不存储，无法找回。设置主密码时界面会明确提示这一点。

### 3.2 内存中的数据

- DEK、KEK 和主密码输入都放在 `zeroize::Zeroizing` 中，离开作用域时清零。
- 解锁期间，已解密的条目只放在 `session` 里；锁定时整体丢弃并清零。
- 已知限制：界面显示密码时（点了"显示"之后），明文会以字符串形式进入 NSH 的 signal 和原生控件，这部分无法保证清零。所以密码默认显示为圆点，列表里不出现密码明文。

### 3.3 Touch ID

- 开启时，把 DEK 存成一个只允许本应用访问的钥匙串通用密码项（service `com.clipboardx.app.vault`）。
- 解锁时先用 `LAContext.evaluatePolicy(.deviceOwnerAuthentication)` 验证（Touch ID，失败可退回系统密码），通过后再读取钥匙串。
- 已知限制：要做到"只有 Touch ID 才能读出这个钥匙串项"（数据保护钥匙串 + `biometryCurrentSet` 访问控制），应用必须用 Developer ID 签名并声明相应权限。当前打包脚本是 ad-hoc 签名，所以先采用上面的"先验证、再读取"方式。它能防止别人在你的 Mac 前直接打开密码本，但对已经在你用户账户里运行的恶意程序防护较弱。换成 Developer ID 签名后再升级。
- 关闭 Touch ID、修改主密码或重置密码本时，删除或更新这个钥匙串项。
- 钥匙串或 Touch ID 不可用时，自动退回主密码。

### 3.4 防暴力破解

- Argon2id 的参数本身就让每次尝试代价很高。
- 连续输错主密码时，等待时间从第 3 次起逐次翻倍：1、2、4……最多 60 秒。失败次数存进数据库，重启应用不会清零；解锁成功后清零。

### 3.5 防泄漏

1. **复制时打标记**：复制账号、密码或验证码时使用 `arboard` 的 `exclude_from_history()`。它在 macOS 上设置 `org.nspasteboard.ConcealedType`，在 Windows 和 Linux 上也有对应的标记。
2. **监听时跳过**：剪贴板监听在 macOS 上检查当前剪贴板是否带 `org.nspasteboard.ConcealedType` 或 `org.nspasteboard.TransientType`，带有就不记录。这同时修复了现有问题：从 1Password 等工具复制的密码目前会被明文记进历史。另外，监听线程还会跳过 ClipboardX 自己刚复制出去的密文所对应的哈希，作为第二道防线。Windows 和 Linux 上的检测留作后续。
3. **自动清除**：复制 30 秒后，如果剪贴板内容的哈希仍然等于当时复制的那一份，就把剪贴板清空。
4. **不外发**：Telegram 同步、日志、`Debug` 输出都不包含密码本内容，相关类型的 `Debug` 实现会脱敏。
5. **界面**：密码默认显示为圆点；锁定时，任何弹窗里的密码本数据都会一并关闭。

## 4. 数据

### 4.1 新增的表（与现有 `clipboard_items`、`settings` 并存，互不影响）

```sql
CREATE TABLE IF NOT EXISTS vault_meta (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    version      INTEGER NOT NULL,
    kdf_params   TEXT NOT NULL,       -- JSON: {"m":65536,"t":3,"p":1}
    salt         BLOB NOT NULL,       -- 16 字节
    wrapped_dek  BLOB NOT NULL,       -- nonce(24) || 密文
    failed_tries INTEGER NOT NULL DEFAULT 0,
    locked_until TEXT                 -- 失败后的等待截止时间
);
CREATE TABLE IF NOT EXISTS vault_entries (
    id         TEXT PRIMARY KEY,      -- UUID v4，合并导入时用它判断是否同一条
    nonce      BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    updated_at TEXT NOT NULL          -- 明文时间戳，只用于合并导入时比较新旧
);
```

### 4.2 条目明文（加密前序列化为 JSON）

```rust
struct VaultEntry {
    title: String,
    username: String,
    password: String,
    url: String,
    totp: Option<String>,   // otpauth:// URI 或 Base32 密钥
    note: String,
    created_at: String,
    updated_at: String,
}
```

### 4.3 新增设置项（`settings` 表）

`vault_lock_minutes`（默认 `5`）、`vault_lock_on_hide`（默认 `true`）、`vault_touch_id`（默认 `false`）。

## 5. 功能

### 5.1 界面

- 顶栏新增钥匙图标，进入"密码本"页面。
- **未设置**：设置主密码（输入两遍，少于 10 位拒绝，显示强度提示），然后询问是否开启 Touch ID。
- **锁定**：主密码输入框、"用 Touch ID 解锁"按钮（已开启时进入页面会自动弹出验证），输错后显示等待倒计时。
- **解锁**：
  - 顶部是搜索框（匹配名称、账号、网址）和"新建"按钮。
  - 每行显示名称、账号和网址域名。右侧按钮可以复制账号、复制密码；有 TOTP 时多一个按钮，显示当前验证码和剩余秒数的倒计时环，点击复制验证码。
  - 点击一行打开详情和编辑弹窗：所有字段可编辑；密码可显示或隐藏，旁边有"生成"按钮；网址可以在浏览器中打开；删除需要二次确认。
- **键盘**：↑↓ 选择，`Enter` 复制密码，`⌘C` 复制账号，复制后面板不收起。`Esc` 的行为与现有页面一致。

### 5.2 设置（新增"密码本"分组）

- 自动锁定时间：1、5、15、30 分钟。
- 面板收起时立即锁定：开关。
- Touch ID 解锁：开关。开启时需要先输入主密码。
- 修改主密码：需要输入旧密码。
- 导出加密备份、导入备份。

### 5.3 TOTP

- 输入方式：`otpauth://totp/...` 链接，或 Base32 密钥（使用默认值 SHA1、6 位、30 秒）。
- 支持 SHA1、SHA256、SHA512，6 位或 8 位，自定义周期。
- 使用 `totp-rs`；保存前校验，无效时提示。

### 5.4 密码生成器

- 使用操作系统的安全随机数（`rand` 的 `OsRng`）。
- 长度 8 到 64，默认 20；可分别选择大写、小写、数字、符号；可排除易混淆字符 `0 O 1 l I`。
- 每个选中的字符类别至少出现一次，用无偏差的方式抽取字符。

### 5.5 加密导出与导入（`.cxvault`）

```json
{
  "format": "clipboardx-vault",
  "version": 1,
  "kdf": { "alg": "argon2id", "m": 65536, "t": 3, "p": 1, "salt": "<base64>" },
  "nonce": "<base64>",
  "ciphertext": "<base64>"   // 所有条目（含 id 和 updated_at）的 JSON 数组加密而成
}
```

- 导出：设置备份密码（默认与主密码相同，可另设），使用新的盐和随机数，通过系统的"保存"对话框选择路径。
- 导入：通过"打开"对话框选择文件，输入备份密码，再选"合并"（同 id 的条目保留 `updated_at` 较新的一份）或"替换"（替换前二次确认）。
- 格式、版本不符或密码错误时给出明确提示，不修改现有数据。

## 6. 代码结构

```
src/vault/
├── mod.rs        # 对外接口：VaultService（设置、解锁、锁定、增删改查、导出导入）
├── crypto.rs     # Argon2id 派生、AEAD 加解密、DEK 包装；纯函数
├── store.rs      # vault_meta / vault_entries 读写，只处理密文
├── session.rs    # 解锁状态、内存中的 DEK 和条目、空闲计时、失败退避
├── totp.rs       # 解析 otpauth 链接或 Base32，计算当前验证码和剩余秒数
├── generator.rs  # 密码生成
├── backup.rs     # .cxvault 编码与解码
└── keychain.rs   # 仅 macOS：钥匙串存取 + LAContext 验证；其他平台为返回"不支持"的空实现
src/ui/vault/
├── mod.rs        # 页面：按状态显示未设置、锁定或列表
├── lock.rs       # 设置主密码、解锁
├── list.rs       # 列表、搜索、复制按钮、TOTP 倒计时
└── editor.rs     # 编辑弹窗、生成器
```

需要改动的现有文件：

- `capture.rs`：复制时打隐藏标记、监听时跳过带标记的内容、30 秒后清除。
- `services.rs`：持有 `VaultService`，提供 `lock_vault()`。
- `macos.rs`：面板收起、系统睡眠、锁屏（`NSWorkspace` 通知）时调用锁定。
- `ui/header.rs`：新增入口；`ui/settings.rs`：新增"密码本"分组。
- `ui/state.rs`：新增 `Page::Vault`。
- `ui/keys.rs`：在密码本页面上把 Enter 和 ⌘C 用于密码本。

新增依赖：`argon2`、`chacha20poly1305`、`zeroize`、`rand`、`totp-rs`、`uuid`；macOS 上另加 `security-framework`、`objc2-local-authentication`、`block2`。

## 7. 错误处理

| 情况 | 处理 |
|---|---|
| 主密码错误 | 只提示"密码错误"，累加失败次数并按 3.4 节等待 |
| 某条记录解密失败（被篡改或损坏） | 列表里标记为"已损坏"，可以删除，其余条目正常使用 |
| `vault_meta` 损坏 | 提示无法打开密码本，可从备份导入或重置（重置会删除全部条目，需输入"重置"二字确认） |
| 钥匙串或 Touch ID 不可用、被取消 | 退回主密码输入，不报错 |
| 导入文件无效或密码错误 | 提示原因，不修改数据 |
| 无效的 TOTP 密钥 | 保存前提示，不允许保存 |

所有错误信息都不包含密码本内容。

## 8. 测试

- **crypto**：加密后能正确解密；改动密文或 nonce 中的任一字节、用错 KEK、对调条目 id 都会解密失败；重新包装后能用新 KEK 解开同一个 DEK。
- **store / service**：设置、锁定、解锁、增删改查完整流程；修改主密码后旧数据可读；数据库原始字节中搜索不到条目的任何明文字段；失败退避的时间计算正确，且重启后仍然有效。
- **totp**：RFC 6238 附录 B 的测试向量（SHA1、SHA256、SHA512）；otpauth 链接解析；无效输入会被拒绝。
- **generator**：长度正确；每个选中的类别都出现；排除易混淆字符；大量抽样确认字符分布大致均匀。
- **backup**：导出再导入能完整还原；合并时保留较新的一份；备份密码错误或文件被篡改时报错。
- **capture**：带隐藏标记的剪贴板内容不会被记录（判断标记的函数单独测试）；30 秒清除只在内容未变时执行。
- **无头 UI 测试**（`tests/vault_ui.rs`）：设置主密码、新建条目、搜索、锁定、输错密码、解锁、编辑、删除。测试只用主密码，不涉及 Touch ID。
- **需要手动验证**：Touch ID 解锁；系统睡眠和锁屏时锁定；从 1Password 复制的内容不出现在历史里；复制 30 秒后剪贴板被清空。

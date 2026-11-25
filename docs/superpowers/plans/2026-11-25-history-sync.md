# 复制历史同步 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 开启了云同步的设备之间同步复制历史的文本条目（新复制、复制时间、收藏、备注、删除），服务器只存密文。设计见 `docs/superpowers/specs/2026-11-25-history-sync-design.md`。

**Architecture:** 服务器多一张只存每条最新密文的 `history` 表和两个接口。客户端在 `Database` 改动条目时往 `history_sync` 记账表记一笔；后台同步线程在每轮密码本同步之后拉取、合并（纯函数，满足交换律、结合律、幂等）、写回本机，再上传待上传的行。密钥由密码本数据密钥经 HMAC 派生，所以只在解锁时同步。

**Tech Stack:** Rust 2024，rusqlite，axum（服务器），reqwest blocking（客户端），XChaCha20-Poly1305 与 HMAC-SHA256（`hmac 0.13`，已在 Cargo.lock 里），NSH UI。

---

## 文件

| 文件 | 职责 |
|---|---|
| `crates/sync-proto/src/history.rs`（新） | 线上类型 `HistoryUp/HistoryPush/HistoryPushed/HistoryDown/HistoryPage`、上限常量、`valid_id` |
| `server/src/store/history.rs`（新） | `Store` 的历史表：建表、上传、分页拉取、清空 |
| `server/src/history.rs`（新） | 两个接口的处理函数 |
| `server/src/store.rs`、`server/src/lib.rs` | 挂上新模块；头部变化时清空历史；路由 |
| `src/history_sync/item.rs`（新） | `ItemState`、合并、存活判断（纯） |
| `src/history_sync/seal.rs`（新） | `HistoryKeys`：子密钥派生、id、加密与校验 |
| `src/history_sync/ledger.rs`（新） | 记账表的 SQL：记账钩子、开启与关闭、应用远端、待上传、进度 |
| `src/history_sync/mod.rs`（新） | 一轮同步 `sync_once` |
| `src/cloud/history.rs`（新） | `Client::history`、`Client::push_history` |
| `src/db.rs` | 记账开关；改动条目的方法调用记账钩子 |
| `src/vault/mod.rs`、`src/vault/sync.rs` | `history_keys()`；`stop_sync` 一并关闭历史同步 |
| `src/cloud.rs` | 后台线程在密码本同步后跑历史同步；`Status.history_error` |
| `src/services.rs` | 开关、把数据库交给同步线程 |
| `src/ui/vault/cloud.rs`、`src/ui/settings.rs`、`src/ui/state.rs` | 「同步复制历史」开关行 |
| `tests/history_sync_e2e.rs`（新）、`server/tests/api.rs`、`tests/sync_ui.rs` | 测试 |
| `README.md`、`server/deploy/README.md` | 文档 |

## Task 1：协议类型

- [ ] 在 `crates/sync-proto/src/history.rs` 定义：

```rust
pub const MAX_CIPHERTEXT: usize = 2 * 1024 * 1024;
pub const PAGE_BYTES: usize = 4 * 1024 * 1024;
pub struct HistoryUp { pub id: String, pub nonce: Bytes, pub ciphertext: Bytes }
pub struct HistoryPush { pub header_version: u64, pub records: Vec<HistoryUp> }
pub struct HistoryPushed { pub seq: u64 }
pub struct HistoryDown { pub id: String, pub nonce: Bytes, pub ciphertext: Bytes, pub seq: u64 }
pub struct HistoryPage { pub records: Vec<HistoryDown>, pub seq: u64, pub more: bool }
pub fn valid_id(id: &str) -> bool // 64 位小写十六进制
```

- [ ] 测试 `valid_id`；`cargo test -p clipboardx-sync-proto`；提交 `feat: 协议加入复制历史的类型`。

## Task 2：服务器存储

- [ ] `server/src/store/history.rs`：`HISTORY_SCHEMA`（`history` 表、`history_seq` 计数器）、`Store::push_history(&mut self, &[HistoryUp]) -> Result<u64>`、`Store::history_page(&self, since, budget) -> Result<HistoryPage>`、`Store::clear_history(&Transaction)`。
- [ ] `update_vault` 在头部换版本时同一事务里清空历史。
- [ ] 先写单元测试：上传分配递增序号、同 id 只留最新、分页（`more`、至少一条）、换头部清空；再实现；`cargo test -p clipboardx-sync-server`；提交。

## Task 3：服务器接口

- [ ] `server/src/history.rs`：`GET /v1/history?since&header`、`POST /v1/history`；头部版本不符 409；id、nonce（24 字节）、密文（≤ `MAX_CIPHERTEXT`）不合格 400；上传后 `notify`。
- [ ] `server/tests/api.rs` 先写：未登录 401、往返、409、400、分页、事件推送；实现；测试；提交。

## Task 4：合并与加密（客户端，纯）

- [ ] `src/history_sync/item.rs`：`ItemState { hash, text: Option<String>, copied_at, meta_at, favorite, note, deleted_at }`，`alive()`、`merge()`、`deleted(at)`。测试：交换律、结合律、幂等（一组状态两两、三三组合）、删除后再复制、删除后收藏、备注同时修改的确定性。
- [ ] `src/history_sync/seal.rs`：`HistoryKeys::derive(&SecretKey)`、`id_of(hash)`、`seal(&ItemState) -> HistoryUp`、`open(&HistoryDown) -> Result<ItemState, VaultError>`（不符返回 `Tampered`）。测试：往返、换 id、改 hash、文本与 hash 不符、不同数据密钥。
- [ ] `Cargo.toml` 加 `hmac = "0.13.0"`；`cargo test --lib history_sync`；提交。

## Task 5：本机记账

- [ ] `src/history_sync/ledger.rs`：`SCHEMA`、`MAX_TEXT_BYTES = 256 * 1024`、钩子 `note_copied/note_meta/note_deleted/note_cleared/forget_expired`、`enable/disable/is_enabled`、`local_item`、`apply_remote`、`pending_batch`、`mark_pushed`、`prepare_round(header)`、`set_seq`、时间换算 `ms_of_local/local_of_ms`。
- [ ] `src/db.rs`：`history_tracking: AtomicBool`，`set_history_sync(on)`、`history_sync_enabled()`；`insert_text_at`、`touch_item`、`toggle_favorite`、`update_note`、`delete_item`、`clear_history`、`cleanup_expired` 调用钩子（同一把锁、同一事务）。
- [ ] 先写测试（在 `ledger.rs`）：每个方法都记账、图片和超长文本不记、关闭时不记、到期不留删除标记、开启时的初始记账、`apply_remote` 的新增/更新/删除/保留期/待上传、`mark_pushed` 不吞掉上传期间的改动；实现；`cargo test --lib`；提交。

## Task 6：一轮同步与客户端接口

- [ ] `src/cloud/history.rs`：`Client::history(since, header) -> Result<Option<HistoryPage>>`（409 → `None`，404 → 需要升级服务器）、`Client::push_history(&HistoryPush) -> Result<Option<HistoryPushed>>`。
- [ ] `src/history_sync/mod.rs`：`sync_once(vault, db, client) -> Result<HistoryReport, VaultError>`，步骤按设计 7 节。
- [ ] `VaultService::history_keys()`；`stop_sync` 关闭历史同步。
- [ ] `tests/history_sync_e2e.rs` 先写：复制、收藏、备注、删除、清除历史在两台设备间同步；未开启的设备不收；修改主密码后重新上传；旧服务器（无接口）报需要升级。实现；`cargo test --test history_sync_e2e`；提交。

## Task 7：后台线程、开关与界面

- [ ] `cloud::Worker::start` 多接一个数据库；`sync_loop` 在密码本同步后跑历史同步，`Status.history_error`，有变化时刷新。
- [ ] `Services::set_history_sync(on)`；`AppState.history_sync_on`；`cloud_section` 加开关行（描述行显示错误）。
- [ ] `tests/sync_ui.rs` 加开关测试；`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check`；提交。

## Task 8：文档、版本与验证

- [ ] README「云同步」加复制历史一节；部署说明写明服务器要先升级。
- [ ] 版本号 0.5.0（`Cargo.toml`、`nsh.json`、`Cargo.lock`）。
- [ ] 推分支让 CI 跑 Windows；本机跑一台真实服务器加两个数据目录做一次手动验证；安卓在模拟器上验证开关和同步。

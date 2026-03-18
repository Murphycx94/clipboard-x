# ClipboardX

一款跨平台（macOS / Windows）桌面级剪贴板历史管理工具，支持文本与图片的历史记录、快捷键唤出、收藏备注等功能。

---

## 功能需求

1. **全量历史记录** — 自动捕获所有复制内容，包括文本与图片
2. **快捷键唤出** — 可自定义全局快捷键，唤出当日历史面板，内容按时间倒序排列
3. **历史归档** — 非当日内容按日期分组收纳，支持浏览与管理
4. **收藏 & 备注** — 可将任意历史条目加入收藏并添加备注，收藏内容在独立 Tab 中展示
5. **快捷复制** — 所有历史条目均可一键复制回剪贴板

---

## 技术栈

### 整体架构

```
ClipboardX
├── 后端：Rust（via Tauri）
└── 前端：React + TypeScript
```

选择 **Tauri** 而非 Electron 的原因：

| 对比项     | Tauri          | Electron        |
|------------|----------------|-----------------|
| 包体大小   | ~3–10 MB       | ~80–150 MB      |
| 内存占用   | 低（Rust 后端）| 高（Node + Chromium）|
| 系统集成   | 优秀           | 良好            |
| 跨平台支持 | Mac / Win / Linux | Mac / Win / Linux |

### 依赖库

#### Rust 后端

| 库 | 用途 |
|----|------|
| `tauri` | 桌面应用框架，系统托盘、窗口管理 |
| `arboard` | 剪贴板读写（文本 + 图片） |
| `rusqlite` | 本地 SQLite 数据库 |
| `global-hotkey` | 注册全局快捷键 |
| `image` | 图片编码 / 解码处理 |
| `serde` / `serde_json` | 数据序列化，前后端通信 |

#### React 前端

| 库 | 用途 |
|----|------|
| `react` + `typescript` | UI 框架 |
| `tailwindcss` | 样式 |
| `zustand` | 全局状态管理 |
| `@tanstack/react-query` | 异步数据获取与缓存 |
| `lucide-react` | 图标库 |

---

## 数据库设计

使用 SQLite，数据库文件存储于系统应用数据目录（`AppData` / `Application Support`）。

### 表：`clipboard_items`

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | INTEGER PK | 自增主键 |
| `content_type` | TEXT | `text` 或 `image` |
| `text_content` | TEXT | 文本内容（可为空） |
| `image_data` | BLOB | 图片二进制（可为空） |
| `image_thumbnail` | BLOB | 图片缩略图（用于列表展示） |
| `created_at` | DATETIME | 复制时间戳 |
| `is_favorite` | BOOLEAN | 是否收藏 |
| `note` | TEXT | 收藏备注 |
| `hash` | TEXT | 内容哈希（用于去重） |

---

## 项目结构

```
ClipboardX/
├── src-tauri/                  # Rust 后端
│   ├── src/
│   │   ├── main.rs             # 程序入口，系统托盘，窗口初始化
│   │   ├── clipboard.rs        # 剪贴板监听与内容捕获
│   │   ├── database.rs         # SQLite CRUD 操作
│   │   ├── hotkey.rs           # 全局快捷键注册与管理
│   │   └── commands.rs         # Tauri IPC 命令（供前端调用）
│   ├── Cargo.toml
│   └── tauri.conf.json
│
├── src/                        # React 前端
│   ├── components/
│   │   ├── ClipboardItem.tsx   # 单条历史记录组件
│   │   ├── HistoryList.tsx     # 今日历史列表
│   │   ├── FavoriteList.tsx    # 收藏列表
│   │   ├── DateGroup.tsx       # 按日期分组的历史归档
│   │   └── SettingsPanel.tsx   # 快捷键等设置面板
│   ├── pages/
│   │   ├── MainPanel.tsx       # 主面板（含 Today / Favorites 两个 Tab）
│   │   └── ArchivePage.tsx     # 历史归档页面
│   ├── store/
│   │   └── useClipboardStore.ts # Zustand 状态管理
│   ├── hooks/
│   │   └── useClipboardData.ts # React Query 数据 hooks
│   └── App.tsx
│
├── README.md
└── package.json
```

---

## UI 交互设计

### 主面板（快捷键唤出）

```
┌─────────────────────────────────────┐
│  ClipboardX          [设置] [归档]  │
├─────────────────────────────────────┤
│  [ 今日历史 ]  [ 收藏夹 ]           │  ← Tab 切换
├─────────────────────────────────────┤
│  🔍 搜索...                         │
├─────────────────────────────────────┤
│  14:32  "npm install tailwindcss"   │  ← 最新在上
│  14:10  [图片] screenshot.png       │
│  13:45  "Hello, World!"             │
│  ...                                │
└─────────────────────────────────────┘
```

### 收藏夹 Tab

```
┌─────────────────────────────────────┐
│  [ 今日历史 ]  [ 收藏夹 ]           │
├─────────────────────────────────────┤
│  ★  "API_KEY=xxx..."                │
│     备注：生产环境密钥               │
│  ★  [图片] design-mockup.png        │
│     备注：最终版设计稿               │
└─────────────────────────────────────┘
```

### 历史归档页

```
┌─────────────────────────────────────┐
│  ← 返回    历史归档                  │
├─────────────────────────────────────┤
│  ▼ 2026-03-17（12 条）              │
│     "git commit -m 'fix bug'"       │
│     [图片] ...                      │
│  ▶ 2026-03-16（8 条）               │
│  ▶ 2026-03-15（20 条）              │
└─────────────────────────────────────┘
```

---

## 核心功能实现思路

### 剪贴板监听
在 Rust 后端起一个独立线程，每隔 500ms 轮询剪贴板内容，与上一次内容做哈希对比，变更时写入数据库并去重。

### 全局快捷键
使用 `global-hotkey` 注册系统级快捷键。用户在设置面板中自定义，快捷键配置持久化存储于 SQLite `settings` 表，应用启动时自动注册。

### 图片处理
`arboard` 读取图片为 RGBA 原始像素数据，使用 `image` crate 编码为 PNG 存入数据库 BLOB，同时生成 64×64 缩略图用于列表展示，避免加载大图影响性能。

### 前后端通信
通过 Tauri IPC（`invoke`）调用 Rust 命令，主要接口：

```typescript
// 获取今日历史
invoke('get_today_items') => ClipboardItem[]

// 获取历史归档（按日期分组）
invoke('get_archive_items') => DateGroup[]

// 切换收藏状态
invoke('toggle_favorite', { id, note }) => void

// 复制条目回剪贴板
invoke('copy_to_clipboard', { id }) => void

// 更新快捷键设置
invoke('update_hotkey', { hotkey }) => void
```

---

## 开发路线图

- [ ] **Phase 1** — 项目初始化，Tauri + React 脚手架，SQLite 数据库 schema
- [ ] **Phase 2** — 剪贴板监听（文本 + 图片），历史写入与去重
- [ ] **Phase 3** — 主面板 UI，今日历史列表，快捷键唤出
- [ ] **Phase 4** — 收藏功能，备注编辑，收藏 Tab
- [ ] **Phase 5** — 历史归档页面，日期分组浏览
- [ ] **Phase 6** — 设置面板，自定义快捷键，搜索功能
- [ ] **Phase 7** — 打包发布（macOS `.dmg` / Windows `.msi`）

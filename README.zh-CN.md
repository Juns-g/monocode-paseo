# Monocode Paseo（实验版）

[English](README.md) | **简体中文**

基于 [MonoCode](https://github.com/hardbeat920/monocode) 的原生 GPUI 客户端（同步到 `main` (`09d04436`, 2026-11-12)），把后端换成本机的 [Paseo](https://github.com/getpaseo/paseo) daemon。界面沿用原版 MonoCode 原生 Shell；对话、执行和历史都留在 Paseo 里，Paseo 各端和本客户端看到的是同一批对话，可以互相接着聊。

> ⚠️ **个人实验版本**：只在作者自己的 macOS（Apple Silicon）+ Paseo 0.17.4 上用过，不保证效果、兼容性或稳定性，不提供支持。需要正式体验请用原版 MonoCode。原版说明见 [README.upstream.md](README.upstream.md)。
<img width="2600" height="1784" alt="image" src="https://github.com/user-attachments/assets/d7802cf2-7a4e-42cf-bf43-23f9fbcb4a16" />

## 为什么做这个组合（架构取舍）

原版 MonoCode Native 与官方 Paseo 刚好各赢一半。本 fork 通过内置的轻量 Node Sidecar（`paseo-host.mjs` / `host-adapter.mjs`）把 MonoCode Host RPC 翻译到官方 `@getpaseo/client` SDK，实现 **MonoCode GPUI 原生壳 × Paseo Daemon 编排底座** 的组合：

| 维度 | 原版 MonoCode Native | 官方 Paseo | 本 fork（`monocode-paseo`） |
| --- | --- | --- | --- |
| **页面与渲染** | Rust + GPUI + Metal（低内存、任意递归分屏、内置 `alacritty_terminal` / `tree-sitter`） | Expo / React Native + Electron 套壳（跨全端且有 UI 插件生态，但桌面壳较重） | **继承 MonoCode GPUI 原生壳** + 针对多 Agent 的侧栏、Diff 批注与额度交互增强 |
| **运行时与编排** | 本地单进程（`monocode-core` + SQLite）；`/operator`、文件作用域并发计划与 Monos 仅限本地 | 强 C/S 分离（`~/.paseo`）、独立 `terminal-worker`、HTTP MCP 子 Agent 树、持久化完工唤醒与跨宿主机路由 | **复用 Paseo Daemon 底座**——完整继承 MCP 子 Agent 编排、多端同步与远端宿主机能力 |
| **进程边界** | 关掉桌面 App 即停本地调度 | 客户端通过 WebSocket 连接，关掉 UI 不影响运行中任务 | **刻意保持 Daemon 外部独立**——重编/重启本客户端不打断任何运行中会话 |

- **为什么不把 Paseo daemon 缝进本 App**：一旦嵌入托管，就会破坏 C/S 解耦（每次改 UI 重启都会打断后台 Agent，且与 Paseo 上游升级互相拖累）。如果想日常只开这一个桌面 App，让 Paseo daemon 以独立无头方式后台常驻即可，不必一直开着 Electron 版 `Paseo.app`；只有低频配置插件、Relay 或权限时再按需打开官方端或用 CLI。

## 怎么用

前提：macOS；本机已在跑 Paseo daemon（默认 `127.0.0.1:6973`，只接受 loopback，可独立无头常驻、无需一直开着 `Paseo.app`）；Rust stable；Node（测试过 24.22.0）；一份已装好依赖的 Paseo 源码（只在打包时用来 bundle 官方 SDK，装好的 App 不再依赖它）。

```sh
RUSTUP_TOOLCHAIN=stable cargo build --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-gpui --release
PASEO_SOURCE=/path/to/paseo python3 crates/monocode-gpui/paseo-prototype/package.py /tmp/monocode-paseo
open "/tmp/monocode-paseo/Monocode Paseo Prototype.app"
```

- 独立 Bundle ID `dev.usemono.paseo-prototype`，数据在 `~/Library/Application Support/Monocode Paseo Prototype/`，不碰原版 MonoCode 的数据。
- 认证走 Paseo 官方的本地凭据读取，不把任何凭据写进源码或 App。
- 可选配置 `~/Library/Application Support/Monocode Paseo Prototype/prototype.json`（或环境变量 `PASEO_PROTOTYPE_CONFIG`）：

```json
{ "endpoint": "127.0.0.1:6973", "serverId": "你的 serverId（可选）", "defaultProjectNames": ["项目A", "项目B"], "nodePath": "/path/to/node" }
```

- 打包产物是 ad-hoc 签名，未公证；Node 路径在打包时记录，移动后用 `nodePath` 覆盖。
- 适配层细节见 [crates/monocode-gpui/paseo-prototype/README.md](crates/monocode-gpui/paseo-prototype/README.md)（英文）。

## 和原版 MonoCode 的差异

**可用**
- 原版窗口、侧栏、标签页、分屏、输入框，以及项目配色等上游预览版功能。Paseo 对话实时同步，回复全部可见，工具调用和思考折叠。点开对话先以预览标签打开（VS Code 式），操作后才固定。
- 回合一开始就显示“工作中”和计时，不用等第一行输出。
- 新建对话首次发送时才在 Paseo 创建，防止重复；可选新建独立工作区，或加入同项目最近 20 个工作区之一。默认模型依次取：项目默认 → 设置里的默认 → 当前对话 → 最近一次选择。
- 切换模型和推理强度、预设、收藏模型（在模型选择器里点星标；预设也出现在 Favorites 里）。
- 子 Agent 嵌套在父对话下显示（含同工作区兄弟子对话归并）；点击父对话默认保持折叠，右侧用聚合状态徽标优先提示待输入/失败/待查看/工作中。同一 Git 仓库的 linked worktree 合并到一个项目导航。
- 输入框支持粘贴、拖拽和上传图片/截图附件，经 Paseo 图片接口发送并在对话中回显，长对话 `historyBefore` 分页加载也不丢图。
- 侧栏底部内置额度用量条，接入 Paseo `listUsageReports()`（自适应可滚动详情弹窗，支持 Codex 与 Antigravity 剩余额度、重置倒计时、消耗速率预测，以及类似 Paseo 的置顶开关自定义底栏展示项）。
- 支持 Paseo 原生 Rewind 回退（仅空闲态可用，带确认弹窗，可选仅回退对话或同时回退文件）与新草稿直接创建/复用 Paseo 托管 Git Worktree。
- 支持侧栏宿主机切换器，经本机 Paseo daemon 路由连接远程宿主机（如 devbox），并对远程宿主机严格隔离本机文件、终端与 `file://` 链接访问。
- 权限审批与问题回答、取消当前回合。
- 回合进行中继续发送：设置 → 聊天 →「后续消息行为」选**排队**（当前回合结束后自动发出，可编辑、移除）或 **Steer**（立即插入当前回合；不支持插入的 provider 由 Paseo 中断并替换回合）。按 Cmd+Enter 发送时，这一条用相反的方式。队列只保存在本客户端，发送成功后两端才会看到；结果不确定时不自动重发。Steer 只支持纯文本。
- 侧栏：筛选、按项目或**按状态分组**（需要输入 → 失败 → 待查看 → 工作中 → 已完成）、按最近活动或创建时间排序；归档/取消归档、改名、置顶（仅主对话）和文件夹均走内存增量更新与无锁 RPC，避免全量目录重扫卡顿。
- 项目图标、底部项目切换（可只保留常用项目，其余手动加回），以及左侧边栏双指横滑切仓库（1:1 跟手双页推入，带惯性弹簧与边缘橡皮筋回弹）。
- 只读的文件和 Git 面板，支持 Diff 多行框选与多条行内评审批注（汇总插入输入框，不自动发送）；本机项目可直接打开**本地终端**。
- 性能专项优化：绕过 shell shim 直调原生 `git`，合并 status/numstat 查询并加单飞/TTL 缓存，消除 Git/bash 进程风暴与远端 2s 硬轮询，并解除远端 RPC 期间的 `Store` 锁阻塞。
- 支持更早历史分页加载（`historyBefore`）。
- 本地笔记；界面 **English / 简体中文** 切换（设置 → 通用 → 语言，默认跟随系统）。

**隐藏 / 未实现**
- 远端 PTY / relay / tunnel，以及远程宿主机的本地文件/Git 面板。
- 删除对话、标签。
- 快速输入框、Monos、自动化、Inbox、多 Agent 编排等依赖原版本地执行的功能。
- 权限选择器：新对话默认使用 provider 的完全访问模式，不在界面显示。

## 致谢与许可

- 上游 MonoCode（MIT），GPUI 相关见 [NOTICE](NOTICE)；Paseo SDK 及其依赖的许可随打包附带。
- 中文词表来自 [hardbeat920/monocode#1073](https://github.com/hardbeat920/monocode/pull/1073)（kev17in，MIT）。
- 本 fork 沿用 MIT 许可，见 [LICENSE](LICENSE)。

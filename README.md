# Monocode Paseo（实验版）

基于 [MonoCode](https://github.com/hardbeat920/monocode) 的原生 GPUI 客户端（`mc/migrate-monocode-to-gpui`，同步到 `native-v0.16.0-preview.7`），把后端换成本机的 [Paseo](https://github.com/getpaseo/paseo) daemon：界面沿用原版 Monocode 原生 Shell，对话、执行和历史都留在 Paseo 里，两边看到的是同一批对话，可以互相接着聊。

> ⚠️ **个人实验版本**：只在作者自己的 macOS（Apple Silicon）+ Paseo 0.13.0-beta.1 上用过，不保证效果、兼容性或稳定性，不提供支持。需要正式体验请用原版 MonoCode。原版说明见 [README.upstream.md](README.upstream.md)。

## 怎么用

前提：macOS；本机已在跑 Paseo daemon（默认 `127.0.0.1:6973`，只接受 loopback）；Rust stable；Node（测试过 24.22.0）；一份已装好依赖的 Paseo 源码（打包时用来 bundle 官方 SDK，装好的 App 不再依赖它）。

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

## 和原版 MonoCode 的差异

**可用**
- 原版窗口、侧栏、标签页、分屏、输入框；Paseo 对话实时同步，回复全部可见、工具和思考折叠。
- 新建对话（首次发送时才在 Paseo 创建，防重复；可选新建独立工作区或加入同项目最近 20 个工作区之一）、切换模型/强度、预设（也出现在模型选择的 Favorites 里）。
- 子 Agent 嵌套在父对话下显示；同一 Git 仓库的 linked worktree 合并到一个项目导航。
- 权限审批与问题回答、取消当前回合。
- 回合进行中继续发送：按设置 → 聊天 →「后续消息行为」**排队**（当前回合结束后自动发出，可编辑/移除）或 **Steer**（立即插入当前回合；不支持插入的 provider 由 Paseo 中断并替换回合）。队列只保存在本客户端，发送成功后两端才会看到；结果不确定时不自动重发。Steer 仅支持纯文本。
- 侧栏：筛选、按项目或**按状态分组**（需要输入 → 失败 → 待查看 → 工作中 → 已完成）、按最近活动/创建时间排序。
- 项目图标、底部项目切换（可只保留常用项目，其余手动加回）。
- 只读的文件和 Git 面板；本机项目可直接打开**本地终端**。
- 本地笔记；界面 **English / 简体中文** 切换（设置 → 通用 → 语言，默认跟随系统）。

**隐藏 / 未实现**
- 额度用量显示、远端 PTY / relay / tunnel。
- 删除对话、标签、更早历史分页。
- 快速输入框、Monos、自动化、Inbox、多 Agent 编排等依赖原版本地执行的功能。
- 权限选择器：新对话默认使用 provider 的完全访问模式，不在界面显示。

## 致谢与许可

- 上游 MonoCode（MIT），GPUI 相关见 [NOTICE](NOTICE)；Paseo SDK 及其依赖的许可随打包附带。
- 中文词表来自 [hardbeat920/monocode#1073](https://github.com/hardbeat920/monocode/pull/1073)（kev17in，MIT）。
- 本 fork 沿用 MIT 许可，见 [LICENSE](LICENSE)。

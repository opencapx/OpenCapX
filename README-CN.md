<p align="center">
 <img src="public/pet-logo.png?raw=true" alt="OpenCapX" height="100px"/>
<h1 align="center">OpenCapX</h1>
<div align="center">
 <strong>
    拦截智能体发出的每一次请求，牢牢守住每一项系统权限，赋予任何模型缺失的多模态能力，托举你的智能体更进一步，而这一切能力，由你用插件与规则亲手定义。
 </strong>
</div>
<br/>
<p align="center">
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="macOS" src="https://img.shields.io/badge/-macOS-black?style=for-the-badge&logo=apple&logoColor=white" />
</a>
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="Windows" src="https://img.shields.io/badge/Windows-0078D6?style=for-the-badge&logo=windows&logoColor=green" />
</a>
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="Linux" src="https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" />
</a>
</p>

<p align="center">
    <a href="./README.md">English</a> | 简体中文 | <a href="./README-VI.md">Tiếng Việt</a>
</p>

Claude Code、Codex 与 OpenCode 早已懂得如何写代码。可它们看不到你的屏幕，开不了口说话，盯不住某个文件夹，也无法在桌面上亮出一行状态。OpenCapX 正是填补这一空白的桌面实体与能力层，也是挡在它身前的那道闸门。内置的多模态提供方，赋予任何模型看、说、读剪贴板、浏览网页与处理媒体的本领；其余能力，则以已签名、运行在沙箱中的插件形式到来。智能体通过 MCP 接入，而它们发出的每一个请求，无论属于能力、插件还是 shell 命令，都会在真正执行之前被 Rust 核心拦截、裁决,命令规则，让你改写智能体实际执行的命令。

<p align="center">
  <img src="public/diagram.jpeg?raw=true" alt="OpenCapX 架构图：智能体 CLI（Claude Code、Codex、OpenCode、Gemini CLI、Cursor、Copilot CLI、Factory Droid、Oh My Pi）通过 MCP 网关与 PreToolUse 钩子接入；Rust 核心将能力路由到内置提供方与已签名插件" width="100%" />
</p>

## 功能特性

**宠物与界面**
- **2D 与 3D 宠物**：精灵图动画或 glTF/VRM 模型，具备工作中、等待你、已完成、空闲四种状态。
- **状态气泡**：主题化气泡显示每个智能体正在做什么，当智能体向你提问时会变成选择按钮。
- **托盘菜单**：状态圆点、按项目分组的会话，以及标明哪个智能体需要你的工具提示。

**智能体接口面**
- **MCP 网关**：六个工具（`say`、`notify`、`set_state`、`ask`、`list_capabilities`、`execute`）外加事件订阅，任何 MCP 宿主都能借此驱动桌面。
- **一条命令接入**：`opencapx connect <agent>` 幂等地接好 13 个智能体宿主（全部支持钩子；Claude Code、Codex、opencode、OMP 另支持 MCP 条目）。
- **通知**：智能体完成或等待输入时弹出系统通知，并汇集到通知中心。

**插件**
- **隔离进程**：插件是普通进程，通过 stdio 使用 JSON-RPC 2.0 通信，绝不是 Tauri 插件。
- **签名与审核**：分发场景使用 Ed25519，本地/团队场景使用 HMAC；第三方能力域在被信任之前必须先行声明并通过审核。
- **声明式设置**：清单中的 `settings[]` 块会渲染出图形表单，密钥保存在操作系统钥匙串中。

**治理**
- **双层权限**：每一项危险操作都要先经过 Rust 权限管理器，之后才会被任何插件看到。
- **命令规则**：`PreToolUse` 钩子会拦截智能体的 shell 命令，并把它改写为你选定的执行器。
- **审计与控制**：授权、拒绝与规则命中都会落入活动时间线，而 kill switch 可一次性停止所有插件。

**能力**
- **内置提供方**：视觉、语音、剪贴板、浏览器读取、媒体，以及 macOS PIM（照片、通讯录、日历、位置、备忘录、提醒事项、邮件）。

**自动化与告警**
- **Event → Rule → Action**：用规则文件把智能体的活动转化为桌面动作。
- **Webhooks**：Slack、Discord 或自定义端点，具备严重级别路由、去重、重试以及死信队列。

**运维**
- **工作区配置档**：维护彼此独立的插件集与权限集，并在其间切换。
- **备份与恢复**：把工作区状态快照到文件，一键恢复。
- **热键与命令面板**：全局快捷键外加一个可搜索的命令面板。

**分发**
- **应用市场与注册表**：从已签名的索引安装，支持发布者吊销以及 stable / beta / dev 渠道。

**平台**
- **跨平台**：提供 macOS、Windows 与 Linux 构建。
- **三种界面语言**：英语、简体中文与越南语。

## 三大支柱

### 插件系统

插件是普通进程，而非 Tauri 插件。每个插件通过 stdio 使用 JSON-RPC 2.0 通信，在 `opencapx-plugin.json` 中声明自己需要的能力与权限，并与核心保持隔离。插件包会经过签名（分发场景使用 Ed25519，本地/团队场景使用 HMAC），第三方能力域在被信任之前必须先行声明并通过审核。

- 作者指南：[docs/plugin-authoring.md](docs/plugin-authoring.md)
- 协议：[docs/plugin-protocol.md](docs/plugin-protocol.md)
- 清单规范：[docs/plugin-manifest.md](docs/plugin-manifest.md)
- 签名与分发：[docs/plugin-signing.md](docs/plugin-signing.md)
- 权限域：[docs/permission-domains.md](docs/permission-domains.md)

### MCP 网关

智能体以 stdio MCP 服务器的形式启动 `opencapx mcp`。该进程通过 HTTP 把每一次工具调用转发给本地核心。v1 的接口面共六个工具：`opencapx.say`、`opencapx.notify`、`opencapx.set_state`、`opencapx.ask`、`opencapx.list_capabilities` 和 `opencapx.execute`。新增能力不会增加工具：`opencapx.execute` 通过路由器触达任何已注册的能力。`opencapx.subscribe` / `opencapx.unsubscribe` 覆盖事件流类能力。

- 规范：[docs/mcp.md](docs/mcp.md)

### 权限系统

每一项危险操作都要先经过 Rust 权限管理器，之后才会被任何插件看到。WebView 从来不是安全边界，核心才是。智能体通过 TOFU 注册流程表明身份，插件预先声明权限，这两层分别接受检查。拒绝与授权都会记入审计记录。

- 模型与作用域：[docs/permissions.md](docs/permissions.md)

## 命令规则

OpenCapX 也挡在智能体自身 shell 命令的前面。`PreToolUse` 钩子会在每次 Bash 工具调用真正运行之前将其拦截，并可以把它**改写**成你选定的执行器形式：`curl https://x` 变成 `sandbox curl https://x`。目前 Claude Code 与 Codex 会被改写，其他宿主则原样放行。

**OpenCapX 只负责路由，并不做裁决。** 命令会交给你配置的执行器，而边界（沙箱、代理、容器）由该执行器负责。这是一条与上文权限系统相互独立的流水线：权限把关的是 `/rpc` 能力调用，命令规则约束的则是智能体自己发出的命令。

规则是分层的：内置（默认为空）、全局（`~/.opencapx/rules.json`）以及项目级（`<project>/.opencapx/rules.json`）。项目级规则是一种注入面，因此在你**显式信任该项目**（`opencapx rules trust`）之前会一直**保持忽略**。一次改写会向活动时间线发出 `rule.applied` 审计事件；规则文件缺失或损坏时会 fail-open：命令按原样运行，智能体永远不会被阻断。

- 规范：[docs/rules.md](docs/rules.md)
- 命令行：`opencapx rewrite`，以及 `opencapx rules list | explain | trust | untrust`
- 设置页面：**命令规则**标签页用于添加规则、切换全局规则开关，以及管理受信任的项目。

## 面向用户

### 安装

从[发布页面](https://github.com/opencapx/OpenCapX/releases/latest)下载最新构建：macOS（Apple Silicon）使用 `.dmg`，Windows 使用 `x64-setup.exe` 或 `.msi`，Linux 使用 `.deb` / `.rpm` / `.AppImage`。macOS 构建未做代码签名，也未经过公证；如果 Gatekeeper 阻止首次启动，请右键点击应用并选择「打开」。

从源码构建的方法、所需前置依赖以及实时开发窗口，参见 [INSTALL.md](INSTALL.md)。

### 接入你的智能体

一条命令即可接好智能体的钩子与 MCP 服务器条目（幂等操作，智能体配置中不会落入任何凭据，令牌流程在启动时由 `opencapx mcp` 与核心之间处理）。若终端里还没有 `opencapx` 命令，一键安装：**托盘菜单 → 安装 opencapx 命令**（或 **设置 → 通用 → 命令行**；见 [INSTALL.md](INSTALL.md#the-opencapx-command)）：

```bash
opencapx connect claude   # 或 13 个宿主中的任意一个——名字打错会列出全部
```

重启智能体，然后让它调用 `opencapx.list_capabilities` 来验证。各宿主的手动配置形式参见 [docs/mcp.md](docs/mcp.md)。

### 五分钟写出你的第一个插件

用已发布的初始化器脚手架生成一个 TypeScript 插件：

```bash
npm create opencapx-plugin -- --id com.acme.hello --name "Hello"
```

接着构建并测试它：

```bash
cd hello && npm install && npm test && npm run build
```

结果是一个带有清单、一个 `image.analyze` 能力以及一个测试的插件。编辑 `src/plugin.ts` 即可改变它的行为，然后在设置窗口中安装该文件夹。包含 Python 路径在内的完整演练参见 [docs/plugin-authoring.md](docs/plugin-authoring.md)。

## 面向开发者

### 架构

智能体通过钩子与 MCP 接入核心。核心拥有事件总线、能力注册表与路由器、权限管理器以及插件生命周期；插件作为相互隔离的子进程运行，WebView 仅负责呈现。

架构图、运行时分层、一次能力调用的数据流以及完整模块地图，参见 [ARCHITECTURE.md](ARCHITECTURE.md)。协议文档从 [docs/README.md](docs/README.md) 开始。

### 开发

开发环境、提交前的验证关卡以及提交风格，参见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 技术栈与致谢

- [Tauri 2](https://tauri.app/)：桌面外壳（托盘图标、macOS 私有 API、PNG 图像支持）。
- [Rust](https://www.rust-lang.org/)：核心，涵盖事件总线、能力注册表/路由器、权限管理器、插件运行时与进程管理器。
- [TypeScript](https://www.typescriptlang.org/) + [Vite](https://vitejs.dev/)：WebView UI。
- [Three.js](https://threejs.org/) + [@pixiv/three-vrm](https://github.com/pixiv/three-vrm)：3D 宠物渲染（glTF/VRM）。
- [rusqlite](https://github.com/rusqlite/rusqlite)：本地 SQLite 存储。
- [tiny_http](https://github.com/tiny-http/tiny-http)：用于钩子事件与 MCP 转发的本地 HTTP 入口。
- [ed25519-dalek](https://github.com/dalek-cryptography/ed25519-dalek) / [hmac](https://github.com/RustCrypto/MACs) / [sha2](https://github.com/RustCrypto/hashes)：插件包签名与验签。
- [keyring](https://github.com/hwchen/keyring-rs)：用于存放插件密钥的操作系统钥匙串。
- [DOMPurify](https://github.com/cure53/DOMPurify) + [marked](https://github.com/markedjs/marked)：UI 中安全的 markdown 渲染。
- [serde](https://serde.rs/) / serde_json / serde_yaml：序列化。

欢迎任何形式的 PR（文档、UI、代码）。

## 路线图

[ROADMAP.md](ROADMAP.md)。

## 许可证

Apache-2.0。参见 [LICENSE](LICENSE)。

## 安全

请勿为安全漏洞提交公开 issue。报告渠道、受支持的版本以及密钥仪式的相关参考，参见 [SECURITY.md](SECURITY.md)。

## 更多

- [INSTALL.md](INSTALL.md)：下载与从源码构建的说明
- [ARCHITECTURE.md](ARCHITECTURE.md)：运行时分层、数据流与模块地图
- [CONTRIBUTING.md](CONTRIBUTING.md)：如何在本仓库中工作
- [ROADMAP.md](ROADMAP.md)：后续计划
- [CHANGELOG.md](CHANGELOG.md)：变更记录
- [docs/](docs/README.md)：完整规范集

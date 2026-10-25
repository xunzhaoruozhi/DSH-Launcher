<p align="center">
  <img src="assets/dsh-icon-source.png" alt="DSH Launcher 图标" width="180">
</p>

[![dsh.so risk](https://www.dsh.so/badge/dsh-launcher-6.svg)](https://www.dsh.so/artifact/dsh-launcher-6/)
[![dsh.so install](https://www.dsh.so/badge/install/dsh-launcher-6.svg)](https://www.dsh.so/artifact/dsh-launcher-6/)

# DSH Launcher

一个基于 Tauri 2 的 DeepSeek Harness 桌面启动器。它负责启动和停止 `dsh web`，主窗口以浏览器式标签页承载 dsh WebUI；包管理、配置文件编辑、插件安装和新窗口入口都收纳在窄工具栏的弹窗里。

插件目录使用 [DSH 插件商店](https://dsh.aitreez.com/) 提供的 `catalog.json` API，仅在用户打开插件弹窗时加载一次；安装通过 dsh 官方命令完成，例如 `dsh plugin --profile web add github:omdsh-dev/DSH-better-sidebar`。

## 界面截图

<p align="center">
  <img src="assets/image1.png" alt="DSH Launcher 主界面" width="32%">
  <img src="assets/image2.png" alt="DSH Launcher 设置界面" width="32%">
  <img src="assets/image3.png" alt="DSH Launcher 插件界面" width="32%">
</p>

## 标签页

顶栏中间是标签条，每个标签都是一个独立的 WebUI 实例，切换时保留各自的会话状态：

- 新建 `Ctrl+T` 或标签条上的 ＋；关闭 `Ctrl+W`、鼠标中键或标签上的 ×；`Ctrl+Tab` 循环切换
- 拖动标签可重排；拖出顶栏松手即成为新窗口；拖到其他 Launcher 窗口的顶栏上则并入该窗口
- 双击标签可重命名；标签很多时会像浏览器一样逐级收窄，实在放不下时标签条支持滚轮滚动

## 端口与外部服务

如果启动时端口（默认 3080）已被一个可用的 Web 服务占用——比如你提前在终端里自己跑了 `dsh web`——Launcher 会直接沿用它，而不是报端口占用：

- Launcher 不接管该进程：管理弹窗中的停止/重启会禁用，退出 Launcher 也不会结束它
- 外部服务退出后，界面自动回到可启动状态，可以一键改由 Launcher 启动
- 只有占用者不是 Web 服务（例如残留的半死进程）时才会提示端口冲突

## 增强插件

以下插件均属于功能与体验增强，不会较大幅度干扰 DSH 的 loop 工作流：

### [DSH Better Sidebar](https://github.com/omdsh-dev/DSH-better-sidebar)

提供更完善的侧边栏与系统终端，让 dsh WebUI 从基础对话界面进一步升级为更专业、更高效的 Agent IDE。

```powershell
dsh plugin --profile web add dsh-better-sidebar
```

### [dsh-at-file](https://github.com/omdsh-dev/dsh-at-file)

支持在对话框中快捷添加文件路径，减少手动输入与来回查找，让文件引用更加顺手自然。

```powershell
dsh plugin --profile web add "github:omdsh-dev/dsh-at-file"
```

### [dsh-tool-diff](https://github.com/omdsh-dev/dsh-tool-diff)

diff工具是一个值得从 Bash 中“毕业”的文件审查工具，帮助 Agent 更高效地查看与审阅文件变更，在提升审查体验的同时大幅节省 token。

```powershell
dsh plugin --profile web add github:omdsh-dev/dsh-tool-diff
```

### [dsh-genui](https://github.com/omdsh-dev/dsh-genui)

让webui支持直接渲染图表、表格、Mermaid 甚至 html 交互组件，不仅限于基础markdown渲染！

```
dsh plugin --profile web add git+https://github.com/omdsh-dev/dsh-genui.git
```

### [dsh-mcp-manager](https://github.com/hyqhyq3/dsh-mcp-manager)

普适性的mcp适配器，让mcp接入代替那些直接添加Tools的插件吧（除非没有对应的mcp），为了你的token、维护难度考虑

```
npx -p @deepseek-ai/dsh dsh plugin --profile web add github:hyqhyq3/dsh-mcp-manager
```


## 开发
```powershell
npm install
npm run tauri dev
```

启动器默认使用 PATH 中的 `dsh`：

```powershell
npm install -g @deepseek-ai/dsh
```

若直接启动本地安装dsh失败，可以在启动设置中切换为 npx 模式。首次通过 npx 启动需要网络连接。

## 构建

```powershell
npm run tauri build
```

配置保存在 Tauri 的应用配置目录中。


## WebView 看门狗（卡死排查）

启动器内置一个看门狗线程（`src-tauri/src/watchdog.rs`），后台每 5 秒扫描自己的
msedgewebview2 子进程树。某个子进程连续 20 秒吃掉超过 0.6 个核时，做两件事：

1. **自动降级**：如果高 CPU 的是 `--type=renderer` 渲染进程，把它降到
   BELOW_NORMAL 优先级。dsh iframe 和壳子页面同站（localhost 不同端口），
   WebView2 不支持 iframe 级进程隔离，两者共用一个渲染进程；不加限制时，
   dsh 页面流式输出烧 CPU 会把壳子的输入响应与绘制一起挤死，表现为整个
   窗口点不动、殃及其他程序无法切换到前台。降级后渲染进程只能用剩余 CPU，
   壳子保持可操作，冷却后自动恢复正常优先级。
2. **留证据**：把进程树、内存、CPU 和当时的启动器状态消息追加写入日志：

```
%APPDATA%\ai.deepseek.dsh-launcher\webview-watchdog.log
```

用途：窗口"点不动、转圈、假死"这类问题事后无法复现，日志会在事发现场留下
进程级证据，不用再靠猜。

排查口诀：

- 日志出现"持续高 CPU"与"已降级优先级"，指向 msedgewebview2 的
  `--type=renderer` 子进程 → 页面（Web 侧）在烧 CPU，看当时的 dsh 是否在
  流式输出。降级只是保壳子不卡，页面本身要流畅还得看 Web 侧
- 日志出现"优先级已恢复" → 烧 CPU 结束，渲染进程回到正常调度
- CPU 全部安静但窗口仍然无响应 → 客户端主线程被同步调用卡住（锁/IPC）
- `dsh-launcher.exe` 自身 CPU 飙高 → 后端 Rust 代码在忙

日志超过 512KB 自动轮转为 `.log.old`；`watchdog 启动` 行出现在每次启动器
冷启动时，可用于对齐时间线。相关沉疴与修复记录见 git log（状态广播节流、
事件去 logs 字段、theme_sync 停用）。


## 相关链接

- [DSH 插件商店](https://dsh.aitreez.com/)
- [Linux.do](https://linux.do)

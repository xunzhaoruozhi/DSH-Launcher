import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { ask, open } from "@tauri-apps/plugin-dialog";
import whaleIconUrl from "../assets/dsh-icon-source.png";
import {
  CircleAlert,
  CircleCheck,
  Code2,
  Download,
  ExternalLink,
  Eye,
  FileCog,
  FileText,
  FolderOpen,
  Github,
  Link,
  LoaderCircle,
  Minus,
  PackageCheck,
  Plus,
  Puzzle,
  RefreshCw,
  RotateCcw,
  Save,
  Search,
  Settings,
  Shield,
  SlidersHorizontal,
  Square,
  TerminalSquare,
  Trash2,
  Wrench,
  X,
  createIcons,
} from "lucide";
import "./styles.css";

type LaunchMode = "command" | "npx";
type Phase = "stopped" | "starting" | "ready" | "stopping" | "failed";
type DialogId = "manage-dialog" | "config-dialog" | "plugin-dialog" | "settings-dialog";

interface LauncherConfig {
  launch_mode: LaunchMode;
  executable: string;
  npx_package: string;
  working_directory: string;
  dsh_home: string;
  port: number;
  trusted_hosts: string[];
  auto_start: boolean;
  open_on_ready: boolean;
  close_behavior: "tray" | "exit";
  stop_dsh_on_exit: boolean;
  summon_shortcut: string;
  download_directory: string;
  download_ask: boolean;
  download_choose_location: boolean;
  developer_mode: boolean;
  window_width: number;
  window_height: number;
}

interface LauncherStatus {
  phase: Phase;
  message: string;
  /** 给人看的干净地址（状态栏、刷新按钮），不带认证令牌。 */
  url: string;
  /** 内嵌页面真正要装载的地址：dsh 0.1.2 起 Web 需要先带令牌换一次 cookie。 */
  web_url: string | null;
  /** 端口上的 dsh 要求浏览器认证（401）。 */
  auth_required: boolean;
  /** 本 WebView 已经认证过一次，不必再提示。 */
  auth_satisfied: boolean;
  pid: number | null;
  external: boolean;
  /** 当前服务以安全模式运行（一次性隔离 DSH_HOME）。 */
  safe_mode: boolean;
  /** 连续启动失败次数；达到阈值时错误界面亮出安全模式入口。 */
  consecutive_failures: number;
  logs: string[];
  busy: string | null;
}

interface EmbeddedPageState {
  frame: HTMLIFrameElement | null;
  loadedUrl: string;
  /** 当前 iframe 内容已装载完成（load 已过），可以放心展示。 */
  frameReady: boolean;
  /** 已消费的带令牌地址：它的页面还在正常跑，状态事件再带来同一令牌地址时不重载。 */
  authConsumedFor: string | null;
  stale: boolean;
}

interface PackageInfo {
  current_version: string;
  latest_version: string;
  source: string;
  checked_at: string;
  detail: string;
}

interface ConfigFileInfo {
  id: string;
  name: string;
  path: string;
  content: string;
  editable: boolean;
}

interface InstalledPlugin { name: string; version: string; bundle: boolean; installed_version: string; channel: string; update_spec: string; }
interface PluginUpdateInfo { name: string; channel: string; installed_version: string; latest_version: string; update_available: boolean; detail: string; }
interface PluginSearchResult { name: string; version: string; description: string; homepage: string; npm_url: string; keywords: string[]; }
interface OperationResult { success: boolean; output: string; }
interface MarketMeta { id: string; label: string; color?: string | null; }
interface MarketPlugin {
  name: string; full_name: string; spec: string; description: string; url: string; homepage: string;
  avatar_url: string; topics: string[]; language: string; stars: number; pushed_at: string;
  archived: boolean; project_type: string; category: string; verified: boolean;
}
interface MarketCatalog { plugins: MarketPlugin[]; fetched_at: number; }

const app = document.querySelector<HTMLDivElement>("#app")!;
app.innerHTML = `
  <div class="shell">
    <header class="titlebar" data-tauri-drag-region>
      <nav class="toolbar-left" aria-label="DSH Launcher 工具">
        <button class="tool-button" data-dialog="manage-dialog" title="管理"><i data-lucide="package-check"></i><span>管理</span></button>
        <button class="tool-button" data-dialog="config-dialog" title="配置"><i data-lucide="sliders-horizontal"></i><span>配置</span></button>
        <button class="tool-button" data-dialog="plugin-dialog" title="插件"><i data-lucide="puzzle"></i><span>插件</span></button>
        <button class="tool-button" data-dialog="settings-dialog" title="设置"><i data-lucide="settings"></i><span>设置</span></button>
        <button id="toolbar-safe" class="tool-button" type="button" title="进入安全模式：一次性隔离环境启动 dsh，不加载插件与正式数据"><i data-lucide="shield"></i><span>安全模式</span></button>
        <button id="new-window" class="tool-button" title="新建窗口"><i data-lucide="plus"></i><span>新建</span></button>
      </nav>
      <nav class="window-controls" aria-label="窗口控制">
        <button id="refresh-web" title="刷新 dsh WebUI"><i data-lucide="refresh-cw"></i></button>
        <button id="window-minimize" title="最小化"><i data-lucide="minus"></i></button>
        <button id="window-maximize" title="最大化"><i data-lucide="square"></i></button>
        <button id="window-close" class="close" title="关闭到托盘"><i data-lucide="x"></i></button>
      </nav>
    </header>

    <div id="safe-mode-banner" class="safe-mode-banner" hidden>
      <i data-lucide="shield"></i>
      <span>安全模式 · 一次性隔离环境运行中，正式数据未加载</span>
      <button id="exit-safe-mode" type="button" title="停掉安全模式、删除临时目录，用正式环境重新启动"><i data-lucide="rotate-ccw"></i><span>退出并正常重启</span></button>
    </div>

    <main id="workspace-view" class="workspace-view">
      <div id="workspace-state" class="workspace-state" data-phase="stopped">
        <button id="workspace-start" class="whale-button" type="button" aria-label="启动 dsh" title="启动 dsh"><span class="whale-wave whale-wave-one"></span><span class="whale-wave whale-wave-two"></span><img src="${whaleIconUrl}" alt="DeepSeek" draggable="false"></button>
        <h2 id="workspace-title">正在准备 dsh</h2>
        <p id="workspace-message">启动器会在服务就绪后载入 WebUI。</p>
        <button id="workspace-safe" type="button" title="用一次性隔离环境启动 dsh：不加载插件和正式数据，适合排查启动失败" hidden><i data-lucide="shield"></i><span>进入安全模式</span></button>
      </div>
      <div id="auth-prompt" class="auth-prompt" hidden>
        <div class="auth-card">
          <div class="auth-head">
            <p class="auth-title">这个 dsh 是别处启动的，需要认证</p>
            <button id="auth-close" class="auth-close" type="button" title="暂不处理"><i data-lucide="x"></i></button>
          </div>
          <p class="auth-hint">它是你在终端里自己起的 dsh，认证钥匙只打印在那个终端里，启动器拿不到。两条路任选：① 把终端里 <code>dsh web: http://127.0.0.1:3080/?token=…</code> 那一行整条粘到下面；② 或者回终端按 Ctrl+C 关掉它，再点启动让启动器自己拉起 dsh —— 那样会自动认证，以后不用再管。</p>
          <div class="auth-row">
            <input id="auth-url" type="text" autocomplete="off" spellcheck="false" placeholder="http://127.0.0.1:3080/?token=…">
            <button id="auth-submit" class="button primary" type="button"><i data-lucide="link"></i><span>连接</span></button>
          </div>
        </div>
      </div>
    </main>

    <div id="manage-dialog" class="modal" hidden>
      <div class="modal-backdrop" data-close-modal></div>
      <section class="modal-panel manage-panel" role="dialog" aria-modal="true" aria-labelledby="manage-title">
        <header class="modal-header"><div><p class="eyebrow">DSH LAUNCHER</p><h2 id="manage-title">管理</h2></div><button class="modal-close" data-close-modal title="关闭"><i data-lucide="x"></i></button></header>
        <div class="package-summary">
          <div><span class="summary-label">当前版本</span><strong id="current-version">未检查</strong></div>
          <div><span class="summary-label">最新版本</span><strong id="latest-version">未检查</strong></div>
          <div><span class="summary-label">启动来源</span><strong id="package-source">读取中</strong></div>
        </div>
        <div class="modal-actions"><button id="check-package" class="button quiet"><i data-lucide="search"></i><span>检查版本</span></button><button id="update-package" class="button primary"><i data-lucide="download"></i><span>更新 dsh</span></button></div>
        <div class="service-line"><div><span class="summary-label">Web 服务</span><strong id="service-state">读取中</strong></div><div class="service-buttons"><button id="manage-restart" class="icon-button" title="重启服务"><i data-lucide="rotate-ccw"></i></button><button id="manage-stop" class="icon-button danger" title="停止服务"><i data-lucide="square"></i></button></div></div>
        <div class="manage-config"><div class="section-title"><div><p class="eyebrow">STARTUP</p><h3>启动配置</h3></div><button id="manage-save" class="button primary"><i data-lucide="save"></i><span>保存并重启</span></button></div>
          <form id="settings-form" class="compact-form">
            <div class="field full"><label>CLI 来源</label><div class="segmented"><label><input type="radio" name="launch_mode" value="command" checked><span>已安装 dsh</span></label><label><input type="radio" name="launch_mode" value="npx"><span>npx</span></label></div></div>
            <div id="command-field" class="field full"><label for="executable">dsh 命令或完整路径</label><input id="executable" name="executable" autocomplete="off" spellcheck="false"></div>
            <div id="npx-field" class="field full" hidden><label for="npx-package">npm 包</label><input id="npx-package" name="npx_package" autocomplete="off" spellcheck="false"></div>
            <div class="field full"><label for="working-directory">工作目录</label><div class="path-input"><input id="working-directory" name="working_directory" autocomplete="off" spellcheck="false"><button id="browse-workspace" type="button" class="icon-button" title="选择目录"><i data-lucide="folder-open"></i></button></div></div>
            <div class="field"><label for="port">本地端口</label><input id="port" name="port" type="number" min="1024" max="65535"></div>
            <div class="field"><label for="dsh-home">DSH_HOME（可选）</label><input id="dsh-home" name="dsh_home" autocomplete="off" spellcheck="false"></div>
          </form>
        </div>
        <pre id="manage-output" class="operation-output" hidden></pre>
      </section>
    </div>

    <div id="config-dialog" class="modal" hidden>
      <div class="modal-backdrop" data-close-modal></div>
      <section class="modal-panel config-panel" role="dialog" aria-modal="true" aria-labelledby="config-title">
        <header class="modal-header"><div><p class="eyebrow">DSH HOME</p><h2 id="config-title">配置文件</h2></div><button class="modal-close" data-close-modal title="关闭"><i data-lucide="x"></i></button></header>
        <div class="config-toolbar"><select id="config-file-select" aria-label="选择配置文件"></select><button id="open-config-file" class="button quiet"><i data-lucide="external-link"></i><span>在文件管理器中打开</span></button></div>
        <p id="config-file-path" class="path-caption"></p>
        <textarea id="config-editor" class="config-editor" spellcheck="false" aria-label="配置文件内容"></textarea>
        <footer class="modal-footer"><span id="config-save-result"></span><button id="save-config-file" class="button primary"><i data-lucide="save"></i><span>保存文件</span></button></footer>
      </section>
    </div>

    <div id="plugin-dialog" class="modal" hidden>
      <div class="modal-backdrop" data-close-modal></div>
      <section class="modal-panel plugin-panel" role="dialog" aria-modal="true" aria-labelledby="plugin-title">
        <header class="modal-header"><div><p class="eyebrow">COMMUNITY CATALOG</p><h2 id="plugin-title">插件</h2></div><button class="modal-close" data-close-modal title="关闭"><i data-lucide="x"></i></button></header>
        <div class="plugin-install"><div class="input-with-icon"><i data-lucide="github"></i><input id="plugin-spec" placeholder="github:owner/repository" autocomplete="off" spellcheck="false"></div><button id="install-plugin" class="button primary"><i data-lucide="download"></i><span>安装</span></button></div>
        <p class="plugin-hint">安装会执行 <code>dsh plugin --profile web add &lt;spec&gt;</code>，服务会自动重启。目录来源：<a data-external href="https://dsh.aitreez.com/" rel="noreferrer">dsh.aitreez.com</a></p>
        <div class="plugin-tabs"><button class="plugin-tab active" data-plugin-tab="installed">已安装</button><button class="plugin-tab" data-plugin-tab="market">插件商店</button><button class="plugin-tab" data-plugin-tab="search">npm 搜索</button></div>
        <div id="installed-plugins" class="plugin-content"></div>
        <div id="market-plugins" class="plugin-content" hidden>
          <div class="market-toolbar">
            <div class="input-with-icon"><i data-lucide="search"></i><input id="market-search" placeholder="搜索名称、作者、简介或关键词" autocomplete="off" spellcheck="false"></div>
            <select id="market-type" aria-label="类型筛选"></select>
            <select id="market-category" aria-label="分类筛选"></select>
            <select id="market-sort" aria-label="排序"><option value="recent">最近活跃</option><option value="stars">Star 最多</option></select>
            <button id="market-refresh" class="icon-button" title="刷新商店数据"><i data-lucide="refresh-cw"></i></button>
          </div>
          <div id="market-list" class="market-list"></div>
          <footer class="market-footer">
            <span id="market-count"></span>
            <button id="market-more" class="button quiet" hidden>加载更多</button>
            <a data-external href="https://dsh.aitreez.com/" title="在浏览器打开插件商店">dsh.aitreez.com</a>
          </footer>
        </div>
        <div id="search-plugins" class="plugin-content" hidden><div class="plugin-search"><div class="input-with-icon"><i data-lucide="search"></i><input id="plugin-search-input" placeholder="搜索 dsh plugin"></div><button id="plugin-search-button" class="icon-button" title="搜索"><i data-lucide="search"></i></button></div><div id="plugin-search-results"></div></div>
        <pre id="plugin-output" class="operation-output" hidden></pre>
      </section>
    </div>
    <div id="settings-dialog" class="modal" hidden>
      <div class="modal-backdrop" data-close-modal></div>
      <section class="modal-panel settings-panel" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <header class="modal-header"><div><p class="eyebrow">DSH LAUNCHER</p><h2 id="settings-title">设置</h2></div><button class="modal-close" data-close-modal title="关闭"><i data-lucide="x"></i></button></header>
        <div class="settings-content">
          <div class="settings-section"><p class="eyebrow">WINDOW</p><h3>关闭按钮行为</h3><div class="segmented wide"><label><input type="radio" name="close_behavior" value="tray"><span>最小化到托盘</span></label><label><input type="radio" name="close_behavior" value="exit"><span>退出 Launcher</span></label></div>
            <div class="field full"><label for="setting-summon-shortcut">全局唤出快捷键</label><div class="input-with-hint"><input id="setting-summon-shortcut" autocomplete="off" spellcheck="false" placeholder="例如 Alt+Shift+D"><small class="field-hint">任意程序里按下即唤出/收起主窗口，留空则禁用</small></div></div></div>
          <label class="check-row"><input id="setting-auto-start" type="checkbox"><span><strong>启动后自动启动 dsh Web 服务</strong><small>Launcher 打开后立即启动本地服务</small></span></label>
          <label class="check-row"><input id="setting-stop-dsh" type="checkbox"><span><strong>退出 Launcher 时结束 dsh</strong><small>只结束由当前 Launcher 启动并登记的进程树</small></span></label>
          <div class="settings-section developer-section"><p class="eyebrow">DEVELOPER</p><h3>开发者模式</h3>
            <label class="check-row compact"><input id="setting-developer-mode" type="checkbox"><span><strong>启用开发者工具</strong><small>开启后按 F12 或 Ctrl+Shift+I 调试当前窗口（包括内嵌的 DSH 页面）；关闭后快捷键不可用。发行版同样支持。</small></span></label>
          </div>
          <div class="settings-section download-section"><p class="eyebrow">DOWNLOAD</p><h3>下载</h3>
            <div class="field full"><label for="setting-download-directory">默认下载目录</label><div class="path-input"><input id="setting-download-directory" autocomplete="off" spellcheck="false"><button id="browse-download-directory" type="button" class="icon-button" title="选择下载目录"><i data-lucide="folder-open"></i></button></div></div>
            <label class="check-row"><input id="setting-download-ask" type="checkbox"><span><strong>下载前确认</strong><small>每次下载前弹出确认提示</small></span></label>
            <label class="check-row compact"><input id="setting-download-choose" type="checkbox"><span><strong>下载前选择保存位置</strong><small>显示“另存为”对话框，可同时修改文件名</small></span></label>
          </div>
          <div class="settings-section about-section"><p class="eyebrow">ABOUT</p><h3>关于 DSH Launcher</h3><p class="about-copy">当前版本 <strong id="launcher-version">读取中</strong></p><div class="about-actions"><a class="button quiet" data-external href="https://github.com/xunzhaoruozhi/DSH-Launcher"><i data-lucide="github"></i><span>GitHub 仓库</span></a></div></div>
        </div>
        <footer class="modal-footer"><span id="settings-save-result"></span><button id="save-settings" class="button primary"><i data-lucide="save"></i><span>保存设置</span></button></footer>
      </section>
    </div>
    <div id="toast" class="toast" role="status" hidden></div>
  </div>
`;

// 平台判断：macOS 上给 <body> 加 os-macos 类，仅用于切换原生红绿灯 + 工具栏右移布局。
// 用 navigator.userAgent 判断（Tauri 桌面 webview 的 UA 在 macOS 含 "Mac"），零 Rust/权限改动，
// 因此 Windows 永远不加该类、布局与作者原始版本完全一致。
if (/Mac/i.test(navigator.userAgent)) document.body.classList.add("os-macos");

createIcons({ icons: { CircleAlert, CircleCheck, Code2, Download, ExternalLink, Eye, FileCog, FileText, FolderOpen, Github, Link, LoaderCircle, Minus, PackageCheck, Plus, Puzzle, RefreshCw, RotateCcw, Save, Search, Settings, Shield, SlidersHorizontal, Square, TerminalSquare, Trash2, Wrench, X } });
const $ = <T extends HTMLElement = HTMLInputElement>(selector: string): T => document.querySelector<T>(selector)!;
const currentWindow = getCurrentWindow();
let config: LauncherConfig;
let status: LauncherStatus = { phase: "stopped", message: "dsh 尚未启动", url: "http://127.0.0.1:3080", web_url: null, auth_required: false, auth_satisfied: false, pid: null, external: false, safe_mode: false, consecutive_failures: 0, logs: [], busy: null };
// 认证提示条是否被用户按下了「暂不处理」。
let authPromptClosed = false;
let configFiles: ConfigFileInfo[] = [];
let toastTimer: number | undefined;
const embeddedPage: EmbeddedPageState = { frame: null, loadedUrl: "", frameReady: false, authConsumedFor: null, stale: false };
let market: MarketCatalog | null = null;
let installedPlugins: InstalledPlugin[] = [];
// 上次“检查更新”的结果，按插件名索引；卸载/更新成功后按名清除对应条目。
let pluginUpdates = new Map<string, PluginUpdateInfo>();
// 插件安装/卸载/更新共用的防重入标记：这些操作会停启服务并改写同一份
// profile，绝不能并发触发（后端另有互斥锁兜底，这里避免把请求排进队列）。
let pluginBusy = false;
const marketCategoryMeta: MarketMeta[] = [
  { id: "ui", label: "界面增强", color: "#a0c3ec" },
  { id: "agent-session", label: "Agent 与会话", color: "#c4b5fd" },
  { id: "development", label: "开发工具", color: "#ffffff" },
  { id: "communication", label: "消息通讯", color: "#ffc285" },
  { id: "data", label: "文件与数据", color: "#8ed6c4" },
  { id: "model-mcp", label: "模型与 MCP", color: "#9bb7ff" },
  { id: "security", label: "安全与治理", color: "#ff9c8c" },
  { id: "operations", label: "部署运维", color: "#d0d3d8" },
  { id: "lifestyle", label: "生活娱乐", color: "#ffb3d1" },
  { id: "research", label: "学习研究", color: "#b7d987" },
  { id: "other", label: "其他", color: "#7d8187" },
];
const marketTypeMeta: MarketMeta[] = [
  { id: "plugin", label: "插件" },
  { id: "skill", label: "技能" },
  { id: "collection", label: "插件合集" },
  { id: "channel", label: "渠道适配" },
  { id: "application", label: "完整应用" },
  { id: "infrastructure", label: "基础设施" },
  { id: "directory", label: "索引目录" },
  { id: "unknown", label: "待识别" },
];
const marketCategories = new Map(marketCategoryMeta.map((item) => [item.id, item]));
const marketTypes = new Map(marketTypeMeta.map((item) => [item.id, item]));
let marketShown = 0;
let marketSearchTimer: number | undefined;
let launcherVersion = "读取中";
let resizeSaveTimer: number | undefined;
const MARKET_PAGE = 120;
const marketFilter: { query: string; type: string; category: string; sort: "recent" | "stars" } = { query: "", type: "plugin", category: "all", sort: "recent" };

function toast(message: string, error = false): void {
  const node = $("#toast");
  node.textContent = message;
  node.dataset.error = String(error);
  node.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => { node.hidden = true; }, 4200);
}

function showDialog(id: DialogId): void {
  $(`#${id}`).hidden = false;
  if (id === "manage-dialog") void refreshPackageInfo();
  if (id === "config-dialog") void loadConfigFiles();
  if (id === "plugin-dialog") void loadInstalledPlugins();
  if (id === "settings-dialog") fillSettings();
}
function closeDialogs(): void {
  document.querySelectorAll<HTMLElement>(".modal").forEach((modal) => { modal.hidden = true; });
}

function fillForm(value: LauncherConfig): void {
  $<HTMLInputElement>(`input[name="launch_mode"][value="${value.launch_mode}"]`).checked = true;
  $("#executable").value = value.executable;
  $("#npx-package").value = value.npx_package;
  $("#working-directory").value = value.working_directory;
  $("#dsh-home").value = value.dsh_home;
  $("#port").value = String(value.port);
  updateModeFields();
}

function fillSettings(): void {
  $<HTMLInputElement>(`input[name="close_behavior"][value="${config.close_behavior}"]`).checked = true;
  $("#setting-summon-shortcut").value = config.summon_shortcut;
  $("#setting-auto-start").checked = config.auto_start;
  $("#setting-stop-dsh").checked = config.stop_dsh_on_exit;
  $("#setting-download-directory").value = config.download_directory;
  $("#setting-download-ask").checked = config.download_ask;
  $("#setting-download-choose").checked = config.download_choose_location;
  $("#setting-developer-mode").checked = config.developer_mode;
  $("#launcher-version").textContent = launcherVersion;
  if (currentWindow.label === "control") $("#window-close").title = config.close_behavior === "tray" ? "关闭到托盘" : "退出 Launcher";
}
function readSettings(): LauncherConfig {
  return { ...config,
    close_behavior: ($<HTMLInputElement>("input[name=close_behavior]:checked")).value as "tray" | "exit",
    summon_shortcut: $("#setting-summon-shortcut").value.trim(),
    auto_start: $("#setting-auto-start").checked,
    stop_dsh_on_exit: $("#setting-stop-dsh").checked,
    download_directory: $("#setting-download-directory").value.trim(),
    download_ask: $("#setting-download-ask").checked,
    download_choose_location: $("#setting-download-choose").checked,
    developer_mode: $("#setting-developer-mode").checked,
  };
}
async function saveSettings(): Promise<void> {
  try {
    config = await invoke<LauncherConfig>("save_config", { config: readSettings() });
    fillForm(config);
    $("#settings-save-result").textContent = "已保存";
    toast("设置已保存");
  } catch (error) { toast(String(error), true); }
}
function persistWindowSize(width: number, height: number): void {
  window.clearTimeout(resizeSaveTimer);
  resizeSaveTimer = window.setTimeout(() => {
    config.window_width = Math.round(width);
    config.window_height = Math.round(height);
    void invoke("save_window_size", { width: Math.round(width), height: Math.round(height) });
  }, 350);
}
function readForm(): LauncherConfig {
  const mode = ($<HTMLInputElement>("input[name=launch_mode]:checked")).value as LaunchMode;
  return { ...config, launch_mode: mode, executable: $("#executable").value.trim(), npx_package: $("#npx-package").value.trim(), working_directory: $("#working-directory").value.trim(), dsh_home: $("#dsh-home").value.trim(), port: Number($("#port").value) };
}
function updateModeFields(): void {
  const npx = ($<HTMLInputElement>("input[name=launch_mode]:checked")).value === "npx";
  $("#command-field").hidden = npx;
  $("#npx-field").hidden = !npx;
}

// 内嵌页面必须和宿主页面同站，认证 Cookie 才落得下来：跨站 iframe 的
// Set-Cookie 会被浏览器的第三方 Cookie 策略丢掉（WebView2 实测同样如此），
// 之后每个请求都是 401。dsh 地址里的回环 host 一律换成当前页面的 host。
function frameUrl(raw: string): string {
  const host = window.location.hostname;
  if (!host) return raw;
  try {
    const url = new URL(raw);
    if (url.hostname !== host) url.hostname = host;
    return url.toString();
  } catch {
    return raw;
  }
}

function syncFrame(): void {
  const ready = status.phase === "ready";
  // 占位层在服务未就绪、或页面尚未装载完成（首次装载/重载期间）时都保持可见，
  // 否则会露出宿主页面的亮色底，看起来像一段莫名的白屏。
  $("#workspace-state").hidden = Boolean(ready && embeddedPage.frame && embeddedPage.frameReady);
  // dsh 自 0.1.2 起给 Web 加了浏览器认证：内嵌页面必须先用启动时打印的带令牌
  // 地址换一次 cookie，裸地址一律 401。没有令牌地址（老版本，或已经换过 cookie）
  // 时沿用干净地址。
  const target = frameUrl(status.web_url ?? status.url);
  if (ready) {
    if (!embeddedPage.frame) {
      const frame = document.createElement("iframe");
      frame.className = "dsh-frame";
      frame.title = "DeepSeek Harness WebUI";
      frame.setAttribute("allow", "clipboard-read; clipboard-write");
      $("#workspace-view").appendChild(frame);
      // 新 iframe 从空白装载 WebUI 的过程会产生白屏，先隐藏，首次 load 完成后才揭晓。
      frame.hidden = true;
      frame.addEventListener("load", () => {
        embeddedPage.frameReady = true;
        frame.hidden = false;
        syncFrame();
        consumeAuthUrl();
      });
      embeddedPage.frame = frame;
    }
    // 只在地址变化或被标记过期时装载。iframe.src 的 getter 会规范化地址，
    // 因此单独记录已装载地址，避免每条状态事件都触发 WebUI 重载。
    const consumedAuth = !embeddedPage.stale
      && embeddedPage.authConsumedFor !== null
      && embeddedPage.authConsumedFor === target;
    if ((embeddedPage.loadedUrl !== target && !consumedAuth) || embeddedPage.stale) {
      embeddedPage.frameReady = false;
      embeddedPage.frame.hidden = true;
      embeddedPage.frame.src = target;
      embeddedPage.loadedUrl = target;
      embeddedPage.stale = false;
      embeddedPage.authConsumedFor = null;
    } else {
      embeddedPage.frame.hidden = false;
    }
  } else if (embeddedPage.frame) {
    embeddedPage.frame.hidden = true;
  }
  syncAuthPrompt();
}

// 带令牌的地址用完即弃：cookie 已经落进 WebView，那个一次性令牌在 dsh 下次
// 重启后必然失效。这里把已加载地址同步改回干净地址，避免立刻重载一次。
function consumeAuthUrl(): void {
  const used = status.web_url;
  if (!used || embeddedPage.loadedUrl !== frameUrl(used)) return;
  embeddedPage.authConsumedFor = embeddedPage.loadedUrl;
  embeddedPage.loadedUrl = frameUrl(status.url);
  void invoke<LauncherStatus>("auth_url_consumed").then(renderStatus).catch(() => undefined);
}
function renderStatus(next: LauncherStatus): void {
  // 去重守卫：launcher-status 事件高频可达（日志流/状态轮询），若关键字段未变则
  // 跳过本次整轮 DOM 更新，避免冗余重绘导致的主线程抖动。busy 必须参与比较：
  // OpsBusy 进入/退出维护态时只改这一个字段就广播，漏掉它会让“维护中”与
  // 启动/停止/重启的禁用状态永远不刷新。
  if (next.phase === status.phase && next.message === status.message && next.url === status.url && next.web_url === status.web_url && next.auth_required === status.auth_required && next.auth_satisfied === status.auth_satisfied && next.pid === status.pid && next.external === status.external && next.safe_mode === status.safe_mode && next.consecutive_failures === status.consecutive_failures && next.busy === status.busy) return;
  const wasReady = status.phase === "ready";
  // 认证开关重新亮起（例如外部 dsh 重启过）时，把「暂不处理」的收起状态复位。
  if (next.auth_required !== status.auth_required) authPromptClosed = false;
  status = next;
  const externalReady = next.phase === "ready" && next.external;
  // 互斥操作（插件安装/卸载/更新、dsh 更新）进行中：服务已被操作方停下并会
  // 自动恢复，启动/重启入口全部禁用（后端同样会拒绝，这里是可见的防呆）。
  const maintenance = Boolean(next.busy) && next.phase !== "ready";
  $("#service-state").textContent = next.phase === "ready" ? (next.external ? "运行中（外部）" : "运行中") : maintenance ? "维护中" : next.phase === "starting" ? "启动中" : next.phase === "failed" ? "启动失败" : "已停止";
  const state = $("#workspace-state");
  state.dataset.phase = next.phase;
  $("#workspace-title").textContent = maintenance ? (next.busy ?? "操作进行中") : next.phase === "failed" ? "dsh 启动失败" : next.phase === "starting" ? "正在启动 dsh" : next.phase === "ready" ? "正在打开 WebUI" : "dsh 尚未运行";
  $("#workspace-message").textContent = maintenance ? "操作完成后会自动恢复服务，请稍候。" : next.message;
  // 安全模式横幅与错误界面的安全模式入口。body 上的类让工作区高度同步
  // 扣掉横幅的 30px，否则内容被整体挤出屏幕、底部按钮被裁掉。
  document.body.classList.toggle("safe-mode-active", next.safe_mode);
  $("#safe-mode-banner").hidden = !next.safe_mode;
  $<HTMLButtonElement>("#workspace-safe").hidden = !(next.phase === "failed" && next.consecutive_failures >= 2);
  // 顶栏按钮随状态切换「进入 / 退出」。
  const toolbarSafe = $<HTMLButtonElement>("#toolbar-safe");
  toolbarSafe.querySelector("span")!.textContent = next.safe_mode ? "退出安全模式" : "安全模式";
  toolbarSafe.title = next.safe_mode ? "退出安全模式：删除一次性目录，用正式环境重新启动" : "进入安全模式：一次性隔离环境启动 dsh，不加载插件与正式数据";
  const startButton = $<HTMLButtonElement>("#workspace-start");
  startButton.toggleAttribute("disabled", maintenance || next.phase === "starting" || next.phase === "stopping");
  const actionLabel = maintenance ? (next.busy ?? "操作进行中") : next.phase === "failed" ? "重启 dsh" : next.phase === "starting" ? "正在连接 dsh" : "启动 dsh";
  startButton.title = actionLabel;
  startButton.ariaLabel = actionLabel;
  // 外部启动的服务不归 Launcher 管：停止/重启在这里没有意义，禁用并说明。
  const restartButton = $<HTMLButtonElement>("#manage-restart");
  const stopButton = $<HTMLButtonElement>("#manage-stop");
  const opsBusy = Boolean(next.busy);
  restartButton.toggleAttribute("disabled", opsBusy || next.phase === "starting" || next.phase === "stopping" || externalReady);
  stopButton.toggleAttribute("disabled", opsBusy || next.phase === "stopped" || next.phase === "stopping" || externalReady);
  restartButton.title = externalReady ? "服务由外部启动，Launcher 无法重启" : opsBusy ? "插件/更新操作进行中，完成后自动恢复" : "重启服务";
  stopButton.title = externalReady ? "服务由外部启动，Launcher 无法停止" : opsBusy ? "插件/更新操作进行中，完成后自动恢复" : "停止服务";
  // 服务重启后旧 iframe 内容已失效；进入就绪时重新装载当前页面。
  if (next.phase === "ready" && !wasReady) embeddedPage.stale = true;
  syncFrame();
}
// —— 浏览器认证提示 ——
// dsh 自 0.1.2 起要求先用启动时打印的带令牌地址换一次 cookie。启动器自己拉起的
// dsh 能从它的输出里认到那行地址；外部接管的拿不到，只能请用户粘贴一次。
// 这条提示本身可以忽略：后端的探测请求不带 WebView 的 cookie，页面能正常用时
// 关掉它即可。
function authPromptNeeded(): boolean {
  return status.phase === "ready" && status.auth_required && !status.auth_satisfied && !status.web_url;
}

function syncAuthPrompt(): void {
  $("#auth-prompt").hidden = !authPromptNeeded() || authPromptClosed;
}

// —— 顶栏主题跟随 ——
// 后端每 2 秒采样 dsh 页面在顶栏正下方的颜色推过来（dsh-theme 事件），这里把
// 标题栏整套颜色变量重算一遍，日间/夜间切换时顶栏无缝跟随。弹窗开着时先不
// 套用（遮罩会污染采样色），等弹窗关掉后由观察器补上最后一次采样结果。
function mixColor(base: string, other: string, ratio: number): string {
  const a = [1, 3, 5].map((i) => parseInt(base.slice(i, i + 2), 16));
  const b = [1, 3, 5].map((i) => parseInt(other.slice(i, i + 2), 16));
  const mixed = a.map((v, i) => Math.round(v * (1 - ratio) + b[i] * ratio));
  return "#" + mixed.map((v) => v.toString(16).padStart(2, "0")).join("");
}
let pendingTitlebarTheme: string | null = null;
function applyTitlebarTheme(hex: string): void {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  const dark = 0.299 * r + 0.587 * g + 0.114 * b < 128;
  const text = dark ? "#e8e8ea" : "#333338";
  const root = document.documentElement.style;
  root.setProperty("--titlebar-bg", hex);
  root.setProperty("--titlebar-text", text);
  root.setProperty("--titlebar-muted", mixColor(text, hex, 0.4));
  root.setProperty("--titlebar-hover", mixColor(hex, dark ? "#ffffff" : "#000000", 0.1));
  root.setProperty("--titlebar-border", mixColor(hex, dark ? "#ffffff" : "#000000", 0.16));
  root.setProperty("--titlebar-border-strong", mixColor(hex, dark ? "#ffffff" : "#000000", 0.24));
}
void listen("dsh-theme", (event) => {
  const hex = String(event.payload);
  pendingTitlebarTheme = hex;
  if (document.querySelector(".modal:not([hidden])")) return;
  applyTitlebarTheme(hex);
});
const modalThemeObserver = new MutationObserver(() => {
  if (!document.querySelector(".modal:not([hidden])") && pendingTitlebarTheme) applyTitlebarTheme(pendingTitlebarTheme);
});
document.querySelectorAll<HTMLElement>(".modal").forEach((modal) => modalThemeObserver.observe(modal, { attributes: true, attributeFilter: ["hidden"] }));

async function submitAuthUrl(): Promise<void> {
  const input = $("#auth-url");
  const value = input.value.trim();
  if (!value) {
    toast("先粘贴 dsh 打印的那行地址", true);
    return;
  }
  try {
    renderStatus(await invoke<LauncherStatus>("submit_auth_url", { url: value }));
    input.value = "";
    authPromptClosed = false;
  } catch (error) {
    toast(String(error), true);
  }
}

$("#auth-close").addEventListener("click", () => {
  authPromptClosed = true;
  syncAuthPrompt();
});
$("#auth-submit").addEventListener("click", () => { void submitAuthUrl(); });
$("#auth-url").addEventListener("keydown", (event) => {
  if (event.key === "Enter") void submitAuthUrl();
});

function refreshWeb(): void {
  if (status.phase !== "ready" || !embeddedPage.frame || !embeddedPage.loadedUrl) { toast("dsh Web 服务尚未就绪", true); return; }
  // 跨域 iframe 访问 contentWindow.location 会抛 SecurityError，重新赋值 src 触发刷新。
  embeddedPage.frame.src = embeddedPage.loadedUrl;
}

async function runAction(command: "start_dsh" | "stop_dsh" | "restart_dsh"): Promise<void> {
  try { renderStatus(await invoke<LauncherStatus>(command)); } catch (error) { toast(String(error), true); }
}
async function refreshPackageInfo(): Promise<void> {
  $("#package-source").textContent = "检查中…";
  try {
    const info = await invoke<PackageInfo>("get_package_info");
    $("#current-version").textContent = info.current_version;
    $("#latest-version").textContent = info.latest_version;
    // detail 里带着 dsh --version 的原始输出与版本解析的结果/失败原因；
    // 查询失败时用户至少能悬停看到是网络问题还是包名写错了。
    $("#latest-version").title = info.detail;
    $("#package-source").textContent = info.source;
  } catch (error) { $("#package-source").textContent = "检查失败"; toast(String(error), true); }
}

async function saveStartupConfig(): Promise<void> {
  try {
    config = await invoke<LauncherConfig>("save_config", { config: readForm() });
    await runAction("restart_dsh");
    toast("启动配置已保存");
  } catch (error) { toast(String(error), true); }
}

async function loadConfigFiles(): Promise<void> {
  try {
    configFiles = await invoke<ConfigFileInfo[]>("list_config_files");
    const select = $("#config-file-select");
    select.innerHTML = configFiles.map((file) => `<option value="${file.id}">${file.name}</option>`).join("");
    renderConfigFile();
  } catch (error) { toast(String(error), true); }
}
function renderConfigFile(): void {
  const file = configFiles.find((item) => item.id === $("#config-file-select").value) ?? configFiles[0];
  if (!file) return;
  $("#config-file-path").textContent = file.path;
  $("#config-editor").value = file.content;
}
async function saveConfigFile(): Promise<void> {
  const id = $("#config-file-select").value;
  try {
    const file = await invoke<ConfigFileInfo>("save_dsh_config", { id, content: $("#config-editor").value });
    configFiles = configFiles.map((item) => item.id === file.id ? file : item);
    $("#config-save-result").textContent = "已保存";
    toast(`${file.name} 已保存`);
  } catch (error) { $("#config-save-result").textContent = String(error); toast(String(error), true); }
}

async function loadInstalledPlugins(): Promise<void> {
  const node = $("#installed-plugins");
  node.innerHTML = `<div class="loading"><i data-lucide="loader-circle"></i>正在读取 profile</div>`;
  createIcons({ icons: { LoaderCircle } });
  try {
    installedPlugins = await invoke<InstalledPlugin[]>("list_plugins");
    renderInstalledPlugins();
  } catch (error) { node.textContent = String(error); }
}
const PLUGIN_CHANNEL_LABELS: Record<string, string> = { npm: "npm", github: "GitHub", git: "Git", local: "本地", alias: "npm 别名" };
function installedPluginCard(plugin: InstalledPlugin): string {
  const update = pluginUpdates.get(plugin.name);
  const badge = !update ? "" : update.update_available
    ? `<span class="plugin-badge update" title="${escapeAttr(update.detail)}">可更新 ${escapeHtml(update.latest_version)}</span>`
    : update.latest_version
      ? `<span class="plugin-badge ok" title="${escapeAttr(update.detail)}">已是最新</span>`
      : `<span class="plugin-badge" title="${escapeAttr(update.detail)}">无法检测</span>`;
  const meta = [
    plugin.version,
    plugin.installed_version && plugin.installed_version !== plugin.version ? `当前 ${plugin.installed_version}` : "",
    PLUGIN_CHANNEL_LABELS[plugin.channel] ?? plugin.channel,
    plugin.bundle ? "bundle" : "",
  ].filter(Boolean).map((part) => escapeHtml(part)).join(" · ");
  const updateTitle = plugin.channel === "npm" ? "更新到最新版本" : "更新（重新解析来源并安装最新内容）";
  const updateButton = plugin.update_spec
    ? `<button class="icon-button update-plugin" data-name="${escapeAttr(plugin.name)}" data-spec="${escapeAttr(plugin.update_spec)}" title="${updateTitle}"><i data-lucide="refresh-cw"></i></button>`
    : "";
  return `<article class="plugin-item"><div><strong>${escapeHtml(plugin.name)}</strong>${badge}<small>${meta}</small></div><div class="plugin-actions">${updateButton}<button class="icon-button danger remove-plugin" data-spec="${escapeAttr(plugin.name)}" title="卸载"><i data-lucide="trash-2"></i></button></div></article>`;
}
function renderInstalledPlugins(): void {
  const node = $("#installed-plugins");
  if (!installedPlugins.length) {
    node.innerHTML = `<div class="empty-state"><i data-lucide="puzzle"></i><p>Web profile 暂无额外插件</p><small>从插件商店选择一个 GitHub 项目开始。</small></div>`;
    createIcons({ icons: { Puzzle } });
    return;
  }
  const updatable = installedPlugins.filter((plugin) => plugin.update_spec && pluginUpdates.get(plugin.name)?.update_available);
  const updateAll = updatable.length
    ? `<button id="update-all-plugins" class="button primary"><i data-lucide="download"></i><span>全部更新（${updatable.length}）</span></button>`
    : "";
  node.innerHTML = `<div class="installed-toolbar"><span>${installedPlugins.length} 个插件</span>${updateAll}<button id="check-plugin-updates" class="button quiet"><i data-lucide="refresh-cw"></i><span>检查更新</span></button></div>${installedPlugins.map(installedPluginCard).join("")}`;
  createIcons({ icons: { Download, RefreshCw, Trash2 } });
  node.querySelectorAll<HTMLButtonElement>(".remove-plugin").forEach((button) => button.addEventListener("click", () => void removePlugin(button.dataset.spec ?? "")));
  node.querySelectorAll<HTMLButtonElement>(".update-plugin").forEach((button) =>
    button.addEventListener("click", () => void updatePlugins([{ name: button.dataset.name ?? "", spec: button.dataset.spec ?? "" }])));
  node.querySelector<HTMLButtonElement>("#check-plugin-updates")?.addEventListener("click", () => void checkPluginUpdates());
  node.querySelector<HTMLButtonElement>("#update-all-plugins")?.addEventListener("click", () =>
    void updatePlugins(updatable.map((plugin) => ({ name: plugin.name, spec: plugin.update_spec }))));
}
async function checkPluginUpdates(): Promise<void> {
  const button = document.querySelector<HTMLButtonElement>("#check-plugin-updates");
  if (button) { button.disabled = true; const label = button.querySelector("span"); if (label) label.textContent = "检查中…"; }
  try {
    const infos = await invoke<PluginUpdateInfo[]>("check_plugin_updates");
    pluginUpdates = new Map(infos.map((info) => [info.name, info]));
    const count = infos.filter((info) => info.update_available).length;
    const failed = infos.filter((info) => info.detail.startsWith("检查失败")).length;
    toast(count ? `发现 ${count} 个插件可更新` : failed ? `未发现可更新的插件（${failed} 个检查失败，可悬停徽标查看原因）` : "所有插件均为已知最新版本");
  } catch (error) { toast(String(error), true); }
  // 重绘会重建工具栏按钮，顺带恢复“检查更新”的可用状态。
  renderInstalledPlugins();
}
// 串行化插件操作：并发的安装/卸载/更新会互相踩踏 profile 和服务停启。
async function withPluginOps(run: () => Promise<void>): Promise<void> {
  if (pluginBusy) { toast("已有插件操作正在进行，请等它完成后再试", true); return; }
  pluginBusy = true;
  document.querySelector(".plugin-panel")?.classList.add("busy");
  try { await run(); } finally {
    pluginBusy = false;
    document.querySelector(".plugin-panel")?.classList.remove("busy");
  }
}
async function installPlugin(spec = $("#plugin-spec").value.trim()): Promise<void> {
  if (!spec) { toast("请输入 npm 包名或 github:owner/repository", true); return; }
  await withPluginOps(async () => {
    const output = $("#plugin-output"); output.hidden = false; output.textContent = `正在安装 ${spec}…`;
    try {
      const result = await invoke<OperationResult>("install_plugin", { spec });
      output.textContent = result.output || (result.success ? "安装完成" : "安装失败");
      toast(result.success ? (status.external ? "插件安装完成（当前沿用外部服务，需自行重启该服务生效）" : "插件安装完成，服务已重新启动") : "插件安装失败", !result.success);
      if (result.success) { $("#plugin-spec").value = ""; await loadInstalledPlugins(); }
    } catch (error) { output.textContent = String(error); toast(String(error), true); }
  });
}
async function removePlugin(spec: string): Promise<void> {
  if (!spec) return;
  const question = `确定卸载插件 ${spec} 吗？`;
  const confirmed = await ask(question, { title: "DSH Launcher", kind: "warning" }).catch(() => window.confirm(question));
  if (!confirmed) return;
  await withPluginOps(async () => {
    const output = $("#plugin-output"); output.hidden = false; output.textContent = `正在卸载 ${spec}…`;
    try {
      const result = await invoke<OperationResult>("remove_plugin", { spec });
      output.textContent = result.output || (result.success ? "卸载完成" : "卸载失败");
      toast(result.success ? (status.external ? "插件已卸载（当前沿用外部服务，需自行重启该服务生效）" : "插件已卸载，服务已重新启动") : "插件卸载失败", !result.success);
      if (result.success) { pluginUpdates.delete(spec); await loadInstalledPlugins(); }
    } catch (error) { output.textContent = String(error); toast(String(error), true); }
  });
}
async function updatePlugins(targets: { name: string; spec: string }[]): Promise<void> {
  const valid = targets.filter((target) => target.spec);
  if (!valid.length) return;
  await withPluginOps(async () => {
    const label = valid.length === 1 ? valid[0].name : `${valid.length} 个插件`;
    const output = $("#plugin-output"); output.hidden = false; output.textContent = `正在更新 ${label}…`;
    try {
      const result = await invoke<OperationResult>("update_plugins", { specs: valid.map((target) => target.spec) });
      output.textContent = result.output || (result.success ? "更新完成" : "更新失败");
      toast(result.success ? (status.external ? `${label} 已更新（当前沿用外部服务，需自行重启该服务生效）` : `${label} 已更新，服务已重新启动`) : "插件更新失败，详见输出", !result.success);
      if (result.success) {
        valid.forEach((target) => pluginUpdates.delete(target.name));
        await loadInstalledPlugins();
      }
    } catch (error) { output.textContent = String(error); toast(String(error), true); }
  });
}
async function searchPlugins(): Promise<void> {
  const node = $("#plugin-search-results"); node.innerHTML = `<div class="loading">正在查询 npm…</div>`;
  try {
    const results = await invoke<PluginSearchResult[]>("search_plugins", { query: $("#plugin-search-input").value.trim() });
    node.innerHTML = results.length ? results.map((item) => `<article class="search-item"><div><strong>${escapeHtml(item.name)}</strong><small>v${escapeHtml(item.version)}</small><p>${escapeHtml(item.description)}</p></div><button class="button quiet search-install" data-spec="${escapeAttr(item.name)}"><i data-lucide="download"></i><span>安装</span></button></article>`).join("") : `<div class="empty-state">没有找到带 dsh plugin 标签的 npm 包。</div>`;
    createIcons({ icons: { Download } });
    node.querySelectorAll<HTMLButtonElement>(".search-install").forEach((button) => button.addEventListener("click", () => void installPlugin(button.dataset.spec ?? "")));
  } catch (error) { node.textContent = String(error); }
}
function escapeHtml(value: string): string { return value.replace(/[&<>"']/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#039;" }[char] ?? char)); }
function escapeAttr(value: string): string { return escapeHtml(value).replace(/`/g, "&#096;"); }

// —— 插件商店（数据来自 dsh.aitreez.com API，后端获取并缓存）——
function safeColor(value: string | null | undefined): string {
  return value && /^#[0-9a-fA-F]{3,8}$/.test(value) ? value : "#7d8187";
}
function safeHttpUrl(value: string): string {
  return /^https?:\/\//i.test(value) ? value : "";
}
function formatStars(count: number): string {
  return count >= 1000 ? `${(count / 1000).toFixed(1).replace(/\.0$/, "")}k` : String(count);
}
function timeAgo(iso: string): string {
  const time = Date.parse(iso);
  if (Number.isNaN(time)) return "";
  const days = Math.floor((Date.now() - time) / 86_400_000);
  if (days <= 0) return "今天活跃";
  if (days < 30) return `${days} 天前活跃`;
  if (days < 365) return `${Math.floor(days / 30)} 个月前活跃`;
  return `${Math.floor(days / 365)} 年前活跃`;
}

function marketCard(plugin: MarketPlugin): string {
  const category = marketCategories.get(plugin.category);
  const typeLabel = marketTypes.get(plugin.project_type)?.label;
  const badges = [
    plugin.verified ? `<span class="market-badge verified">已验证</span>` : "",
    plugin.archived ? `<span class="market-badge archived">已归档</span>` : "",
    category ? `<span class="market-badge tinted" style="--badge-color:${safeColor(category.color)}">${escapeHtml(category.label)}</span>` : "",
    typeLabel && marketFilter.type === "all" ? `<span class="market-badge">${escapeHtml(typeLabel)}</span>` : "",
  ].join("");
  const meta = [`★ ${formatStars(plugin.stars)}`, plugin.language, timeAgo(plugin.pushed_at)]
    .filter(Boolean).map((part) => escapeHtml(String(part))).join(" · ");
  const avatar = safeHttpUrl(plugin.avatar_url);
  const repo = safeHttpUrl(plugin.url);
  return `<article class="market-item">
    <img class="market-avatar" ${avatar ? `src="${escapeAttr(avatar)}"` : ""} alt="" loading="lazy">
    <div class="market-info">
      <div class="market-title"><strong>${escapeHtml(plugin.name)}</strong><small>${escapeHtml(plugin.full_name)}</small>${badges}</div>
      <p class="market-desc">${escapeHtml(plugin.description || "无描述")}</p>
      <div class="market-meta">${meta}</div>
    </div>
    <div class="market-actions">
      ${repo ? `<a class="icon-button" data-external href="${escapeAttr(repo)}" title="在 GitHub 查看"><i data-lucide="external-link"></i></a>` : ""}
      <button class="button quiet market-install" data-spec="${escapeAttr(plugin.spec)}" title="安装到 web profile"><i data-lucide="download"></i><span>安装</span></button>
    </div>
  </article>`;
}

function filteredMarket(): MarketPlugin[] {
  if (!market) return [];
  let items = market.plugins;
  if (marketFilter.type !== "all") items = items.filter((plugin) => plugin.project_type === marketFilter.type);
  if (marketFilter.category !== "all") items = items.filter((plugin) => plugin.category === marketFilter.category);
  if (marketFilter.query) {
    const query = marketFilter.query;
    items = items.filter((plugin) =>
      `${plugin.name} ${plugin.full_name} ${plugin.description} ${plugin.language} ${plugin.topics.join(" ")}`.toLowerCase().includes(query));
  }
  if (marketFilter.sort === "stars") {
    items = [...items].sort((a, b) => b.stars - a.stars);
  } else {
    items = [...items].sort((a, b) => {
      const aTime = Date.parse(a.pushed_at);
      const bTime = Date.parse(b.pushed_at);
      if (Number.isNaN(aTime)) return Number.isNaN(bTime) ? 0 : 1;
      if (Number.isNaN(bTime)) return -1;
      return bTime - aTime;
    });
  }
  return items;
}

function renderMarketList(): void {
  if (!market) return;
  const node = $("#market-list");
  const items = filteredMarket();
  const visible = items.slice(0, marketShown);
  node.innerHTML = visible.length
    ? visible.map(marketCard).join("")
    : `<div class="empty-state"><i data-lucide="puzzle"></i><p>没有匹配的插件</p><small>换个关键词或筛选条件试试。</small></div>`;
  createIcons({ icons: { Download, ExternalLink, Puzzle } });
  node.querySelectorAll<HTMLImageElement>(".market-avatar[src]").forEach((img) =>
    img.addEventListener("error", () => img.removeAttribute("src")));
  node.querySelectorAll<HTMLButtonElement>(".market-install").forEach((button) =>
    button.addEventListener("click", async () => {
      button.disabled = true;
      try { await installPlugin(button.dataset.spec ?? ""); } finally { button.disabled = false; }
    }));
  $("#market-count").textContent = `${items.length} 个项目 · 共收录 ${market.plugins.length}`;
  const more = $("#market-more");
  more.hidden = items.length <= marketShown;
  if (!more.hidden) more.textContent = `加载更多（还有 ${items.length - marketShown} 个）`;
}

function resetMarketList(): void {
  marketShown = MARKET_PAGE;
  renderMarketList();
}

function populateMarketFilters(): void {
  if (!market) return;
  const typeCounts = new Map<string, number>();
  const categoryCounts = new Map<string, number>();
  for (const plugin of market.plugins) {
    typeCounts.set(plugin.project_type, (typeCounts.get(plugin.project_type) ?? 0) + 1);
    categoryCounts.set(plugin.category, (categoryCounts.get(plugin.category) ?? 0) + 1);
  }
  const typeSelect = $<HTMLSelectElement>("#market-type");
  typeSelect.innerHTML = [
    `<option value="all">全部类型 (${market.plugins.length})</option>`,
    ...marketTypeMeta.filter((item) => typeCounts.has(item.id))
      .map((item) => `<option value="${escapeAttr(item.id)}">${escapeHtml(item.label)} (${typeCounts.get(item.id)})</option>`),
  ].join("");
  if (marketFilter.type !== "all" && !typeCounts.has(marketFilter.type)) marketFilter.type = "all";
  typeSelect.value = marketFilter.type;
  const categorySelect = $<HTMLSelectElement>("#market-category");
  categorySelect.innerHTML = [
    `<option value="all">全部分类</option>`,
    ...marketCategoryMeta.filter((item) => categoryCounts.has(item.id))
      .map((item) => `<option value="${escapeAttr(item.id)}">${escapeHtml(item.label)} (${categoryCounts.get(item.id)})</option>`),
  ].join("");
  if (marketFilter.category !== "all" && !categoryCounts.has(marketFilter.category)) marketFilter.category = "all";
  categorySelect.value = marketFilter.category;
}

async function loadMarket(force: boolean): Promise<boolean> {
  const node = $("#market-list");
  if (!market) {
    node.innerHTML = `<div class="loading"><i data-lucide="loader-circle"></i>正在载入插件商店…</div>`;
    createIcons({ icons: { LoaderCircle } });
  }
  try {
    market = await invoke<MarketCatalog>("fetch_market", { force });
    populateMarketFilters();
    resetMarketList();
    return true;
  } catch (error) {
    if (market) { toast(String(error), true); return false; }
    node.innerHTML = `<div class="empty-state"><i data-lucide="circle-alert"></i><p>插件商店加载失败</p><small>${escapeHtml(String(error))}</small><button id="market-retry" class="button quiet">重试</button></div>`;
    createIcons({ icons: { CircleAlert } });
    $("#market-retry").addEventListener("click", () => void loadMarket(true));
    return false;
  }
}

document.querySelectorAll<HTMLButtonElement>("[data-dialog]").forEach((button) => button.addEventListener("click", () => showDialog(button.dataset.dialog as DialogId)));
document.querySelectorAll<HTMLElement>("[data-close-modal]").forEach((node) => node.addEventListener("click", closeDialogs));
document.querySelectorAll<HTMLButtonElement>(".modal-close").forEach((button) => button.addEventListener("click", closeDialogs));
document.querySelectorAll<HTMLInputElement>("input[name=launch_mode]").forEach((input) => input.addEventListener("change", updateModeFields));
$("#config-file-select").addEventListener("change", renderConfigFile);
$("#save-config-file").addEventListener("click", () => void saveConfigFile());
$("#open-config-file").addEventListener("click", () => void invoke("open_dsh_config", { id: $("#config-file-select").value }));
$("#manage-save").addEventListener("click", () => void saveStartupConfig());
$("#save-settings").addEventListener("click", () => void saveSettings());
$("#check-package").addEventListener("click", () => void refreshPackageInfo());
$("#update-package").addEventListener("click", async () => {
  const output = $("#manage-output");
  output.hidden = false;
  output.textContent = "正在更新 dsh…（运行中的服务会先停止，更新后自动恢复）";
  try {
    const result = await invoke<OperationResult>("update_dsh");
    output.textContent = result.output || (result.success ? "更新完成" : "更新失败");
    toast(result.success ? "dsh 更新完成" : "dsh 更新失败", !result.success);
    if (result.success) await refreshPackageInfo();
  } catch (error) {
    output.textContent = String(error);
    toast(String(error), true);
  }
});
$("#manage-restart").addEventListener("click", () => void runAction("restart_dsh"));
$("#manage-stop").addEventListener("click", () => void runAction("stop_dsh"));
$("#workspace-start").addEventListener("click", () => void runAction("start_dsh"));
$("#workspace-safe").addEventListener("click", () => void invoke("start_safe_mode").catch((error) => toast(String(error), true)));
$("#exit-safe-mode").addEventListener("click", () => void invoke("exit_safe_mode").catch((error) => toast(String(error), true)));
$("#toolbar-safe").addEventListener("click", () => void invoke(status.safe_mode ? "exit_safe_mode" : "start_safe_mode").catch((error) => toast(String(error), true)));
$("#plugin-spec").addEventListener("keydown", (event) => { if (event.key === "Enter") void installPlugin(); });
$("#install-plugin").addEventListener("click", () => void installPlugin());
$("#plugin-search-button").addEventListener("click", () => void searchPlugins());
$("#plugin-search-input").addEventListener("keydown", (event) => { if (event.key === "Enter") void searchPlugins(); });
document.querySelectorAll<HTMLButtonElement>(".plugin-tab").forEach((tab) => tab.addEventListener("click", () => {
  document.querySelectorAll<HTMLButtonElement>(".plugin-tab").forEach((item) => item.classList.toggle("active", item === tab));
  document.querySelectorAll<HTMLElement>(".plugin-content").forEach((item) => { item.hidden = item.id !== `${tab.dataset.pluginTab}-plugins`; });
  if (tab.dataset.pluginTab === "market" && !market) void loadMarket(false);
}));
$("#market-search").addEventListener("input", () => {
  window.clearTimeout(marketSearchTimer);
  marketSearchTimer = window.setTimeout(() => {
    marketFilter.query = $("#market-search").value.trim().toLowerCase();
    resetMarketList();
  }, 160);
});
$<HTMLSelectElement>("#market-type").addEventListener("change", () => { marketFilter.type = $<HTMLSelectElement>("#market-type").value; resetMarketList(); });
$<HTMLSelectElement>("#market-category").addEventListener("change", () => { marketFilter.category = $<HTMLSelectElement>("#market-category").value; resetMarketList(); });
$<HTMLSelectElement>("#market-sort").addEventListener("change", () => { marketFilter.sort = $<HTMLSelectElement>("#market-sort").value as "recent" | "stars"; resetMarketList(); });
$("#market-refresh").addEventListener("click", async () => {
  const button = $<HTMLButtonElement>("#market-refresh");
  button.disabled = true;
  try { if (await loadMarket(true)) toast("插件商店数据已刷新"); } finally { button.disabled = false; }
});
$("#market-more").addEventListener("click", () => { marketShown += MARKET_PAGE; renderMarketList(); });
// webview 里外部链接不会自己打开，统一交给系统浏览器。
document.addEventListener("click", (event) => {
  const anchor = (event.target as HTMLElement | null)?.closest?.("a[data-external]");
  if (anchor instanceof HTMLAnchorElement && anchor.href) {
    event.preventDefault();
    void invoke("open_external", { url: anchor.href }).catch((error) => toast(String(error), true));
  }
});
$("#browse-workspace").addEventListener("click", async () => { const selected = await open({ directory: true, multiple: false, defaultPath: $("#working-directory").value || undefined }); if (typeof selected === "string") $("#working-directory").value = selected; });
$("#browse-download-directory").addEventListener("click", async () => { const selected = await open({ directory: true, multiple: false, defaultPath: $("#setting-download-directory").value || undefined }); if (typeof selected === "string") $("#setting-download-directory").value = selected; });
$("#refresh-web").addEventListener("click", refreshWeb);
$("#new-window").addEventListener("click", () => void invoke("new_launcher_window").catch((error) => toast(String(error), true)));
$("#window-minimize").addEventListener("click", () => void currentWindow.minimize());
$("#window-maximize").addEventListener("click", () => void currentWindow.toggleMaximize());
$("#window-close").addEventListener("click", () => void currentWindow.close());
// 只有主窗口关闭时驻留托盘，新建窗口是真正关闭。
if (currentWindow.label !== "control") $("#window-close").title = "关闭窗口";
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") { closeDialogs(); return; }
});

async function revealWindow(): Promise<void> {
  // 窗口以隐藏状态创建（tauri.conf.json / spawn_launcher_window），首帧内容
  // 就绪后才显示，避免 WebView 初始化期间的白屏；失败时由后端兜底定时器接管。
  try {
    await currentWindow.show();
    await currentWindow.setFocus();
  } catch { /* 后端兜底 */ }
}

async function init(): Promise<void> {
  await listen<LauncherStatus>("launcher-status", (event) => renderStatus(event.payload));
  // 托盘菜单唤出的功能：打开对应面板 / 刷新页面（只在主窗口处理）。
  await listen<string>("open-dialog", (event) => { if (currentWindow.label === "control") showDialog(event.payload as DialogId); });
  await listen("refresh-web", () => { if (currentWindow.label === "control") refreshWeb(); });
  const [loadedConfig, loadedStatus, version] = await Promise.all([
    invoke<LauncherConfig>("load_config"),
    invoke<LauncherStatus>("get_status"),
    invoke<string>("get_launcher_version"),
  ]);
  config = loadedConfig;
  launcherVersion = version;
  if (currentWindow.label === "control") {
    // 下限 1024：dsh WebUI 在更窄的视口会切到移动端布局，窗口变宽后已渲染的
    // 移动端元素会以裸样式残留（见 windows_ui.rs 同款注释）。
    const width = Math.max(1024, Math.min(4096, config.window_width || 1100));
    const height = Math.max(560, Math.min(4096, config.window_height || 760));
    await currentWindow.setSize(new LogicalSize(width, height));
    await currentWindow.onResized(async ({ payload }) => {
      if (await currentWindow.isMaximized() || await currentWindow.isFullscreen()) return;
      const logical = payload.toLogical(await currentWindow.scaleFactor());
      persistWindowSize(logical.width, logical.height);
    });
  }
  fillForm(config);
  fillSettings();
  renderStatus(loadedStatus);
  await revealWindow();
  // 有互斥操作在进行时不自动启动：操作方会在结束后自行恢复服务。
  if (config.auto_start && loadedStatus.phase === "stopped" && !loadedStatus.busy) await runAction("start_dsh");
}
void init().catch((error) => { void revealWindow(); toast(String(error), true); });

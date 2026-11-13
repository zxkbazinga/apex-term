<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import packageJson from "../package.json";
import {
  Check, ChevronDown, Download, File, Folder, FolderPlus, Plus,
  RefreshCw, Server, Settings2, TerminalSquare, Trash2, Upload, X,
} from "lucide-vue-next";
import "@xterm/xterm/css/xterm.css";

type HostProfile = { id: string; name: string; host: string; port: number; username: string; fingerprint?: string };
type SessionKind = "local" | "ssh";
type ConnectionState = "connected" | "connecting" | "disconnected";
type TerminalSession = { id: string; kind: SessionKind; profile: HostProfile };
type ThemeName = "graphite" | "midnight" | "paper" | "homebrew" | "dracula" | "nord" | "solarized" | "monokai" | "macos-glass";
type UpdateStatus = "idle" | "checking" | "current" | "available" | "error";
type LatestRelease = { version: string; url: string };
type SftpEntry = { name: string; path: string; isDir: boolean; size: number | null; modified: number | null };
type TerminalRuntime = {
  terminal: Terminal;
  fitAddon: FitAddon;
  outputUnlisten?: () => void;
  connectionEndedUnlisten?: () => void;
  resizeObserver: ResizeObserver;
  resizeTimer?: ReturnType<typeof setTimeout>;
  lastPtySize?: { cols: number; rows: number };
  pendingInput: string;
  inputFlushTimer?: ReturnType<typeof setTimeout>;
  writeQueue: Promise<void>;
  flushInput: () => void;
  clipboardKeyHandler: (event: KeyboardEvent) => void;
};
const APP_VERSION = packageJson.version;
const APP_AUTHOR = "Zzbazinga";
const MAX_SFTP_FILE_SIZE = 32 * 1024 * 1024;
const LATEST_RELEASE_URL = "https://api.github.com/repos/zxkbazinga/apex-term/releases/latest";
const DISMISSED_UPDATE_VERSION_KEY = "apex-dismissed-update-version";

const profiles = ref<HostProfile[]>([]);
const connectionStates = reactive<Record<string, ConnectionState>>({});
const sessionStates = reactive<Record<string, ConnectionState>>({});
const sessions = ref<TerminalSession[]>([]);
const activeSessionId = ref<string | null>(null);
const activeSession = computed(() => sessions.value.find((session) => session.id === activeSessionId.value) || null);
const runtimeAvailable = ref(isTauri());
const availableThemes: { id: ThemeName; label: string }[] = [
  { id: "graphite", label: "石墨灰" },
  { id: "homebrew", label: "Homebrew" },
  { id: "midnight", label: "午夜" },
  { id: "dracula", label: "Dracula" },
  { id: "nord", label: "Nord" },
  { id: "solarized", label: "Solarized Dark" },
  { id: "monokai", label: "Monokai" },
  { id: "macos-glass", label: "macOS 玻璃" },
  { id: "paper", label: "浅色" },
];
const savedTheme = localStorage.getItem("apex-theme") as ThemeName | null;
const themeMigrationKey = "apex-theme-homebrew-default-v2";
const migrateLegacyTheme = !localStorage.getItem(themeMigrationKey);
if (migrateLegacyTheme) {
  if (savedTheme === "graphite") localStorage.setItem("apex-theme", "homebrew");
  localStorage.setItem(themeMigrationKey, "1");
}
const theme = ref<ThemeName>(migrateLegacyTheme && savedTheme === "graphite" ? "homebrew" : availableThemes.some((item) => item.id === savedTheme) ? savedTheme! : "homebrew");
const isMacOS = navigator.platform.toLowerCase().includes("mac");
const isWindowsOS = navigator.platform.toLowerCase().includes("win");
const defaultFontFamily = isMacOS ? "MesloLGS NF" : "DejaVu Sans Mono";
const savedFontFamily = localStorage.getItem("apex-font");
const fontFamily = ref(!savedFontFamily || (isMacOS && savedFontFamily === "Cascadia Code") ? defaultFontFamily : savedFontFamily);
const fontSize = ref(Number(localStorage.getItem("apex-font-size") || 14));
const wallpaper = ref(localStorage.getItem("apex-wallpaper") || "");
const wallpaperTransparency = ref(Math.min(80, Math.max(0, Number(localStorage.getItem("apex-wallpaper-transparency") || 30))));
const wallpaperError = ref("");
const wallpaperInput = ref<HTMLInputElement | null>(null);
const search = ref("");
const isModalOpen = ref(false);
const windowMenuOpen = ref(false);
const settingsOpen = ref(false);
const sessionsOpen = ref(false);
const aboutOpen = ref(false);
const updateStatus = ref<UpdateStatus>("idle");
const latestRelease = ref<LatestRelease | null>(null);
const updateError = ref("");
const updateToastVisible = ref(false);
const hostTrustRequest = ref<{ sessionId: string; profile: HostProfile; password: string; fingerprint: string; knownFingerprint?: string } | null>(null);
const sftpOpen = ref(false);
const sftpPath = ref(".");
const sftpEntries = ref<SftpEntry[]>([]);
const sftpLoading = ref(false);
const sftpError = ref("");
const uploadInput = ref<HTMLInputElement | null>(null);
const editingProfileId = ref<string | null>(null);
const profileToDelete = ref<HostProfile | null>(null);
const profileDeleteError = ref("");
const profileDeleteLoading = ref(false);
const formError = ref("");
const testError = ref("");
const testStatus = ref("");
const testLoading = ref(false);
const testFingerprintToTrust = ref("");
const profileFormFingerprint = ref<string | undefined>();
const profileFingerprintEndpoint = ref<string | null>(null);
const profileForm = reactive({ name: "", host: "", port: 22, username: "", password: "" });

const credentialCache = new Map<string, string>();
const sshPasswords = new Map<string, string>();
const terminalElements = new Map<string, HTMLElement>();
const terminalRuntimes = new Map<string, TerminalRuntime>();
const pendingPasswords = new Map<string, string>();
let resolveHostTrust: (() => void) | undefined;
let rejectHostTrust: ((error: Error) => void) | undefined;
let unlistenNativeMenu: (() => void) | undefined;

function handleApplicationShortcut(event: KeyboardEvent) {
  if (event.repeat || (!event.metaKey && !event.ctrlKey)) return;
  const key = event.key.toLowerCase();
  if (key === "q" && runtimeAvailable.value) {
    event.preventDefault();
    event.stopImmediatePropagation();
    void exitApplication();
    return;
  }
  if (key === "v") {
    const target = event.target;
    if (target instanceof HTMLElement && target.closest(".terminal-mount")) return;
    if (!(target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement)) return;
    if (target instanceof HTMLInputElement && !["text", "search", "url", "tel", "password", "number"].includes(target.type)) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    const clipboardText = isTauri() ? readText() : navigator.clipboard.readText();
    void clipboardText.then((text) => {
      if (target.disabled || target.readOnly) return;
      if (target instanceof HTMLInputElement && target.type === "number") {
        target.value = text;
        target.dispatchEvent(new Event("input", { bubbles: true }));
      } else {
        target.focus();
        const inserted = document.execCommand("insertText", false, text);
        if (!inserted) {
          const start = target.selectionStart ?? target.value.length;
          const end = target.selectionEnd ?? start;
          target.setRangeText(text, start, end, "end");
          target.dispatchEvent(new Event("input", { bubbles: true }));
        }
      }
    }).catch((error: unknown) => console.error("粘贴到输入框失败：", error));
    return;
  }
  if (key === "z") {
    const target = event.target;
    if (target instanceof HTMLElement && target.closest(".terminal-mount")) return;
    if (!(target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement)) return;
    if (target instanceof HTMLInputElement && !["text", "search", "url", "tel", "password", "number"].includes(target.type)) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    document.execCommand("undo");
    return;
  }
  if (key === "r" && runtimeAvailable.value && activeSession.value) {
    event.preventDefault();
    event.stopImmediatePropagation();
    void reconnectActiveSession();
  }
}

async function exitApplication() {
  try {
    await invoke("app_exit");
  } catch (error) {
    console.error("退出 Apex Term 失败：", error);
  }
}

async function syncNativeBackground() {
  if (!runtimeAvailable.value) return;
  const backgroundColors: Record<ThemeName, string> = {
    graphite: "#181a1d",
    midnight: "#111827",
    paper: "#f6f4ee",
    homebrew: "#050805",
    dracula: "#282a36",
    nord: "#2e3440",
    solarized: "#002b36",
    monokai: "#272822",
    "macos-glass": "#151b26",
  };
  const color = backgroundColors[theme.value];
  try {
    await Promise.all([
      getCurrentWindow().setBackgroundColor(color),
      getCurrentWebviewWindow().setBackgroundColor(color),
    ]);
  } catch (error) {
    console.error("同步窗口背景色失败：", error);
  }
}

const filteredProfiles = computed(() => {
  const query = search.value.trim().toLowerCase();
  return query ? profiles.value.filter((profile) => `${profile.name} ${profile.host} ${profile.username}`.toLowerCase().includes(query)) : profiles.value;
});
const activeState = computed(() => activeSession.value ? sessionStates[activeSession.value.id] : undefined);

function loadProfiles() {
  try { profiles.value = JSON.parse(localStorage.getItem("apex-hosts") || "[]"); } catch { profiles.value = []; }
}
function setTheme(value: ThemeName) { theme.value = value; localStorage.setItem("apex-theme", value); }
function setFont(value: string) { fontFamily.value = value; localStorage.setItem("apex-font", value); }
function setFontSize(value: number) { fontSize.value = value; localStorage.setItem("apex-font-size", String(value)); }
function parseVersion(value: string): [number, number, number] | null {
  const match = /^v?(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.exec(value.trim());
  if (!match) return null;
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}
function compareVersions(left: string, right: string): number | null {
  const leftParts = parseVersion(left);
  const rightParts = parseVersion(right);
  if (!leftParts || !rightParts) return null;
  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] !== rightParts[index]) return leftParts[index] - rightParts[index];
  }
  return 0;
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
async function checkForUpdates() {
  updateStatus.value = "checking";
  latestRelease.value = null;
  updateError.value = "";
  try {
    const response = await fetch(LATEST_RELEASE_URL, {
      headers: {
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
      },
      cache: "no-store",
    });
    if (response.status === 403 || response.status === 429) {
      throw new Error("GitHub API 请求暂时受限，请稍后重试。");
    }
    if (response.status === 404) {
      throw new Error("GitHub 上还没有可检查的正式版本。");
    }
    if (!response.ok) throw new Error(`GitHub API 返回 HTTP ${response.status}。`);

    const payload: unknown = await response.json();
    if (!isRecord(payload) || typeof payload.tag_name !== "string" || typeof payload.html_url !== "string") {
      throw new Error("GitHub 返回的版本信息格式无效。");
    }
    const releaseVersion = parseVersion(payload.tag_name);
    if (!releaseVersion) throw new Error(`无法识别版本号：${payload.tag_name}`);
    const releaseUrl = new URL(payload.html_url);
    if (releaseUrl.origin !== "https://github.com" || !releaseUrl.pathname.startsWith("/zxkbazinga/apex-term/releases/tag/")) {
      throw new Error("GitHub 返回的发布页面地址无效。");
    }
    latestRelease.value = { version: payload.tag_name, url: releaseUrl.toString() };
    const comparison = compareVersions(payload.tag_name, APP_VERSION);
    if (comparison === null) throw new Error(`无法比较当前版本号：${APP_VERSION}`);
    updateStatus.value = comparison > 0 ? "available" : "current";
  } catch (error) {
    updateStatus.value = "error";
    updateError.value = `检查更新失败：${error instanceof Error ? error.message : String(error)}`;
    console.error(updateError.value, error);
  }
}
async function checkForUpdatesAutomatically() {
  await checkForUpdates();
  const release = latestRelease.value;
  if (updateStatus.value !== "available" || !release) return;
  try {
    updateToastVisible.value = localStorage.getItem(DISMISSED_UPDATE_VERSION_KEY) !== release.version;
  } catch (error) {
    console.error("读取已忽略的更新版本失败：", error);
    updateToastVisible.value = true;
  }
}
function dismissUpdateToast() {
  const release = latestRelease.value;
  if (release) {
    try {
      localStorage.setItem(DISMISSED_UPDATE_VERSION_KEY, release.version);
    } catch (error) {
      console.error("保存已忽略的更新版本失败：", error);
    }
  }
  updateToastVisible.value = false;
}
function openUpdateDetails() {
  dismissUpdateToast();
  aboutOpen.value = true;
}
async function openLatestRelease() {
  const release = latestRelease.value;
  if (!release) return;
  try {
    if (runtimeAvailable.value) {
      await openUrl(release.url);
    } else {
      const openedWindow = window.open(release.url, "_blank", "noopener,noreferrer");
      if (!openedWindow) throw new Error("浏览器阻止了打开发布页面。");
    }
  } catch (error) {
    updateError.value = `打开下载页面失败：${error instanceof Error ? error.message : String(error)}`;
    console.error(updateError.value, error);
  }
}
function setWallpaperTransparency(value: number) {
  wallpaperTransparency.value = value;
  localStorage.setItem("apex-wallpaper-transparency", String(value));
}
function loadWallpaper(event: Event) {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  wallpaperError.value = "";
  if (!file) return;
  if (!["image/png", "image/jpeg", "image/webp", "image/gif"].includes(file.type)) {
    wallpaperError.value = "请选择 PNG、JPEG、WebP 或 GIF 图片。";
    return;
  }
  if (file.size > 2 * 1024 * 1024) {
    wallpaperError.value = "图片不能超过 2 MB。";
    return;
  }
  const reader = new FileReader();
  reader.onload = () => {
    if (typeof reader.result !== "string") {
      wallpaperError.value = "无法读取所选图片。";
      return;
    }
    try {
      localStorage.setItem("apex-wallpaper", reader.result);
      wallpaper.value = reader.result;
    } catch (error) {
      wallpaperError.value = `保存背景图失败：${String(error)}`;
    }
  };
  reader.onerror = () => {
    wallpaperError.value = "读取背景图失败。";
  };
  reader.readAsDataURL(file);
}
function clearWallpaper() {
  localStorage.removeItem("apex-wallpaper");
  wallpaper.value = "";
  wallpaperError.value = "";
}
function persistProfiles() { localStorage.setItem("apex-hosts", JSON.stringify(profiles.value)); }
function profileEndpoint(host: string, port: number | string) {
  return `${host.trim().toLowerCase()}:${Number(port)}`;
}
function currentProfileFingerprint() {
  return profileFingerprintEndpoint.value === profileEndpoint(profileForm.host, profileForm.port)
    ? profileFormFingerprint.value
    : undefined;
}
function closeMenuPanels() {
  windowMenuOpen.value = false;
  settingsOpen.value = false;
  sessionsOpen.value = false;
}

function toggleWindowMenu() {
  if (windowMenuOpen.value) {
    windowMenuOpen.value = false;
    return;
  }
  closeMenuPanels();
  windowMenuOpen.value = true;
}

function handleMenuAction(payload: string) {
  windowMenuOpen.value = false;
  aboutOpen.value = false;
  if (payload === "open-settings") {
    settingsOpen.value = !settingsOpen.value;
    sessionsOpen.value = false;
  } else if (payload === "manage-sessions") {
    sessionsOpen.value = !sessionsOpen.value;
    settingsOpen.value = false;
  } else if (payload === "add-session") {
    closeMenuPanels();
    openNewProfile();
  } else if (payload === "open-about") {
    settingsOpen.value = false;
    sessionsOpen.value = false;
    aboutOpen.value = true;
  } else if (payload === "open-sftp") {
    void openSftp();
  }
}

function openSessionsList() {
  windowMenuOpen.value = false;
  settingsOpen.value = false;
  sessionsOpen.value = true;
}

function openNewProfile() {
  sessionsOpen.value = false;
  editingProfileId.value = null;
  Object.assign(profileForm, { name: "", host: "", port: 22, username: "", password: "" });
  formError.value = "";
  testError.value = "";
  testStatus.value = "";
  testFingerprintToTrust.value = "";
  profileFormFingerprint.value = undefined;
  profileFingerprintEndpoint.value = null;
  isModalOpen.value = true;
}
function openProfile(profile: HostProfile) {
  sessionsOpen.value = false;
  editingProfileId.value = profile.id;
  Object.assign(profileForm, { name: profile.name, host: profile.host, port: profile.port, username: profile.username, password: "" });
  formError.value = "";
  testError.value = "";
  testStatus.value = "";
  testFingerprintToTrust.value = "";
  profileFormFingerprint.value = profile.fingerprint;
  profileFingerprintEndpoint.value = profileEndpoint(profile.host, profile.port);
  isModalOpen.value = true;
}
function requestDeleteProfile(profile: HostProfile) {
  profileToDelete.value = profile;
  profileDeleteError.value = "";
}
async function confirmDeleteProfile() {
  const profile = profileToDelete.value;
  if (!profile || profileDeleteLoading.value) return;
  profileDeleteLoading.value = true;
  profileDeleteError.value = "";
  if (runtimeAvailable.value) {
    try {
      await invoke("credential_delete", { profileId: profile.id });
    } catch (error) {
      profileDeleteError.value = `无法删除保存的 SSH 密码：${String(error)}`;
      profileDeleteLoading.value = false;
      return;
    }
  }
  credentialCache.delete(profile.id);
  profiles.value = profiles.value.filter((item) => item.id !== profile.id);
  delete connectionStates[profile.id];
  persistProfiles();
  profileToDelete.value = null;
  profileDeleteLoading.value = false;
}
async function connectSavedProfile(profile: HostProfile) {
  sessionsOpen.value = false;
  if (!runtimeAvailable.value) {
    openProfile(profile);
    formError.value = "SSH 连接需要在 Tauri 桌面应用中运行。";
    return;
  }
  try {
    const password = credentialCache.get(profile.id) || await invoke<string | null>("credential_get", { profileId: profile.id });
    if (!password) {
      openProfile(profile);
      formError.value = "此会话尚未保存密码。输入密码并保存后，下次即可直接连接。";
      return;
    }
    credentialCache.set(profile.id, password);
    await connectProfile(profile, password);
  } catch (error) {
    window.alert(`读取保存的 SSH 密码失败：${String(error)}`);
  }
}

async function testConnection(trustFingerprint = currentProfileFingerprint()) {
  testError.value = "";
  testStatus.value = "";
  formError.value = "";
  if (!runtimeAvailable.value) {
    testError.value = "测试连接需要在 Tauri 桌面应用中运行。";
    return;
  }
  if (!profileForm.host.trim() || !profileForm.username.trim()) {
    testError.value = "请先填写主机地址和用户名。";
    return;
  }
  if (!Number.isInteger(Number(profileForm.port)) || Number(profileForm.port) < 1 || Number(profileForm.port) > 65535) {
    testError.value = "端口范围应为 1–65535。";
    return;
  }
  let password = profileForm.password;
  const profileId = editingProfileId.value;
  if (!password && profileId) {
    password = credentialCache.get(profileId) || "";
    if (!password) {
      try {
        password = await invoke<string | null>("credential_get", { profileId }) || "";
        if (password) credentialCache.set(profileId, password);
      } catch (error) {
        testError.value = `读取已保存密码失败：${String(error)}`;
        return;
      }
    }
  }
  if (!password) {
    testError.value = "请先输入 SSH 密码。";
    return;
  }
  testLoading.value = true;
  try {
    await invoke<void>("ssh_test_connection", {
      host: profileForm.host.trim(),
      port: Number(profileForm.port),
      username: profileForm.username.trim(),
      password,
      trustedFingerprint: trustFingerprint || null,
    });
    profileFormFingerprint.value = trustFingerprint;
    profileFingerprintEndpoint.value = profileEndpoint(profileForm.host, profileForm.port);
    testFingerprintToTrust.value = "";
    testStatus.value = "连接成功，主机和认证信息有效。";
  } catch (error) {
    const message = String(error);
    const unknownFingerprint = message.match(/HOST_KEY_CONFIRM:(SHA256:[a-fA-F0-9]+)/)?.[1];
    if (unknownFingerprint && !trustFingerprint) {
      testFingerprintToTrust.value = unknownFingerprint;
      testError.value = "";
    } else if (message.startsWith("HOST_KEY_CHANGED:")) {
      testError.value = `主机指纹与已信任值不一致，已拒绝连接：${message.slice("HOST_KEY_CHANGED:".length)}`;
    } else {
      testError.value = message.replace(/^Error:\s*/, "");
    }
  } finally {
    testLoading.value = false;
  }
}
async function startLocalSessionFromMenu() {
  sessionsOpen.value = false;
  startLocalSession();
}

function destroyTerminal(sessionId: string) {
  const runtime = terminalRuntimes.get(sessionId);
  if (!runtime) return;
  if (runtime.resizeTimer) clearTimeout(runtime.resizeTimer);
  runtime.flushInput();
  terminalElements.get(sessionId)?.removeEventListener("keydown", runtime.clipboardKeyHandler, true);
  runtime.outputUnlisten?.();
  runtime.connectionEndedUnlisten?.();
  runtime.resizeObserver.disconnect();
  runtime.terminal.dispose();
  terminalRuntimes.delete(sessionId);
  terminalElements.delete(sessionId);
}

function markSessionDisconnected(sessionId: string, reason?: string) {
  const session = sessions.value.find((item) => item.id === sessionId);
  if (!session || sessionStates[sessionId] !== "connected") return;
  sessionStates[sessionId] = "disconnected";
  if (!sessions.value.some((item) =>
    item.id !== sessionId &&
    item.profile.id === session.profile.id &&
    sessionStates[item.id] === "connected"
  )) {
    connectionStates[session.profile.id] = "disconnected";
  }
  if (reason) console.warn(`会话 ${session.profile.name || session.profile.host} 已断开：${reason}`);
  const shortcut = isMacOS ? "Command+R" : "Ctrl+R";
  terminalRuntimes.get(sessionId)?.terminal.writeln(
    `\r\n${reason ? `\x1b[90m断开原因：${reason}\x1b[0m\r\n` : ""}\x1b[31m连接已断开。按 ${shortcut} 重新连接。\x1b[0m\r\n`,
  );
}

function startLocalSession() {
  if (!runtimeAvailable.value) return;
  const profile: HostProfile = {
    id: "local-terminal",
    name: "本地终端",
    host: "本机",
    port: 0,
    username: "Shell",
  };
  const id = `local-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`;
  connectionStates[profile.id] = "connecting";
  sessionStates[id] = "connecting";
  sessions.value.push({ id, kind: "local", profile });
  activeSessionId.value = id;
}

async function establishSshSession(sessionId: string, profile: HostProfile, password: string, cols: number, rows: number) {
  const connect = (trustedFingerprint: string | null) => invoke("ssh_connect", {
    sessionId,
    host: profile.host,
    port: profile.port,
    username: profile.username,
    password,
    trustedFingerprint,
    cols,
    rows,
  });

  try {
    await connect(profile.fingerprint || null);
  } catch (error) {
    const message = String(error);
    if (message.startsWith("HOST_KEY_CONFIRM:")) {
      const fingerprint = message.slice("HOST_KEY_CONFIRM:".length);
      await new Promise<void>((resolve, reject) => {
        hostTrustRequest.value = { sessionId, profile, password, fingerprint };
        resolveHostTrust = resolve;
        rejectHostTrust = reject;
      });
    } else if (message.startsWith("HOST_KEY_CHANGED:")) {
      const fingerprint = message.slice("HOST_KEY_CHANGED:".length);
      await new Promise<void>((resolve, reject) => {
        hostTrustRequest.value = {
          sessionId,
          profile,
          password,
          fingerprint,
          knownFingerprint: profile.fingerprint,
        };
        resolveHostTrust = resolve;
        rejectHostTrust = reject;
      });
    } else {
      throw error;
    }
  }
  connectionStates[profile.id] = "connected";
}

async function trustHostFingerprint() {
  const request = hostTrustRequest.value;
  if (!request) return;
  const runtime = terminalRuntimes.get(request.sessionId);
  try {
    await invoke("ssh_connect", {
      sessionId: request.sessionId,
      host: request.profile.host,
      port: request.profile.port,
      username: request.profile.username,
      password: request.password,
      trustedFingerprint: request.fingerprint,
      cols: runtime?.terminal.cols || 80,
      rows: runtime?.terminal.rows || 24,
    });
    request.profile.fingerprint = request.fingerprint;
    persistProfiles();
    hostTrustRequest.value = null;
    resolveHostTrust?.();
  } catch (error) {
    hostTrustRequest.value = null;
    rejectHostTrust?.(new Error(String(error)));
  } finally {
    resolveHostTrust = undefined;
    rejectHostTrust = undefined;
  }
}

function cancelHostTrust() {
  hostTrustRequest.value = null;
  rejectHostTrust?.(new Error("已取消连接，服务器指纹尚未受信任。"));
  resolveHostTrust = undefined;
  rejectHostTrust = undefined;
}

function getTerminalPalette(value: ThemeName) {
  const palettes = {
    graphite: {
      background: "#151719", foreground: "#e1e4e8", cursor: "#c2c8d0", selectionBackground: "#3a4048",
      black: "#151719", red: "#d07b7b", green: "#93b58f", yellow: "#c9b37e", blue: "#829bb8", magenta: "#ae91b8", cyan: "#83aaa9", white: "#c8cdd3",
      brightBlack: "#747b84", brightRed: "#e39a9a", brightGreen: "#a9c9a5", brightYellow: "#ddc992", brightBlue: "#9bb5d2", brightMagenta: "#c2a6cc", brightCyan: "#9ac1c0", brightWhite: "#f0f2f4",
    },
    paper: {
      background: "#f5f4ef", foreground: "#343833", cursor: "#4c795e", selectionBackground: "#cad8ca",
      black: "#343833", red: "#b04c45", green: "#4c795e", yellow: "#a88138", blue: "#4f708a", magenta: "#916587", cyan: "#4d8583", white: "#d5d4cd",
      brightBlack: "#77796f", brightRed: "#c35e55", brightGreen: "#659270", brightYellow: "#ba984f", brightBlue: "#6689a5", brightMagenta: "#a67ba0", brightCyan: "#65a09d", brightWhite: "#fffefa",
    },
    homebrew: {
      background: "#000000", foreground: "#00ff00", cursor: "#00ff00", selectionBackground: "#1b3b1b",
      black: "#000000", red: "#990000", green: "#00a600", yellow: "#999900", blue: "#0000b2", magenta: "#b200b2", cyan: "#00a6b2", white: "#bfbfbf",
      brightBlack: "#666666", brightRed: "#e50000", brightGreen: "#00d900", brightYellow: "#e5e500", brightBlue: "#0000ff", brightMagenta: "#e500e5", brightCyan: "#00e5e5", brightWhite: "#e5e5e5",
    },
    midnight: {
      background: "#111827", foreground: "#dbeafe", cursor: "#93c5fd", selectionBackground: "#334155",
      black: "#111827", red: "#f87171", green: "#86efac", yellow: "#fde68a", blue: "#93c5fd", magenta: "#d8b4fe", cyan: "#67e8f9", white: "#dbeafe",
      brightBlack: "#64748b", brightRed: "#fca5a5", brightGreen: "#bbf7d0", brightYellow: "#fef3c7", brightBlue: "#bfdbfe", brightMagenta: "#e9d5ff", brightCyan: "#a5f3fc", brightWhite: "#f8fafc",
    },
    dracula: {
      background: "#282a36", foreground: "#f8f8f2", cursor: "#f8f8f2", selectionBackground: "#44475a",
      black: "#21222c", red: "#ff5555", green: "#50fa7b", yellow: "#f1fa8c", blue: "#bd93f9", magenta: "#ff79c6", cyan: "#8be9fd", white: "#f8f8f2",
      brightBlack: "#6272a4", brightRed: "#ff6e6e", brightGreen: "#69ff94", brightYellow: "#ffffa5", brightBlue: "#d6acff", brightMagenta: "#ff92df", brightCyan: "#a4ffff", brightWhite: "#ffffff",
    },
    nord: {
      background: "#2e3440", foreground: "#d8dee9", cursor: "#88c0d0", selectionBackground: "#434c5e",
      black: "#3b4252", red: "#bf616a", green: "#a3be8c", yellow: "#ebcb8b", blue: "#81a1c1", magenta: "#b48ead", cyan: "#88c0d0", white: "#e5e9f0",
      brightBlack: "#4c566a", brightRed: "#bf616a", brightGreen: "#a3be8c", brightYellow: "#ebcb8b", brightBlue: "#81a1c1", brightMagenta: "#b48ead", brightCyan: "#8fbcbb", brightWhite: "#eceff4",
    },
    solarized: {
      background: "#002b36", foreground: "#839496", cursor: "#93a1a1", selectionBackground: "#073642",
      black: "#073642", red: "#dc322f", green: "#859900", yellow: "#b58900", blue: "#268bd2", magenta: "#d33682", cyan: "#2aa198", white: "#eee8d5",
      brightBlack: "#002b36", brightRed: "#cb4b16", brightGreen: "#586e75", brightYellow: "#657b83", brightBlue: "#839496", brightMagenta: "#6c71c4", brightCyan: "#93a1a1", brightWhite: "#fdf6e3",
    },
    monokai: {
      background: "#272822", foreground: "#f8f8f2", cursor: "#f8f8f0", selectionBackground: "#49483e",
      black: "#272822", red: "#f92672", green: "#a6e22e", yellow: "#f4bf75", blue: "#66d9ef", magenta: "#ae81ff", cyan: "#a1efe4", white: "#f8f8f2",
      brightBlack: "#75715e", brightRed: "#f92672", brightGreen: "#a6e22e", brightYellow: "#f4bf75", brightBlue: "#66d9ef", brightMagenta: "#ae81ff", brightCyan: "#a1efe4", brightWhite: "#f9f8f5",
    },
    "macos-glass": {
      background: "#151b26", foreground: "#e5edf8", cursor: "#d7e5ff", selectionBackground: "#536985",
      black: "#151b26", red: "#ff7f8e", green: "#83d6a5", yellow: "#f3ce83", blue: "#8ab8ff", magenta: "#d4a9f5", cyan: "#82d8e2", white: "#dce6f2",
      brightBlack: "#7c8ca2", brightRed: "#ff9daa", brightGreen: "#a6e9bd", brightYellow: "#ffe2a1", brightBlue: "#a9cbff", brightMagenta: "#e1c0fb", brightCyan: "#a2e8ef", brightWhite: "#f5f8ff",
    },
  } satisfies Record<ThemeName, Terminal["options"]["theme"]>;
  const palette = palettes[value];
  if (wallpaper.value) {
    return { ...palette, background: `${palette.background}00` };
  }
  return palette;
}

function setTerminalElement(sessionId: string, element: unknown) {
  if (!(element instanceof HTMLElement)) return;
  terminalElements.set(sessionId, element);
  const session = sessions.value.find((item) => item.id === sessionId);
  if (session && !terminalRuntimes.has(sessionId)) createTerminal(session, element);
}

function createTerminal(session: TerminalSession, element: HTMLElement) {
  const terminal = new Terminal({
    cursorBlink: true,
    fontFamily: `"${fontFamily.value}", Menlo, "SFMono-Regular", Consolas, monospace`,
    fontSize: fontSize.value,
    lineHeight: 1.38,
    scrollback: 5000,
    theme: getTerminalPalette(theme.value),
  });
  const fitAddon = new FitAddon();
  terminal.loadAddon(fitAddon);
  terminal.open(element);
  if (activeSessionId.value === session.id) fitAddon.fit();
  const copySelectionToClipboard = (text: string) => {
    const copyText = isTauri() ? writeText(text) : navigator.clipboard.writeText(text);
    void copyText.catch((error: unknown) => console.error("复制终端选区失败：", error));
  };
  terminal.onSelectionChange(() => {
    const selection = terminal.getSelection();
    if (selection) copySelectionToClipboard(selection);
  });
  const clipboardKeyHandler = (event: KeyboardEvent) => {
    if (event.type !== "keydown" || (!event.ctrlKey && !event.metaKey)) return;
    const key = event.key.toLowerCase();
    if (key === "c" && terminal.hasSelection()) {
      event.preventDefault();
      event.stopPropagation();
      copySelectionToClipboard(terminal.getSelection());
      terminal.clearSelection();
    } else if (key === "v") {
      event.preventDefault();
      event.stopPropagation();
      const pasteText = isTauri() ? readText() : navigator.clipboard.readText();
      void pasteText
        .then((text) => terminal.paste(text))
        .catch((error: unknown) => console.error("读取剪贴板内容失败：", error));
    }
  };
  element.addEventListener("keydown", clipboardKeyHandler, true);

  let runtime: TerminalRuntime;
  runtime = {
    terminal,
    fitAddon,
    clipboardKeyHandler,
    resizeObserver: new ResizeObserver(() => scheduleTerminalResize(session, runtime, 180)),
    pendingInput: "",
    writeQueue: Promise.resolve(),
    flushInput: () => undefined,
  };
  terminalRuntimes.set(session.id, runtime);

  runtime.flushInput = () => {
    if (runtime.inputFlushTimer) {
      clearTimeout(runtime.inputFlushTimer);
      runtime.inputFlushTimer = undefined;
    }
    const data = runtime.pendingInput;
    runtime.pendingInput = "";
    if (!data || !terminalRuntimes.has(session.id) || sessionStates[session.id] !== "connected") return;
    const command = session.kind === "local" ? "local_write" : "ssh_write";
    runtime.writeQueue = runtime.writeQueue
      .then(() => invoke<void>(command, { sessionId: session.id, data }))
      .catch((error: unknown) => {
        terminal.writeln(`\r\n\x1b[31m${String(error)}\x1b[0m`);
      });
  };
  terminal.onData((data) => {
    if (activeSessionId.value !== session.id || sessionStates[session.id] !== "connected") return;
    runtime.pendingInput += data;
    if (!runtime.inputFlushTimer) runtime.inputFlushTimer = setTimeout(runtime.flushInput, 8);
  });
  const outputListener = listen<string>(`${session.kind}-output-${session.id}`, (event) => terminal.write(event.payload));
  const endedListener = listen<string>(`${session.kind}-session-ended-${session.id}`, (event) => {
    markSessionDisconnected(session.id, event.payload);
  });
  void Promise.all([outputListener, endedListener]).then(([unlistenOutput, unlistenEnded]) => {
    if (!terminalRuntimes.has(session.id)) {
      unlistenOutput();
      unlistenEnded?.();
      return;
    }
    runtime.outputUnlisten = unlistenOutput;
    runtime.connectionEndedUnlisten = unlistenEnded;
    const password = pendingPasswords.get(session.id) || "";
    pendingPasswords.delete(session.id);
    runtime.lastPtySize = { cols: terminal.cols || 80, rows: terminal.rows || 24 };
    const start = session.kind === "local"
      ? invoke("local_start", { sessionId: session.id, cols: terminal.cols || 80, rows: terminal.rows || 24 })
      : establishSshSession(session.id, session.profile, password, terminal.cols || 80, terminal.rows || 24);
    void start.then(() => {
      if (sessionStates[session.id] === "connecting") {
        sessionStates[session.id] = "connected";
        connectionStates[session.profile.id] = "connected";
        scheduleTerminalResize(session, runtime, 180);
      }
    }).catch((error: unknown) => {
      sessionStates[session.id] = "disconnected";
      connectionStates[session.profile.id] = "disconnected";
      terminal.writeln(`\r\n\x1b[31m${session.kind === "local" ? "本地终端启动失败" : "SSH 连接失败"}：${String(error)}\x1b[0m\r\n`);
    });
  }).catch((error: unknown) => {
    sessionStates[session.id] = "disconnected";
    connectionStates[session.profile.id] = "disconnected";
    terminal.writeln(`\r\n\x1b[31m无法监听终端输出：${String(error)}\x1b[0m\r\n`);
  });
  runtime.resizeObserver.observe(element);
}

function scheduleTerminalResize(session: TerminalSession, runtime: TerminalRuntime, delay = 0) {
  if (activeSessionId.value !== session.id) return;
  runtime.fitAddon.fit();
  if (runtime.resizeTimer) clearTimeout(runtime.resizeTimer);
  runtime.resizeTimer = setTimeout(() => {
    runtime.resizeTimer = undefined;
    if (!terminalRuntimes.has(session.id) || activeSessionId.value !== session.id || sessionStates[session.id] !== "connected") return;
    const size = { cols: runtime.terminal.cols, rows: runtime.terminal.rows };
    if (runtime.lastPtySize?.cols === size.cols && runtime.lastPtySize.rows === size.rows) return;
    runtime.lastPtySize = size;
    const command = session.kind === "local" ? "local_resize" : "ssh_resize";
    void invoke(command, { sessionId: session.id, ...size }).catch((error: unknown) => {
      if (runtime.lastPtySize?.cols === size.cols && runtime.lastPtySize.rows === size.rows) runtime.lastPtySize = undefined;
      if (terminalRuntimes.has(session.id) && sessionStates[session.id] === "connected") {
        runtime.terminal.writeln(`\r\n\x1b[31m终端尺寸调整失败：${String(error)}\x1b[0m\r\n`);
      }
    });
  }, delay);
}

async function connectProfile(profile: HostProfile, password: string) {
  if (!runtimeAvailable.value) {
    formError.value = "SSH 连接需要在 Tauri 桌面应用中运行。";
    return;
  }
  const sessionId = `ssh-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`;
  pendingPasswords.set(sessionId, password);
  sshPasswords.set(sessionId, password);
  sessionStates[sessionId] = "connecting";
  connectionStates[profile.id] = "connecting";
  sessions.value.push({ id: sessionId, kind: "ssh", profile });
  activeSessionId.value = sessionId;
  isModalOpen.value = false;
  profileForm.password = "";
}
async function reconnectActiveSession() {
  const session = activeSession.value;
  if (!session || !runtimeAvailable.value || sessionStates[session.id] === "connecting") return;

  let password = "";
  if (session.kind === "ssh") {
    password = sshPasswords.get(session.id) || credentialCache.get(session.profile.id) || "";
    if (!password) {
      try {
        password = await invoke<string | null>("credential_get", { profileId: session.profile.id }) || "";
      } catch (error) {
        const runtime = terminalRuntimes.get(session.id);
        runtime?.terminal.writeln(`\r\n\x1b[31m读取已保存的 SSH 密码失败：${String(error)}\x1b[0m\r\n`);
        return;
      }
    }
    if (!password) {
      openProfile(session.profile);
      formError.value = "此会话没有已保存的密码，请输入密码后重新连接。";
      return;
    }
    credentialCache.set(session.profile.id, password);
  }

  const sessionId = session.id;
  const runtime = terminalRuntimes.get(sessionId);
  runtime?.flushInput();
  sessionStates[sessionId] = "connecting";
  connectionStates[session.profile.id] = "connecting";
  await runtime?.writeQueue;

  const stopCommand = session.kind === "local" ? "local_stop" : "ssh_disconnect";
  await invoke(stopCommand, { sessionId }).catch((error: unknown) => {
    console.error(`重新连接前关闭旧会话失败：${String(error)}`);
  });
  if (sftpOpen.value) {
    sftpOpen.value = false;
    sftpEntries.value = [];
    sftpError.value = "";
  }

  const cols = runtime?.terminal.cols || 80;
  const rows = runtime?.terminal.rows || 24;
  runtime?.terminal.clear();
  try {
    if (session.kind === "local") {
      await invoke("local_start", { sessionId, cols, rows });
    } else {
      sshPasswords.set(sessionId, password);
      await establishSshSession(sessionId, session.profile, password, cols, rows);
    }
    if (!sessions.value.some((item) => item.id === sessionId)) {
      await invoke(stopCommand, { sessionId });
      return;
    }
    sessionStates[sessionId] = "connected";
    connectionStates[session.profile.id] = "connected";
    if (runtime) {
      runtime.lastPtySize = { cols, rows };
      scheduleTerminalResize(session, runtime, 180);
      runtime.terminal.focus();
    }
  } catch (error) {
    sessionStates[sessionId] = "disconnected";
    connectionStates[session.profile.id] = "disconnected";
    runtime?.terminal.writeln(`\r\n\x1b[31m重新连接失败：${String(error)}\x1b[0m\r\n`);
  }
}
async function refreshSftp(path = sftpPath.value) {
  const session = activeSession.value;
  const password = session ? sshPasswords.get(session.id) : undefined;
  if (!session || session.kind !== "ssh" || !password) return;
  sftpLoading.value = true;
  sftpError.value = "";
  try {
    const result = await invoke<{ path: string; entries: SftpEntry[] }>("sftp_list", {
      host: session.profile.host,
      port: session.profile.port,
      username: session.profile.username,
      password,
      trustedFingerprint: session.profile.fingerprint || null,
      path,
    });
    sftpPath.value = result.path;
    sftpEntries.value = result.entries;
  } catch (error) {
    sftpError.value = String(error);
  } finally {
    sftpLoading.value = false;
  }
}
async function openSftp() {
  settingsOpen.value = false;
  sessionsOpen.value = false;
  if (activeSession.value?.kind !== "ssh" || activeState.value !== "connected") {
    window.alert("请先连接 SSH 主机，再使用 SFTP。");
    return;
  }
  sftpOpen.value = true;
  await refreshSftp(sftpPath.value);
}
function remoteChildPath(name: string) {
  return `${sftpPath.value.replace(/\/+$/, "")}/${name}` || `/${name}`;
}
function parentSftpPath() {
  const path = sftpPath.value.replace(/\/+$/, "");
  return path.includes("/") ? path.slice(0, path.lastIndexOf("/")) || "/" : "/";
}
function formatBytes(size: number | null) {
  if (size === null) return "—";
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}
async function uploadSftpFile(event: Event) {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  const session = activeSession.value;
  input.value = "";
  if (!file || !session || session.kind !== "ssh") return;
  const password = sshPasswords.get(session.id);
  if (!password) return;
  const existing = sftpEntries.value.find((entry) => entry.name === file.name);
  const overwrite = existing ? window.confirm(`远程目录中已存在“${file.name}”，是否覆盖？`) : false;
  if (existing && !overwrite) return;
  if (file.size > MAX_SFTP_FILE_SIZE) {
    sftpError.value = "单个文件最大支持 32 MB。";
    return;
  }
  sftpLoading.value = true;
  sftpError.value = "";
  try {
    const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
    await invoke("sftp_upload", {
      host: session.profile.host, port: session.profile.port, username: session.profile.username,
      password, trustedFingerprint: session.profile.fingerprint || null,
      path: remoteChildPath(file.name), data: bytes, overwrite,
    });
    await refreshSftp();
  } catch (error) {
    sftpError.value = String(error);
  } finally {
    sftpLoading.value = false;
  }
}
async function downloadSftpFile(entry: SftpEntry) {
  const session = activeSession.value;
  if (!session || session.kind !== "ssh") return;
  const password = sshPasswords.get(session.id);
  if (!password) return;
  sftpError.value = "";
  try {
    const bytes = await invoke<number[]>("sftp_download", {
      host: session.profile.host, port: session.profile.port, username: session.profile.username,
      password, trustedFingerprint: session.profile.fingerprint || null, path: entry.path,
    });
    const url = URL.createObjectURL(new Blob([new Uint8Array(bytes)]));
    const link = document.createElement("a");
    link.href = url;
    link.download = entry.name;
    link.click();
    URL.revokeObjectURL(url);
  } catch (error) {
    sftpError.value = String(error);
  }
}
async function createSftpDirectory() {
  const name = window.prompt("新目录名称");
  const session = activeSession.value;
  if (!name?.trim() || !session || session.kind !== "ssh") return;
  const password = sshPasswords.get(session.id);
  if (!password) return;
  try {
    await invoke("sftp_mkdir", {
      host: session.profile.host, port: session.profile.port, username: session.profile.username,
      password, trustedFingerprint: session.profile.fingerprint || null,
      path: remoteChildPath(name.trim()),
    });
    await refreshSftp();
  } catch (error) {
    sftpError.value = String(error);
  }
}
async function deleteSftpEntry(entry: SftpEntry) {
  if (!window.confirm(`确定删除“${entry.name}”吗？`)) return;
  const session = activeSession.value;
  if (!session || session.kind !== "ssh") return;
  const password = sshPasswords.get(session.id);
  if (!password) return;
  try {
    await invoke("sftp_delete", {
      host: session.profile.host, port: session.profile.port, username: session.profile.username,
      password, trustedFingerprint: session.profile.fingerprint || null, path: entry.path,
    });
    await refreshSftp();
  } catch (error) {
    sftpError.value = String(error);
  }
}
async function saveAndConnect() {
  formError.value = "";
  if (!runtimeAvailable.value) {
    formError.value = "SSH 连接需要在 Tauri 桌面应用中运行。";
    return;
  }
  if (!profileForm.name.trim() || !profileForm.host.trim() || !profileForm.username.trim()) {
    formError.value = "请填写连接名称、主机地址和用户名。";
    return;
  }
  if (!Number.isInteger(Number(profileForm.port)) || Number(profileForm.port) < 1 || Number(profileForm.port) > 65535) {
    formError.value = "端口范围应为 1–65535。";
    return;
  }
  const profile: HostProfile = {
    id: editingProfileId.value || `host-${Date.now()}`,
    name: profileForm.name.trim(), host: profileForm.host.trim(), port: Number(profileForm.port), username: profileForm.username.trim(),
    fingerprint: currentProfileFingerprint(),
  };
  let password = profileForm.password;
  if (!password) {
    try {
      password = await invoke<string | null>("credential_get", { profileId: profile.id }) || "";
    } catch (error) {
      formError.value = `读取已保存的 SSH 密码失败：${String(error)}`;
      return;
    }
  }
  if (!password) {
    formError.value = "请输入 SSH 密码。首次保存后，之后可从会话管理直接连接。";
    return;
  }
  try {
    await invoke("credential_store", { profileId: profile.id, password });
    credentialCache.set(profile.id, password);
  } catch (error) {
    formError.value = `无法将密码保存到系统凭据管理器：${String(error)}`;
    return;
  }
  if (editingProfileId.value) profiles.value = profiles.value.map((item) => item.id === profile.id ? profile : item);
  else profiles.value.unshift(profile);
  persistProfiles();
  await connectProfile(profile, password);
}
async function closeSession(sessionId = activeSessionId.value) {
  if (!sessionId) return;
  const session = sessions.value.find((item) => item.id === sessionId);
  if (!session) return;
  const runtime = terminalRuntimes.get(sessionId);
  runtime?.flushInput();
  sessionStates[sessionId] = "disconnected";
  await runtime?.writeQueue;
  if (session.kind === "local" || sessions.value.filter((item) => item.profile.id === session.profile.id).length === 1) {
    connectionStates[session.profile.id] = "disconnected";
  }
  const command = session.kind === "local" ? "local_stop" : "ssh_disconnect";
  await invoke(command, { sessionId }).catch((error: unknown) => {
    console.error(`关闭会话失败：${String(error)}`);
  });
  if (hostTrustRequest.value?.sessionId === sessionId) cancelHostTrust();
  destroyTerminal(sessionId);
  pendingPasswords.delete(sessionId);
  sshPasswords.delete(sessionId);
  delete sessionStates[sessionId];
  sftpOpen.value = false;
  const index = sessions.value.findIndex((item) => item.id === sessionId);
  sessions.value = sessions.value.filter((item) => item.id !== sessionId);
  if (activeSessionId.value === sessionId) {
    activeSessionId.value = sessions.value[Math.max(0, index - 1)]?.id || sessions.value[0]?.id || null;
  }
}

watch([theme, fontFamily, fontSize, wallpaper, wallpaperTransparency], async ([nextTheme, nextFont, nextSize]) => {
  for (const runtime of terminalRuntimes.values()) {
    runtime.terminal.options.fontFamily = `"${nextFont}", Menlo, "SFMono-Regular", Consolas, monospace`;
    runtime.terminal.options.fontSize = nextSize;
    runtime.terminal.options.theme = getTerminalPalette(nextTheme);
  }
  await nextTick();
  if (theme.value === nextTheme) void syncNativeBackground();
  for (const [sessionId, runtime] of terminalRuntimes) {
    if (sessionId === activeSessionId.value) {
      const session = sessions.value.find((item) => item.id === sessionId);
      if (session) scheduleTerminalResize(session, runtime, 120);
    }
  }
});
watch(activeSessionId, async (sessionId) => {
  if (!sessionId) return;
  await nextTick();
  const runtime = terminalRuntimes.get(sessionId);
  const session = sessions.value.find((item) => item.id === sessionId);
  if (!runtime || !session) return;
  scheduleTerminalResize(session, runtime, 120);
  runtime.terminal.focus();
});
onMounted(() => {
  window.addEventListener("keydown", handleApplicationShortcut, true);
  void syncNativeBackground();
  void listen<string>("native-menu-action", ({ payload }) => handleMenuAction(payload)).then((unlisten) => {
    unlistenNativeMenu = unlisten;
  }).catch((error: unknown) => {
    console.error("无法监听系统菜单操作：", error);
  });
});
onMounted(loadProfiles);
onMounted(() => {
  if (runtimeAvailable.value) startLocalSession();
});
onMounted(() => {
  if (runtimeAvailable.value) void checkForUpdatesAutomatically();
});
onUnmounted(() => {
  window.removeEventListener("keydown", handleApplicationShortcut, true);
  unlistenNativeMenu?.();
  for (const sessionId of terminalRuntimes.keys()) destroyTerminal(sessionId);
});
</script>

<template>
  <main
    class="app-shell"
    :class="[`theme-${theme}`]"
    @click="closeMenuPanels"
  >
    <div class="native-menu-panels">
        <div v-if="settingsOpen" class="modal-backdrop menu-backdrop" @click.self="closeMenuPanels">
          <section class="settings-popover menu-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title" @click.stop>
            <div id="settings-title" class="settings-title">外观</div>
            <label class="settings-select">主题
              <select :value="theme" @change="setTheme(($event.target as HTMLSelectElement).value as ThemeName)">
                <option v-for="item in availableThemes" :key="item.id" :value="item.id">{{ item.label }}</option>
              </select>
            </label>
            <label class="settings-select">字体
              <select :value="fontFamily" @change="setFont(($event.target as HTMLSelectElement).value)">
                <option>MesloLGS NF</option><option>Menlo</option><option>SFMono-Regular</option><option>Cascadia Code</option><option>Consolas</option><option>JetBrains Mono</option><option>Courier New</option>
              </select>
            </label>
            <label class="settings-select">字号
              <select :value="fontSize" @change="setFontSize(Number(($event.target as HTMLSelectElement).value))">
                <option :value="12">12 px</option><option :value="13">13 px</option><option :value="14">14 px</option><option :value="15">15 px</option><option :value="16">16 px</option><option :value="18">18 px</option>
              </select>
            </label>
            <div class="wallpaper-setting">
              <span>背景图</span>
              <div class="wallpaper-actions">
                <button type="button" @click="wallpaperInput?.click()">{{ wallpaper ? "更换图片" : "选择图片" }}</button>
                <button v-if="wallpaper" type="button" @click="clearWallpaper">移除</button>
              </div>
              <input ref="wallpaperInput" class="hidden-file-input" type="file" accept="image/png,image/jpeg,image/webp,image/gif" @change="loadWallpaper" />
              <label v-if="wallpaper" class="wallpaper-opacity">
                <span>背景透明度 <strong>{{ wallpaperTransparency }}%</strong></span>
                <input :value="wallpaperTransparency" type="range" min="0" max="80" step="5" @input="setWallpaperTransparency(Number(($event.target as HTMLInputElement).value))" />
              </label>
              <span v-if="wallpaperError" class="wallpaper-error" role="alert">{{ wallpaperError }}</span>
              <span class="wallpaper-hint">支持 PNG、JPEG、WebP、GIF，最大 2 MB</span>
            </div>
          </section>
        </div>
        <div v-if="sessionsOpen" class="modal-backdrop menu-backdrop" @click.self="closeMenuPanels">
          <section class="sessions-popover menu-dialog" role="dialog" aria-modal="true" aria-labelledby="sessions-title" @click.stop>
            <div id="sessions-title" class="sessions-popover-heading"><span>会话列表</span><button type="button" @click="openNewProfile"><Plus :size="14" /> 添加主机</button></div>
            <button class="saved-session local-session-button" type="button" :class="{ active: activeSession?.kind === 'local' }" @click="startLocalSessionFromMenu">
              <TerminalSquare :size="15" /><span class="saved-session-copy"><strong>本机</strong></span><span class="status-dot" :class="activeSession?.kind === 'local' ? activeState : 'disconnected'"></span>
            </button>
            <div class="sessions-divider"></div>
            <label class="search-box session-search">
              <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="8.8" cy="8.8" r="5.5" /><path d="m13 13 4 4" /></svg>
              <input v-model="search" aria-label="搜索已保存的 SSH 会话" placeholder="搜索已保存的主机" />
            </label>
            <div class="saved-session-list">
              <div v-if="filteredProfiles.length === 0" class="sessions-empty">{{ profiles.length ? "没有匹配的会话" : "还没有保存的 SSH 会话" }}</div>
              <div v-for="profile in filteredProfiles" :key="profile.id" class="saved-session-row">
                <button class="saved-session" type="button" @click="connectSavedProfile(profile)">
                  <Server :size="15" /><span class="saved-session-copy"><strong>{{ profile.name }}</strong><small>{{ profile.username }}@{{ profile.host }}:{{ profile.port }}</small></span><span class="status-dot" :class="connectionStates[profile.id] || 'disconnected'"></span>
                </button>
                <button class="saved-session-edit" type="button" :title="`编辑 ${profile.name}`" @click="openProfile(profile)"><Settings2 :size="14" /></button>
                <button class="saved-session-delete" type="button" :title="`删除 ${profile.name}`" :aria-label="`删除 ${profile.name}`" @click.stop="requestDeleteProfile(profile)"><Trash2 :size="14" /></button>
              </div>
            </div>
            <div class="sessions-footnote">连接资料保存在本机 · 密码存入系统凭据管理器</div>
          </section>
        </div>
    </div>

    <div class="app-body">
      <section class="main-panel">
        <div v-if="(isMacOS || isWindowsOS) && runtimeAvailable" class="session-strip" :class="{ 'app-titlebar': isMacOS || isWindowsOS }">
          <div v-for="session in sessions" :key="session.id" class="session-tab" :class="{ 'session-tab-active': activeSessionId === session.id }" @click="activeSessionId = session.id">
            <span class="tab-live-dot" :class="sessionStates[session.id] || 'disconnected'"></span>
            <TerminalSquare :size="15" />
            <span class="tab-title">{{ session.kind === "local" ? "本机" : (session.profile.name?.trim() || session.profile.host) }}</span>
            <button type="button" :title="session.kind === 'local' ? '关闭本机终端' : '关闭会话'" :aria-label="session.kind === 'local' ? '关闭本机终端' : `关闭 ${session.profile.name}`" @click.stop="closeSession(session.id)"><X :size="14" /></button>
          </div>
          <button class="new-tab-button" type="button" title="打开会话列表" @click.stop="openSessionsList"><Plus :size="15" /></button>
          <div class="window-menu-wrap">
            <button class="window-menu-button" type="button" aria-label="打开应用菜单" :aria-expanded="windowMenuOpen" @click.stop="toggleWindowMenu">
              <ChevronDown :size="14" />
            </button>
            <div v-if="windowMenuOpen" class="window-menu-dropdown" @click.stop>
              <button type="button" @click="handleMenuAction('open-settings')"><Settings2 :size="14" />外观</button>
              <button type="button" @click="handleMenuAction('manage-sessions')"><Server :size="14" />会话列表</button>
              <button type="button" @click="handleMenuAction('add-session')"><Plus :size="14" />添加 SSH 主机</button>
              <button type="button" @click="handleMenuAction('open-sftp')"><Folder :size="14" />SFTP 文件</button>
              <button type="button" @click="handleMenuAction('open-about')"><TerminalSquare :size="14" />关于 Apex Term</button>
              <div class="window-menu-divider"></div>
              <button type="button" @click="exitApplication"><X :size="14" />退出 ApexTerm</button>
            </div>
          </div>
        </div>
        <div v-else-if="sessions.length > 0" class="session-strip">
          <div v-for="session in sessions" :key="session.id" class="session-tab" :class="{ 'session-tab-active': activeSessionId === session.id }" @click="activeSessionId = session.id">
            <span class="tab-live-dot" :class="sessionStates[session.id] || 'disconnected'"></span>
            <TerminalSquare :size="15" />
            <span class="tab-title">{{ session.kind === "local" ? "本机" : (session.profile.name?.trim() || session.profile.host) }}</span>
            <button type="button" :title="session.kind === 'local' ? '关闭本机终端' : '关闭会话'" :aria-label="session.kind === 'local' ? '关闭本机终端' : `关闭 ${session.profile.name}`" @click.stop="closeSession(session.id)"><X :size="14" /></button>
          </div>
          <button class="new-tab-button" type="button" title="打开会话列表" @click.stop="openSessionsList"><Plus :size="15" /></button>
        </div>
        <div
          v-for="session in sessions"
          v-show="activeSessionId === session.id"
          :key="session.id"
          class="terminal-stage"
          :class="[`terminal-${theme}`, { 'has-wallpaper': wallpaper }]"
          :style="wallpaper ? { '--wallpaper-image': `url('${wallpaper}')`, '--wallpaper-overlay-opacity': String((100 - wallpaperTransparency) / 100) } : undefined"
        >
          <div :ref="(element) => setTerminalElement(session.id, element)" class="terminal-mount"></div>
        </div>
        <div v-if="sessions.length === 0" class="welcome-stage">
          <div class="welcome-content">
            <div class="welcome-art"><div class="orbit orbit-one"></div><div class="orbit orbit-two"></div><div class="welcome-terminal"><span>$_</span><i></i></div><div class="art-spark spark-one">+</div><div class="art-spark spark-two">·</div></div>
            <div class="welcome-kicker"><span></span> SECURE SHELL, SIMPLIFIED</div>
            <h2>连接到你的<br /><em>下一台主机。</em></h2>
            <p v-if="runtimeAvailable">连接主机，在一个清爽的终端里开始工作。</p>
            <p v-else class="runtime-message">请通过 Tauri 桌面应用启动，以使用终端和 SSH。</p>
            <button class="primary-button" type="button" :disabled="!runtimeAvailable" @click="openNewProfile"><Plus :size="17" /> 新建 SSH 连接</button>
            <div class="welcome-footnote"><span class="footnote-line"></span>{{ runtimeAvailable ? "SSH 密码安全保存" : "当前为浏览器预览" }}<span class="footnote-line"></span></div>
          </div>
          <div class="welcome-corner"><span>01</span> / WORKSPACE</div>
        </div>
      </section>
    </div>

    <div v-if="isModalOpen" class="modal-backdrop" @click.self="isModalOpen = false">
      <section class="connection-modal" role="dialog" aria-modal="true" aria-labelledby="modal-title">
        <header class="modal-header">
          <div class="modal-icon"><Server :size="18" /></div><div><span class="eyebrow">SSH CONNECTION</span><h2 id="modal-title">{{ editingProfileId ? "连接主机" : "新建连接" }}</h2></div>
          <button class="icon-button modal-close" type="button" aria-label="关闭" @click="isModalOpen = false"><X :size="18" /></button>
        </header>
        <form class="connection-form" @submit.prevent="saveAndConnect">
          <label class="field-label">连接名称<input v-model="profileForm.name" autofocus autocapitalize="off" autocorrect="off" spellcheck="false" placeholder="例如：生产环境" /></label>
          <div class="form-row"><label class="field-label grow-field">主机地址<input v-model="profileForm.host" autocapitalize="off" autocorrect="off" spellcheck="false" placeholder="服务器 IP 或域名" /></label><label class="field-label port-field">端口<input v-model.number="profileForm.port" type="number" min="1" max="65535" /></label></div>
          <label class="field-label">用户名<input v-model="profileForm.username" autocomplete="username" autocapitalize="off" autocorrect="off" spellcheck="false" placeholder="root" /></label>
          <label class="field-label">密码<input v-model="profileForm.password" type="password" autocomplete="current-password" autocapitalize="off" autocorrect="off" spellcheck="false" placeholder="输入 SSH 密码" /></label>
          <p v-if="formError" class="form-error">{{ formError }}</p>
          <p v-if="testError" class="form-error">{{ testError }}</p>
          <p v-if="testStatus" class="test-status">{{ testStatus }}</p>
          <div v-if="testFingerprintToTrust" class="fingerprint-confirm">
            <span>主机指纹：<code>{{ testFingerprintToTrust }}</code></span>
            <button class="secondary-button" type="button" :disabled="testLoading" @click="testConnection(testFingerprintToTrust)">信任指纹并重试</button>
          </div>
          <div class="credential-note"><Check :size="14" />密码保存于系统凭据管理器，不写入主机资料。</div>
          <div class="modal-actions"><button class="secondary-button" type="button" :disabled="testLoading" @click="testConnection()">{{ testLoading ? "测试中…" : "测试连接" }}</button><button class="secondary-button" type="button" @click="isModalOpen = false">取消</button><button class="primary-button modal-submit" type="submit"><TerminalSquare :size="16" /> 保存并连接</button></div>
        </form>
      </section>
    </div>
    <div v-if="hostTrustRequest" class="modal-backdrop" @click.self="cancelHostTrust">
      <section class="about-modal trust-modal" role="dialog" aria-modal="true" aria-labelledby="trust-title">
        <div class="about-mark"><Server :size="22" /></div>
        <h2 id="trust-title">验证服务器指纹</h2>
        <p class="about-description">{{ hostTrustRequest.knownFingerprint ? "该连接保存的指纹与服务器当前提供的指纹不同。不同端口可能对应不同服务器；请通过可信渠道核对当前指纹后再决定是否更新信任。" : "首次连接此主机。请通过可信渠道核对以下指纹；只有确认匹配时才继续连接。" }}</p>
        <div v-if="hostTrustRequest.knownFingerprint" class="known-fingerprint"><span>已保存</span><code>{{ hostTrustRequest.knownFingerprint }}</code></div>
        <span v-if="hostTrustRequest.knownFingerprint" class="fingerprint-arrow">当前服务器指纹</span>
        <code class="host-fingerprint">{{ hostTrustRequest.fingerprint }}</code>
        <div class="modal-actions trust-actions">
          <button class="secondary-button" type="button" @click="cancelHostTrust">取消连接</button>
          <button class="primary-button modal-submit" type="button" @click="trustHostFingerprint">信任并连接</button>
        </div>
      </section>
    </div>
    <div v-if="aboutOpen" class="modal-backdrop" @click.self="aboutOpen = false">
      <section class="about-modal" role="dialog" aria-modal="true" aria-labelledby="about-title">
        <button class="icon-button modal-close" type="button" aria-label="关闭关于窗口" @click="aboutOpen = false"><X :size="18" /></button>
        <div class="about-mark"><TerminalSquare :size="25" /></div>
        <h2 id="about-title">apex.term</h2>
        <p class="about-version">版本 {{ APP_VERSION }}</p>
        <p class="about-description">一款简洁的桌面终端与 SSH 会话管理工具。</p>
        <p class="about-author">作者：{{ APP_AUTHOR }}</p>
        <div class="about-update">
          <button class="secondary-button" type="button" :disabled="updateStatus === 'checking'" @click="checkForUpdates">
            <RefreshCw :size="14" :class="{ spin: updateStatus === 'checking' }" />
            {{ updateStatus === "checking" ? "检查中…" : "检查更新" }}
          </button>
          <p v-if="updateStatus === 'current'" class="update-message" role="status">当前已是最新版本（{{ APP_VERSION }}）。</p>
          <p v-if="updateStatus === 'available' && latestRelease" class="update-message" role="status">发现新版本 {{ latestRelease.version }}。</p>
          <p v-if="updateError" class="update-error" role="alert">{{ updateError }}</p>
          <button v-if="updateStatus === 'available'" class="primary-button" type="button" @click="openLatestRelease"><Download :size="14" /> 前往 GitHub 下载</button>
        </div>
        <div class="about-credits">Built with Tauri · Vue · Rust</div>
        <button class="secondary-button about-close" type="button" @click="aboutOpen = false">关闭</button>
      </section>
    </div>
    <div v-if="profileToDelete" class="modal-backdrop delete-profile-backdrop" @click.self="profileToDelete = null">
      <section class="about-modal delete-profile-modal" role="alertdialog" aria-modal="true" aria-labelledby="delete-profile-title">
        <button class="icon-button modal-close" type="button" aria-label="取消删除" :disabled="profileDeleteLoading" @click="profileToDelete = null"><X :size="18" /></button>
        <div class="about-mark"><Trash2 :size="22" /></div>
        <h2 id="delete-profile-title">删除连接</h2>
        <p class="about-description">确定删除“{{ profileToDelete.name }}”吗？已保存的 SSH 密码也会从系统凭据管理器中删除。</p>
        <p v-if="profileDeleteError" class="form-error delete-profile-error">{{ profileDeleteError }}</p>
        <div class="modal-actions delete-profile-actions">
          <button class="secondary-button" type="button" :disabled="profileDeleteLoading" @click="profileToDelete = null">取消</button>
          <button class="danger-button" type="button" :disabled="profileDeleteLoading" @click="confirmDeleteProfile">{{ profileDeleteLoading ? "删除中…" : "删除" }}</button>
        </div>
      </section>
    </div>
    <section v-if="updateToastVisible && latestRelease" class="update-toast" role="status" aria-live="polite" aria-label="发现软件更新">
      <div class="update-toast-icon"><Download :size="17" /></div>
      <div class="update-toast-copy">
        <strong>Apex Term 有新版本</strong>
        <span>{{ latestRelease.version }} 已发布。</span>
        <button class="update-toast-action" type="button" @click="openUpdateDetails">查看更新详情</button>
      </div>
      <button class="update-toast-close" type="button" :aria-label="`忽略 ${latestRelease.version} 更新提示`" @click="dismissUpdateToast"><X :size="15" /></button>
    </section>
    <div v-if="sftpOpen" class="modal-backdrop sftp-backdrop" @click.self="sftpOpen = false">
      <section class="sftp-modal" role="dialog" aria-modal="true" aria-labelledby="sftp-title">
        <header class="sftp-header">
          <div class="modal-icon"><Folder :size="18" /></div>
          <div class="sftp-heading"><span class="eyebrow">SECURE FILE TRANSFER</span><h2 id="sftp-title">SFTP 文件</h2></div>
          <button class="icon-button modal-close" type="button" aria-label="关闭 SFTP" @click="sftpOpen = false"><X :size="18" /></button>
        </header>
        <div class="sftp-toolbar">
          <button class="sftp-tool-button" type="button" title="上级目录" :disabled="sftpPath === '/'" @click="refreshSftp(parentSftpPath())"><ChevronDown class="sftp-up-icon" :size="15" /></button>
          <form class="sftp-path-form" @submit.prevent="refreshSftp(sftpPath)"><input v-model="sftpPath" aria-label="远程目录路径" /><button type="submit" title="打开目录"><ChevronDown class="sftp-open-icon" :size="14" /></button></form>
          <button class="sftp-tool-button" type="button" title="新建目录" @click="createSftpDirectory"><FolderPlus :size="16" /></button>
          <button class="sftp-tool-button" type="button" title="上传文件" :disabled="sftpLoading" @click="uploadInput?.click()"><Upload :size="16" /></button>
          <button class="sftp-tool-button" type="button" title="刷新" :disabled="sftpLoading" @click="refreshSftp()"><RefreshCw :size="15" :class="{ spin: sftpLoading }" /></button>
          <input ref="uploadInput" class="hidden-file-input" type="file" @change="uploadSftpFile" />
        </div>
        <div class="sftp-columns"><span>名称</span><span>大小</span><span>操作</span></div>
        <div class="sftp-list">
          <button v-for="entry in sftpEntries" :key="entry.path" class="sftp-entry" type="button" @click="entry.isDir ? refreshSftp(entry.path) : undefined">
            <span class="sftp-entry-name"><Folder v-if="entry.isDir" :size="16" /><File v-else :size="16" /><span>{{ entry.name }}</span></span>
            <span class="sftp-entry-size">{{ entry.isDir ? "文件夹" : formatBytes(entry.size) }}</span>
            <span class="sftp-entry-actions" @click.stop>
              <button v-if="!entry.isDir" type="button" :title="`下载 ${entry.name}`" @click="downloadSftpFile(entry)"><Download :size="15" /></button>
              <button type="button" :title="`删除 ${entry.name}`" @click="deleteSftpEntry(entry)"><Trash2 :size="15" /></button>
            </span>
          </button>
          <div v-if="!sftpLoading && sftpEntries.length === 0 && !sftpError" class="sftp-empty">此目录为空</div>
          <div v-if="sftpLoading" class="sftp-empty">正在读取远程目录…</div>
          <p v-if="sftpError" class="sftp-error">{{ sftpError }}</p>
        </div>
        <footer class="sftp-footer">通过 SSH 加密传输 · 单个文件最大 32 MB</footer>
      </section>
    </div>
  </main>
</template>

<style>
@import url("https://fonts.googleapis.com/css2?family=DM+Sans:wght@400;500;600;700&family=IBM+Plex+Mono:wght@400;500;600&display=swap");
:root { font-family: "DM Sans", "Segoe UI", sans-serif; font-synthesis: none; text-rendering: optimizeLegibility; -webkit-font-smoothing: antialiased; -moz-osx-font-smoothing: grayscale; font-optical-sizing: auto; font-size: 14px; font-weight: 400; color: #282e2b; background: #050505; }
* { box-sizing: border-box; }
html, body, #app { width: 100%; height: 100%; overflow: hidden; }
body { margin: 0; min-width: 360px; min-height: 0; background: #050505; }
button, input, select { font: inherit; }
button { cursor: pointer; }
.app-shell { --canvas:#f4f4ef; --sidebar:#fbfbf7; --surface:#fff; --surface-muted:#f2f3ed; --text:#29312d; --text-soft:#68736c; --text-faint:#9aa29b; --border:#e6e8e1; --accent:#577d60; --accent-bright:#a5d38c; height:100%; min-height:0; overflow:hidden; color:var(--text); background:var(--canvas); display:flex; flex-direction:column; }
.app-shell.theme-paper { --canvas:#f6f4ee; --sidebar:#fffefa; --surface:#fffefa; --surface-muted:#f1f0e9; --text:#3d403a; --text-soft:#797d73; --text-faint:#a3a59a; --border:#e8e5dc; --accent:#8b7050; --accent-bright:#d3b783; }
.app-shell.theme-homebrew { --canvas:#050805; --sidebar:#080d08; --surface:#0a100a; --surface-muted:#101a10; --text:#c7f5c7; --text-soft:#83bb83; --text-faint:#527852; --border:#183018; --accent:#00b800; --accent-bright:#00ff00; }
.session-tab,.welcome-footnote,.modal-header,.modal-actions { display:flex; align-items:center; }
.native-menu-panels { position:static; }
.sessions-popover,.settings-popover { position:fixed; z-index:8; top:12px; left:12px; right:auto; pointer-events:auto; border:1px solid var(--border); border-radius:8px; color:var(--text); background:var(--sidebar); box-shadow:0 12px 34px #0006; }
.sessions-popover { width:320px; max-height:calc(100vh - 24px); display:flex; flex-direction:column; padding:9px; }
.sessions-popover-heading { display:flex; align-items:center; justify-content:space-between; padding:5px 5px 9px; font-size:11px; font-weight:600; }.sessions-popover-heading button { display:inline-flex; align-items:center; gap:4px; padding:5px 8px; border:1px solid var(--border); border-radius:5px; color:var(--accent); background:var(--surface); font-size:10px; }.saved-session-list { min-height:0; max-height:min(260px, calc(100vh - 230px)); flex:0 1 260px; overflow-y:auto; overscroll-behavior:contain; }.saved-session-row { display:flex; align-items:center; border-radius:5px; }.saved-session-row:hover,.saved-session.active { background:var(--surface-muted); }.saved-session { min-width:0; min-height:44px; flex:1; display:flex; align-items:center; gap:10px; padding:5px 8px; border:0; color:var(--text-soft); text-align:left; background:transparent; }.saved-session-copy { min-width:0; flex:1; display:flex; flex-direction:column; gap:3px; }.saved-session-copy strong,.saved-session-copy small { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }.saved-session-copy strong { color:var(--text); font-size:10px; font-weight:500; }.saved-session-copy small { color:var(--text-faint); font-family:"IBM Plex Mono",monospace; font-size:9px; }.saved-session-delete { width:29px; height:29px; display:grid; place-items:center; border:0; border-radius:4px; color:var(--text-faint); background:transparent; }.saved-session-delete:hover { color:#bd5e54; background:#bd5e5415; }.sessions-divider { height:1px; margin:5px 0; background:var(--border); }.session-search { height:31px; margin:0 0 6px; }.sessions-empty { padding:20px 8px; color:var(--text-faint); text-align:center; font-size:10px; }.sessions-footnote { padding:8px 5px 2px; border-top:1px solid var(--border); color:var(--text-faint); font-size:9px; }
.settings-popover { width:218px; padding:14px; }
.settings-title { margin-bottom:12px; color:var(--text); font-size:12px; font-weight:600; }.settings-select { display:flex; align-items:center; justify-content:space-between; gap:10px; margin-top:9px; color:var(--text-soft); font-size:10px; }.settings-select select { width:132px; height:30px; padding:0 7px; border:1px solid var(--border); border-radius:5px; outline:none; color:var(--text); background:var(--surface); font-size:10px; }
.app-body { flex:1; min-height:0; display:grid; grid-template-columns:minmax(0,1fr); }.eyebrow { color:#9aa29b; font-family:"IBM Plex Mono",monospace; font-size:9px; font-weight:500; letter-spacing:1px; }.add-button { width:30px; height:30px; display:grid; place-items:center; color:#f5f8f3; background:#52725b; border:0; border-radius:7px; }.add-button:hover,.primary-button:hover { background:#46644e; }
.search-box { height:35px; margin:0 15px 14px; padding:0 9px; display:flex; align-items:center; gap:8px; border:1px solid var(--border); border-radius:6px; background:var(--surface); color:var(--text-faint); }.search-box svg { width:15px; height:15px; fill:none; stroke:currentColor; stroke-width:1.6; }.search-box input { width:100%; min-width:0; border:0; outline:0; color:var(--text); background:transparent; font-size:11px; }.search-box input::placeholder { color:#a4aba5; }.search-box kbd { padding:1px 5px; border:1px solid var(--border); border-radius:4px; color:#a0a69f; font-family:"IBM Plex Mono",monospace; font-size:10px; }.host-list { flex:1; min-height:0; overflow:auto; padding:0 9px; }.sidebar-empty { min-height:145px; display:flex; flex-direction:column; align-items:center; justify-content:center; gap:10px; color:var(--text-faint); font-size:11px; }.sidebar-empty button { display:flex; align-items:center; gap:3px; padding:6px 9px; color:var(--accent); background:transparent; border:0; font-size:11px; }.host-item { margin-bottom:4px; padding:4px; border:1px solid transparent; border-radius:7px; }.host-item.selected { background:#edf2eb; border-color:#e4ebe1; }.theme-paper .host-item.selected { background:#f4efe5; border-color:#ebe3d5; }.host-main { width:100%; gap:10px; padding:6px 5px; border:0; color:inherit; text-align:left; background:transparent; }.host-icon { width:29px; height:29px; display:grid; flex:0 0 29px; place-items:center; border-radius:6px; color:#73847a; background:#f0f2ed; }.host-copy { min-width:0; flex:1; display:flex; flex-direction:column; gap:3px; }.host-copy strong { overflow:hidden; color:var(--text); font-size:11px; font-weight:600; text-overflow:ellipsis; white-space:nowrap; }.host-copy small { overflow:hidden; color:var(--text-faint); font-family:"IBM Plex Mono",monospace; font-size:9px; text-overflow:ellipsis; white-space:nowrap; }.status-dot,.tab-live-dot,.statusbar-led { width:6px; height:6px; flex:0 0 6px; border-radius:50%; background:#b2b8b1; }.status-dot.connected,.tab-live-dot.connected,.statusbar-led.connected { background:#77a882; box-shadow:0 0 0 3px #77a88220; }.status-dot.connecting,.tab-live-dot.connecting,.statusbar-led.connecting { background:#d4a85a; }.host-actions { justify-content:space-between; padding:0 7px 2px 45px; color:#a1a8a1; font-family:"IBM Plex Mono",monospace; font-size:9px; }.host-actions button { padding:2px; display:grid; place-items:center; border:0; color:#a8aea8; background:transparent; opacity:0; }.host-item:hover .host-actions button { opacity:1; }.host-actions button:hover { color:#bd5e54; }.sidebar-footer { min-height:59px; gap:9px; padding:10px 15px; border-top:1px solid var(--border); }.footer-avatar { width:27px; height:27px; display:grid; place-items:center; border-radius:7px; color:#617361; background:#e8ede4; font-family:"IBM Plex Mono",monospace; font-size:9px; font-weight:600; }.footer-copy { display:flex; flex:1; flex-direction:column; gap:2px; }.footer-copy strong { color:var(--text); font-size:10px; font-weight:600; }.footer-copy span { color:var(--text-faint); font-size:9px; }.version-tag { color:#a6aca5; font-family:"IBM Plex Mono",monospace; font-size:9px; }
.main-panel { min-width:0; min-height:0; display:flex; flex-direction:column; background:var(--canvas); }.toolbar { height:52px; flex:0 0 52px; justify-content:space-between; padding:0 22px; border-bottom:1px solid var(--border); }.breadcrumb { gap:9px; font-size:11px; }.breadcrumb-muted { color:var(--text-faint); }.breadcrumb-separator { color:#c0c5bd; }.breadcrumb-current { color:var(--text); font-weight:500; }.toolbar-controls { gap:11px; }.terminal-state { gap:6px; color:var(--text-faint); font-size:10px; }.terminal-state.connected { color:#60866a; }.terminal-state.connecting { color:#a98449; }.spin { animation:spin 1.1s linear infinite; } @keyframes spin { to { transform:rotate(360deg); } }.toolbar-divider { width:1px; height:17px; background:var(--border); }.select-control { gap:5px; color:var(--text-soft); }.select-control select { max-width:127px; padding:3px 0; appearance:none; border:0; outline:0; color:var(--text-soft); background:transparent; font-size:10px; cursor:pointer; }.select-control select option { color:#29312d; background:#fff; }.size-control select { width:43px; }.session-strip { height:39px; flex:0 0 39px; display:flex; align-items:stretch; gap:2px; padding:0 15px; background:var(--sidebar); border-bottom:1px solid var(--border); }.session-tab { position:relative; gap:8px; padding:0 12px; color:var(--text-soft); font-size:10px; }.session-tab::after { position:absolute; right:0; bottom:-1px; left:0; height:2px; content:""; background:var(--accent); }.tab-live-dot { width:6px; height:6px; flex-basis:6px; }.tab-host { color:var(--text-faint); font-family:"IBM Plex Mono",monospace; font-size:9px; }.session-tab button { width:19px; height:19px; display:grid; place-items:center; margin-left:4px; border:0; border-radius:4px; color:var(--text-faint); background:transparent; }.session-tab button:hover { color:var(--text); background:var(--surface-muted); }.new-tab-button { width:30px; display:grid; place-items:center; border:0; color:var(--text-soft); background:transparent; }.new-tab-button:hover { color:var(--text); }
.terminal-stage { position:relative; flex:1; min-height:0; margin:15px 17px 10px; overflow:hidden; border:1px solid #2b332e; border-radius:8px; background:#171c1a; box-shadow:0 5px 18px #26372d10; }.terminal-stage.terminal-paper { border-color:#dfddd4; background:#f5f4ef; }.terminal-mount { position:absolute; inset:18px; }.terminal-mount .xterm { height:100%; }
.welcome-stage { position:relative; flex:1; min-height:0; display:grid; place-items:center; overflow:hidden; background:radial-gradient(ellipse at 51% 46%,#fbfcf8 0%,#f4f5ef 52%,#eef0e9 100%); }.theme-paper .welcome-stage { background:radial-gradient(ellipse at 51% 46%,#fffefa 0%,#f7f4ec 57%,#efece3 100%); }.welcome-stage::before { position:absolute; inset:0; content:""; opacity:.22; background-image:radial-gradient(#869584 .55px,transparent .55px); background-size:17px 17px; mask-image:linear-gradient(90deg,transparent 15%,#000 50%,transparent 85%); }.welcome-content { position:relative; z-index:1; display:flex; flex-direction:column; align-items:center; padding:28px 20px; text-align:center; animation:rise-in .55s ease-out both; } @keyframes rise-in { from { opacity:0; transform:translateY(9px); } to { opacity:1; transform:translateY(0); } }.welcome-art { position:relative; width:132px; height:112px; margin-bottom:14px; display:grid; place-items:center; }.orbit { position:absolute; width:112px; height:66px; border:1px solid #cbd5c8; border-radius:50%; transform:rotate(-28deg); }.orbit-two { width:91px; height:92px; border-color:#d6ddd2; transform:rotate(36deg); }.welcome-terminal { position:relative; width:63px; height:51px; display:flex; align-items:center; padding-left:13px; border:1px solid #647b67; border-radius:8px; color:#dcebdd; background:#405747; box-shadow:0 9px 17px #31463630; font-family:"IBM Plex Mono",monospace; font-size:18px; }.welcome-terminal i { position:absolute; right:13px; bottom:14px; width:7px; height:2px; background:#b5d6af; animation:blink 1.2s steps(2,start) infinite; } @keyframes blink { to { visibility:hidden; } }.art-spark { position:absolute; color:#6c8a69; font-family:"IBM Plex Mono",monospace; font-size:18px; }.spark-one { top:5px; right:18px; }.spark-two { bottom:8px; left:12px; color:#b39d6d; font-size:28px; }.welcome-kicker { display:flex; align-items:center; gap:7px; color:#829183; font-family:"IBM Plex Mono",monospace; font-size:9px; letter-spacing:1.2px; }.welcome-kicker span { width:5px; height:5px; border-radius:50%; background:#88a78a; }.welcome-content h2 { margin:15px 0 8px; color:#303b34; font-size:31px; font-weight:600; line-height:1.2; }.welcome-content h2 em { color:#618068; font-style:normal; }.welcome-content p { margin:0 0 22px; color:#869088; font-size:11px; }.primary-button { min-height:37px; display:inline-flex; align-items:center; justify-content:center; gap:7px; padding:0 14px; border:0; border-radius:6px; color:#f5f8f3; background:#55765d; font-size:11px; font-weight:500; transition:background .15s ease; }.welcome-footnote { gap:9px; margin-top:22px; color:#a0a69d; font-size:9px; }.footnote-line { width:21px; height:1px; background:#d5dad2; }.welcome-corner { position:absolute; right:22px; bottom:17px; color:#a7aea4; font-family:"IBM Plex Mono",monospace; font-size:9px; }.welcome-corner span { color:#748a75; }.main-footer { height:29px; flex:0 0 29px; justify-content:space-between; padding:0 18px; color:#a0a79e; font-family:"IBM Plex Mono",monospace; font-size:9px; }.main-footer>span:first-child { display:flex; align-items:center; gap:7px; }.footer-pulse { width:5px; height:5px; }
.modal-backdrop { position:fixed; z-index:5; inset:0; display:grid; place-items:center; padding:20px; background:#17201ab0; backdrop-filter:blur(3px); }.connection-modal { width:min(100%,430px); max-height:calc(100vh - 40px); overflow-y:auto; padding:22px; border:1px solid #e6e8e1; border-radius:10px; background:#fbfbf7; box-shadow:0 22px 70px #17201a35; animation:rise-in .2s ease-out both; }.modal-header { position:relative; gap:12px; margin-bottom:21px; }.modal-icon { width:37px; height:37px; display:grid; place-items:center; border-radius:8px; color:#56745c; background:#e9efe6; }.modal-header h2 { margin:3px 0 0; color:#29312d; font-size:17px; font-weight:600; }.modal-close { position:absolute; top:0; right:0; }.connection-form { display:flex; flex-direction:column; gap:13px; }.field-label { display:flex; flex-direction:column; gap:6px; color:#626c64; font-size:10px; font-weight:500; }.field-label input { width:100%; height:37px; padding:0 10px; outline:none; border:1px solid #e1e5dd; border-radius:5px; color:#303832; background:#fff; font-size:11px; }.field-label input:focus { border-color:#78967b; box-shadow:0 0 0 3px #7193761c; }.field-label input::placeholder { color:#b3b9b2; }.form-row { display:flex; gap:10px; }.grow-field { flex:1; }.port-field { width:92px; }.form-error { margin:-4px 0 0; color:#b9574f; font-size:10px; }.credential-note { display:flex; align-items:center; gap:6px; margin-top:1px; color:#758778; font-size:10px; }.credential-note svg { color:#6d9473; }.modal-actions { justify-content:flex-end; gap:8px; margin-top:8px; padding-top:14px; border-top:1px solid #e9ebe5; }.secondary-button { min-height:36px; padding:0 13px; border:1px solid #e0e4dc; border-radius:6px; color:#687168; background:#fff; font-size:11px; }.secondary-button:hover { background:#f2f3ed; }.modal-submit { min-height:36px; }
.primary-button:disabled { cursor:not-allowed; opacity:.55; }
.welcome-content .runtime-message { max-width:320px; color:#9b6b4a; }
.saved-session-edit { width:29px; height:29px; display:grid; place-items:center; border:0; border-radius:4px; color:var(--text-faint); background:transparent; }.saved-session-edit:hover { color:var(--accent); background:var(--surface-muted); }.about-modal { position:relative; width:min(100%,360px); padding:28px 26px 22px; border:1px solid var(--border); border-radius:10px; color:var(--text); text-align:center; background:var(--sidebar); box-shadow:0 22px 70px #17201a35; animation:rise-in .2s ease-out both; }.about-modal .modal-close { top:14px; right:14px; }.about-mark { width:50px; height:50px; display:grid; place-items:center; margin:3px auto 13px; border-radius:12px; color:#ecf3e9; background:#384c3e; }.about-modal h2 { margin:0; font-size:19px; font-weight:650; }.about-version { margin:6px 0 17px; color:var(--text-faint); font-family:"IBM Plex Mono",monospace; font-size:10px; }.about-description { margin:0 auto; max-width:260px; color:var(--text-soft); font-size:11px; line-height:1.7; }.about-credits { margin:18px 0; padding-top:14px; border-top:1px solid var(--border); color:var(--text-faint); font-family:"IBM Plex Mono",monospace; font-size:9px; }.about-close { min-width:76px; }
.theme-homebrew .terminal-stage { border-color:#183018; background:#000; box-shadow:0 0 24px #00ff0010; }.theme-homebrew .terminal-stage.terminal-homebrew { border-color:#183018; background:#000; }.terminal-homebrew .terminal-statusbar { color:#00b800; background:#071007; }.theme-homebrew .welcome-stage { background:radial-gradient(ellipse at 50% 45%,#0b140b 0%,#050805 70%); }.theme-homebrew .welcome-content h2 { color:#c7f5c7; }.theme-homebrew .welcome-content h2 em { color:#00d900; }.theme-homebrew .welcome-content p,.theme-homebrew .welcome-footnote { color:#83bb83; }
.settings-wrap { position:relative; }.settings-popover { position:absolute; z-index:8; top:41px; right:0; width:218px; padding:14px; border:1px solid var(--border); border-radius:8px; background:var(--sidebar); box-shadow:0 12px 34px #17201a20; }.settings-title { margin-bottom:12px; color:var(--text); font-size:12px; font-weight:600; }.settings-select { display:flex; align-items:center; justify-content:space-between; gap:10px; margin-top:9px; color:var(--text-soft); font-size:10px; }.settings-select select { width:132px; height:30px; padding:0 7px; border:1px solid var(--border); border-radius:5px; outline:none; color:var(--text); background:var(--surface); font-size:10px; }.sftp-open-button { display:inline-flex; align-items:center; gap:6px; padding:6px 9px; border:1px solid var(--border); border-radius:5px; color:var(--text-soft); background:var(--sidebar); font-size:10px; }.sftp-open-button:hover { border-color:#b6c7b4; color:var(--accent); }.sftp-backdrop { z-index:6; }.sftp-modal { width:min(100%,680px); height:min(700px, calc(100vh - 40px)); min-height:320px; display:flex; flex-direction:column; overflow:hidden; border:1px solid #e6e8e1; border-radius:10px; color:#29312d; background:#fbfbf7; box-shadow:0 22px 70px #17201a35; animation:rise-in .2s ease-out both; }.sftp-header { position:relative; min-height:72px; display:flex; align-items:center; gap:12px; padding:16px 20px; border-bottom:1px solid #e6e8e1; }.sftp-heading h2 { margin:3px 0 0; font-size:16px; font-weight:600; }.sftp-toolbar { display:flex; align-items:center; gap:7px; padding:11px 16px; border-bottom:1px solid #e9ebe5; }.sftp-tool-button { width:31px; height:31px; flex:0 0 31px; display:grid; place-items:center; border:1px solid #e2e5de; border-radius:5px; color:#68736c; background:#fff; }.sftp-tool-button:hover:not(:disabled) { color:#577d60; border-color:#b9cbb7; }.sftp-tool-button:disabled { cursor:default; opacity:.45; }.sftp-up-icon { transform:rotate(90deg); }.sftp-open-icon { transform:rotate(-90deg); }.sftp-path-form { min-width:0; height:31px; flex:1; display:flex; overflow:hidden; border:1px solid #e2e5de; border-radius:5px; background:#fff; }.sftp-path-form input { width:100%; min-width:0; padding:0 9px; border:0; outline:0; color:#526056; font-family:"IBM Plex Mono",monospace; font-size:10px; }.sftp-path-form button { width:30px; flex:0 0 30px; display:grid; place-items:center; border:0; border-left:1px solid #edf0e9; color:#768178; background:transparent; }.hidden-file-input { display:none; }.sftp-columns,.sftp-entry { display:grid; grid-template-columns:minmax(0,1fr) 90px 72px; align-items:center; gap:8px; }.sftp-columns { padding:9px 19px; color:#929b92; background:#f5f6f1; font-size:9px; }.sftp-columns span:last-child { text-align:right; }.sftp-list { flex:1; min-height:0; overflow:auto; padding:3px 10px; }.sftp-entry { width:100%; min-height:38px; padding:0 8px; border:0; border-bottom:1px solid #eef0eb; color:#526056; text-align:left; background:transparent; }.sftp-entry:hover { background:#f3f6f1; }.sftp-entry-name { min-width:0; display:flex; align-items:center; gap:9px; color:#5d7661; }.sftp-entry-name span { overflow:hidden; color:#3d4840; font-size:10px; text-overflow:ellipsis; white-space:nowrap; }.sftp-entry-size { color:#8d978e; font-family:"IBM Plex Mono",monospace; font-size:9px; }.sftp-entry-actions { display:flex; justify-content:flex-end; gap:4px; }.sftp-entry-actions button { width:27px; height:27px; display:grid; place-items:center; border:0; border-radius:4px; color:#89948a; background:transparent; }.sftp-entry-actions button:hover { color:#bd5e54; background:#f8eeec; }.sftp-empty { padding:30px 12px; color:#9aa29b; text-align:center; font-size:11px; }.sftp-error { margin:10px; padding:9px 10px; border:1px solid #efd7d3; border-radius:5px; color:#a34e47; background:#fff7f5; font-size:10px; overflow-wrap:anywhere; }.sftp-footer { padding:10px 17px; border-top:1px solid #e9ebe5; color:#929b92; font-size:9px; }
@media (max-width:900px) { .app-body { grid-template-columns:minmax(0,1fr); }.toolbar { padding:0 14px; }.toolbar-controls { gap:8px; }.terminal-stage { margin-right:10px; margin-left:10px; } }
@media (max-width:760px) { .app-body { grid-template-columns:minmax(0,1fr); grid-template-rows:minmax(0,1fr); }.menu-button { gap:4px; padding:0 7px; font-size:10px; }.sessions-popover { width:min(320px,calc(100vw - 24px)); }.settings-popover { width:min(218px,calc(100vw - 24px)); }.toolbar { height:45px; flex-basis:45px; }.terminal-stage { margin:8px; }.terminal-mount { inset:12px 10px 35px; }.welcome-content h2 { font-size:27px; }.welcome-art { transform:scale(.85); margin-bottom:2px; }.sftp-columns,.sftp-entry { grid-template-columns:minmax(0,1fr) 65px 65px; gap:4px; }.sftp-header { padding:13px; }.sftp-toolbar { gap:5px; padding:9px; } }
.about-author { margin:14px 0 0; color:var(--text-soft); font-size:10px; }
@media (max-width:760px) { .topbar-right { margin-left:10px; }.sessions-popover,.settings-popover { left:auto; right:8px; } }
.app-shell { background:var(--canvas); transition:opacity 100ms linear; }
.app-shell.theme-midnight { --canvas:#111827; --sidebar:#172033; --surface:#1e293b; --surface-muted:#263449; --text:#dbeafe; --text-soft:#a9bdd8; --text-faint:#7185a0; --border:#334155; --accent:#60a5fa; --accent-bright:#93c5fd; }
.app-shell.theme-graphite { --canvas:#181a1d; --sidebar:#1e2125; --surface:#25292e; --surface-muted:#2d3238; --text:#e3e6e9; --text-soft:#b3bac2; --text-faint:#7c858f; --border:#353b42; --accent:#aeb8c2; --accent-bright:#d3d9df; }
.app-shell.theme-paper { --canvas:#f6f4ee; --sidebar:#fffefa; --surface:#fffefa; --surface-muted:#f1f0e9; --text:#3d403a; --text-soft:#797d73; --text-faint:#a3a59a; --border:#e8e5dc; --accent:#8b7050; --accent-bright:#d3b783; }
.app-shell.theme-homebrew { --canvas:#050805; --sidebar:#080d08; --surface:#0a100a; --surface-muted:#101a10; --text:#c7f5c7; --text-soft:#83bb83; --text-faint:#527852; --border:#183018; --accent:#00b800; --accent-bright:#00ff00; }
.app-shell.theme-dracula { --canvas:#282a36; --sidebar:#22232e; --surface:#303341; --surface-muted:#393c4d; --text:#f8f8f2; --text-soft:#c7c7d1; --text-faint:#8b8da0; --border:#44475a; --accent:#bd93f9; --accent-bright:#ff79c6; }
.app-shell.theme-nord { --canvas:#2e3440; --sidebar:#272d38; --surface:#3b4252; --surface-muted:#434c5e; --text:#eceff4; --text-soft:#d8dee9; --text-faint:#9aa6b7; --border:#4c566a; --accent:#88c0d0; --accent-bright:#8fbcbb; }
.app-shell.theme-solarized { --canvas:#002b36; --sidebar:#073642; --surface:#0b3b46; --surface-muted:#124752; --text:#eee8d5; --text-soft:#93a1a1; --text-faint:#657b83; --border:#214b54; --accent:#2aa198; --accent-bright:#b58900; }
.app-shell.theme-monokai { --canvas:#272822; --sidebar:#22231d; --surface:#30312a; --surface-muted:#3b3c34; --text:#f8f8f2; --text-soft:#c5c5b9; --text-faint:#88877b; --border:#49483e; --accent:#a6e22e; --accent-bright:#f92672; }
.app-shell .main-panel,.app-shell .toolbar,.app-shell .terminal-stage { background-color:var(--canvas); }
.app-shell .session-strip { background-color:var(--sidebar); }
.app-shell .terminal-stage { border-color:var(--border); }
.theme-graphite .terminal-stage { background:#151719; box-shadow:0 5px 18px #0003; }
.theme-graphite .welcome-stage { background:radial-gradient(ellipse at 50% 45%,#24282d 0%,#181a1d 72%); }
.theme-graphite .welcome-stage::before { opacity:.12; background-image:radial-gradient(#aeb8c2 .55px,transparent .55px); }
.theme-graphite .welcome-kicker,.theme-graphite .welcome-content p,.theme-graphite .welcome-footnote { color:#aab2bb; }
.theme-graphite .welcome-content h2 { color:#e3e6e9; }
.theme-graphite .welcome-content h2 em { color:#b8c2cc; }
.theme-graphite .primary-button,.theme-graphite .add-button { color:#f1f3f5; background:#59636e; }
.theme-graphite .primary-button:hover,.theme-graphite .add-button:hover { background:#68737f; }
.theme-graphite .host-item.selected { background:#25292e; border-color:#353b42; }
.theme-graphite .host-icon { color:#aeb8c2; background:#282d33; }
.theme-graphite .about-mark { color:#e3e6e9; background:#353b42; }
.settings-popover,.sessions-popover { color:var(--text); border-color:var(--border); background:var(--sidebar); }
.settings-select,.sessions-popover-heading,.saved-session-copy strong { color:var(--text); }
.settings-select select,.search-box,.sessions-popover-heading button { color:var(--text); border-color:var(--border); background:var(--surface); }
.session-search input { color:#e5e5e5; }
.connection-modal,.about-modal,.sftp-modal { color:#e5e5e5; border-color:#292929; background:#090909; }
.modal-header h2,.about-modal h2 { color:#f0f0f0; }
.modal-icon { color:#39e879; background:#102016; }
.field-label { color:#b2b2b2; }
.field-label input { color:#ededed; border-color:#333; background:#121212; }
.field-label input::placeholder { color:#777; }
.field-label input:focus { border-color:#39a867; box-shadow:0 0 0 3px #39a86720; }
.credential-note,.about-description,.about-author { color:#a0a0a0; }
.credential-note svg { color:#39e879; }
.modal-actions,.about-credits { border-color:#292929; }
.secondary-button { color:#d0d0d0; border-color:#383838; background:#151515; }
.secondary-button:hover { background:#202020; }
.trust-modal { text-align:left; }
.trust-modal .about-mark { margin-left:0; }
.host-fingerprint { display:block; margin-top:16px; padding:12px; overflow-wrap:anywhere; border:1px solid #333; border-radius:6px; color:#75e89e; background:#111; font-family:"IBM Plex Mono",monospace; font-size:11px; }
.known-fingerprint { display:flex; flex-direction:column; gap:5px; margin-top:15px; padding:10px 12px; overflow-wrap:anywhere; border:1px solid #5d302d; border-radius:6px; color:#ffaaa3; background:#271614; font-size:10px; }
.known-fingerprint code { font-family:"IBM Plex Mono",monospace; }
.fingerprint-arrow { display:block; margin-top:11px; color:#a0a0a0; font-size:10px; }
.trust-actions { justify-content:flex-end; }
.menu-backdrop { z-index:10; cursor:default; }
.menu-dialog { position:relative; top:auto; left:auto; right:auto; max-height:calc(100vh - 40px); overflow-y:auto; }
.session-strip { min-height:46px; overflow-x:auto; }
.session-tab { flex:0 0 auto; cursor:pointer; }
.session-tab-active { color:var(--text); background:var(--surface-muted); box-shadow:inset 0 -2px var(--accent); }
.session-tab button { display:grid; place-items:center; }
.sftp-header { border-color:#292929; }
.sftp-toolbar { border-color:#292929; background:#0d0d0d; }
.sftp-tool-button,.sftp-path-form { color:#d0d0d0; border-color:#383838; background:#151515; }
.sftp-tool-button:hover:not(:disabled) { color:#75e89e; border-color:#39a867; }
.sftp-path-form input { color:#e5e5e5; background:#151515; }
.sftp-path-form button { color:#b8b8b8; border-color:#383838; }
.sftp-columns { color:#a5b2a8; background:#111; }
.sftp-entry { color:#d0d0d0; border-color:#252525; }
.sftp-entry:hover { background:#151a16; }
.sftp-entry-name,.sftp-entry-name span { color:#d8eadb; }
.sftp-entry-size,.sftp-entry-actions button { color:#a0a0a0; }
.sftp-entry-actions button:hover { color:#ff8a80; background:#351c1c; }
.sftp-empty,.sftp-footer { color:#a0a0a0; }
.sftp-footer { border-color:#292929; }
.sftp-error { color:#ffaaa3; border-color:#5d302d; background:#271614; }
.local-session-button { min-height:56px; gap:13px; padding:9px 12px; }
.local-session-button > svg { width:19px; height:19px; flex:0 0 19px; }
.local-session-button .saved-session-copy strong { font-size:12px; }
.test-status { margin:-4px 0 0; color:#75e89e; font-size:10px; }
.fingerprint-confirm { display:flex; flex-direction:column; gap:8px; padding:10px; border:1px solid #66572e; border-radius:6px; color:#e6d39a; background:#211c10; font-size:10px; overflow-wrap:anywhere; }
.fingerprint-confirm code { color:#f0dfa7; font-family:"IBM Plex Mono",monospace; }
.delete-profile-backdrop { z-index:12; }
.delete-profile-modal { text-align:left; }
.delete-profile-modal .about-mark { margin-left:0; color:#ffaaa3; background:#351c1c; }
.delete-profile-modal .about-description { margin:12px 0 0; }
.delete-profile-error { margin-top:12px; overflow-wrap:anywhere; }
.delete-profile-actions { margin-top:18px; }
.danger-button { min-height:36px; padding:0 14px; border:1px solid #813d39; border-radius:6px; color:#fff; background:#8f3934; font-size:11px; }
.danger-button:hover:not(:disabled) { background:#a6443e; }
.danger-button:disabled { cursor:wait; opacity:.6; }
.settings-popover { width:min(275px,calc(100vw - 24px)); }
.wallpaper-setting { display:flex; flex-direction:column; gap:8px; margin-top:14px; padding-top:12px; border-top:1px solid var(--border); color:var(--text-soft); font-size:10px; }
.wallpaper-actions { display:flex; gap:7px; }
.wallpaper-actions button { min-height:29px; padding:0 9px; border:1px solid var(--border); border-radius:5px; color:var(--text); background:var(--surface); font-size:10px; }
.wallpaper-actions button:hover { border-color:var(--accent); color:var(--accent); }
.wallpaper-opacity { display:flex; flex-direction:column; gap:6px; margin-top:2px; }
.wallpaper-opacity span { display:flex; justify-content:space-between; color:var(--text-soft); }
.wallpaper-opacity strong { color:var(--accent); font-weight:600; }
.wallpaper-opacity input { width:100%; accent-color:var(--accent); }
.wallpaper-hint { color:var(--text-faint); font-size:9px; }
.wallpaper-error { color:#d66b62; font-size:10px; overflow-wrap:anywhere; }
.terminal-stage { background-position:center; background-size:cover; background-repeat:no-repeat; }
.terminal-stage.has-wallpaper { margin:0; border:0; border-radius:0; background:var(--canvas); box-shadow:none; isolation:isolate; }
.terminal-stage.has-wallpaper::before,.terminal-stage.has-wallpaper::after { position:absolute; inset:0; content:""; pointer-events:none; }
.terminal-stage.has-wallpaper::before { z-index:-2; background-image:var(--wallpaper-image); background-position:center; background-size:cover; background-repeat:no-repeat; }
.terminal-stage.has-wallpaper::after { z-index:-1; background:var(--canvas); opacity:var(--wallpaper-overlay-opacity); }
.terminal-stage.has-wallpaper .terminal-mount { z-index:0; }
.terminal-stage.has-wallpaper .terminal-mount .xterm .xterm-viewport { background-color:transparent !important; }
.terminal-mount .xterm-viewport { scrollbar-width:none; }
.terminal-mount .xterm-viewport::-webkit-scrollbar { display:none; width:0; height:0; }
.app-titlebar { position:relative; z-index:11; height:46px; min-height:46px; flex:0 0 46px; overflow:visible; padding:0 0 0 15px; user-select:none; color:var(--text); background:var(--sidebar); border-color:var(--border); }
.app-titlebar .session-tab { min-width:72px; max-width:240px; flex:0 1 auto; overflow:hidden; color:var(--text-soft); }
.app-titlebar .session-tab-active { color:var(--text); background:var(--surface-muted); box-shadow:inset 0 -2px var(--accent); }
.app-titlebar .tab-title { min-width:24px; flex:1 1 auto; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.app-titlebar .session-tab::after { display:block; }
.window-menu-wrap { position:relative; display:flex; flex:0 0 auto; align-items:center; }
.window-menu-button { width:32px; height:30px; display:grid; place-items:center; border:0; border-radius:4px; color:var(--text-soft); background:transparent; }
.window-menu-button:hover,.window-menu-button[aria-expanded="true"] { color:var(--text); background:var(--surface-muted); }
.window-menu-dropdown { position:absolute; z-index:1; top:38px; left:0; width:190px; padding:5px; border:1px solid var(--border); border-radius:6px; background:var(--sidebar); box-shadow:0 12px 30px #0005; }
.window-menu-dropdown button { width:100%; min-height:34px; display:flex; align-items:center; gap:9px; padding:0 9px; border:0; border-radius:4px; color:var(--text-soft); text-align:left; background:transparent; font-size:11px; }
.window-menu-dropdown button:hover { color:var(--text); background:var(--surface-muted); }
.window-menu-divider { height:1px; margin:5px 3px; background:var(--border); }
.app-shell .modal-backdrop { background:#080a0de0; }
.app-shell .sessions-popover,.app-shell .settings-popover { color:#e3e6e9; border-color:#3a4047; background:#202328; box-shadow:0 24px 64px #0009; }
.app-shell .sessions-popover-heading,.app-shell .settings-title,.app-shell .settings-select { color:#e3e6e9; }
.app-shell .sessions-popover-heading button,.app-shell .settings-select select,.app-shell .sessions-popover .search-box { color:#d5dae0; border-color:#3a4047; background:#292e34; }
.app-shell .sessions-popover-heading button:hover { border-color:#626c77; background:#343a42; }
.app-shell .saved-session { color:#b9c0c8; }
.app-shell .saved-session-copy strong { color:#e3e6e9; }
.app-shell .saved-session-copy small,.app-shell .sessions-footnote { color:#929ba5; }
.app-shell .saved-session-row:hover,.app-shell .saved-session.active { background:#2b3036; }
.app-shell .sessions-divider { background:#3a4047; }
.app-shell .saved-session-delete,.app-shell .saved-session-edit { color:#929ba5; }
.app-shell .saved-session-delete:hover,.app-shell .saved-session-edit:hover { color:#e3e6e9; background:#343a42; }
.app-shell .connection-modal,.app-shell .about-modal,.app-shell .sftp-modal { color:#dfe3e8; border-color:#3a4047; background:#202328; box-shadow:0 28px 80px #0009; }
.app-shell .modal-header h2,.app-shell .about-modal h2 { color:#f0f2f4; }
.app-shell .modal-icon { color:#d5dbe2; background:#30363d; }
.app-shell .connection-modal .eyebrow { color:#858f9a; }
.app-shell .field-label { color:#b9c0c8; }
.app-shell .field-label input { color:#e3e6e9; border-color:#3a4047; background:#171a1e; }
.app-shell .field-label input::placeholder { color:#737d87; }
.app-shell .field-label input:focus { border-color:#8995a2; box-shadow:0 0 0 3px #aeb8c21a; }
.app-shell .credential-note { color:#aab3bd; }
.app-shell .credential-note svg { color:#aeb8c2; }
.app-shell .modal-actions { border-color:#3a4047; }
.app-shell .secondary-button { color:#c8ced5; border-color:#454c54; background:#2a2f35; }
.app-shell .secondary-button:hover { color:#fff; background:#363c44; }
.app-shell .connection-modal .primary-button { color:#171a1e; background:#c1c8d0; }
.app-shell .connection-modal .primary-button:hover { background:#d4dae0; }
.app-shell .modal-close { position:absolute; top:16px; right:16px; width:32px; height:32px; display:grid; place-items:center; padding:0; border:1px solid #414850; border-radius:9px; color:#aeb7c1; background:#292e34; transition:background .15s ease,color .15s ease,border-color .15s ease,transform .15s ease; }
.app-shell .modal-close:hover { transform:rotate(90deg); color:#fff; border-color:#65707b; background:#3a4149; }
.app-shell .modal-close:focus-visible { outline:2px solid #aeb8c2; outline-offset:2px; }
.app-shell .about-modal .modal-close { top:14px; right:14px; }
.session-tab { min-width:72px; max-width:240px; flex:0 1 auto; overflow:hidden; }
.session-tab .tab-title { min-width:24px; flex:1 1 auto; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.session-strip { height:46px; min-height:46px; flex-basis:46px; }
.app-titlebar .window-menu-button { color:var(--text-soft); }
.app-titlebar .window-menu-button:hover { color:var(--text); background:var(--surface-muted); }
.app-titlebar .window-menu-dropdown { color:var(--text); border-color:var(--border); background:var(--sidebar); }
.app-titlebar .window-menu-dropdown button { color:var(--text-soft); }
.app-titlebar .window-menu-dropdown button:hover { color:var(--text); background:var(--surface-muted); }
.session-strip { position:relative; z-index:11; border-bottom:1px solid #ffffff12; background:linear-gradient(180deg,#20262de8,#11161bdc); box-shadow:0 8px 24px #0004,inset 0 1px #ffffff12; -webkit-backdrop-filter:blur(22px) saturate(150%); backdrop-filter:blur(22px) saturate(150%); }
.app-titlebar { border-color:#ffffff12; background:linear-gradient(180deg,#20262de8,#11161bdc); }
.session-tab { min-height:34px; gap:8px; margin:5px 3px; padding:0 9px; border:1px solid #ffffff0b; border-radius:9px; color:#aeb8c3; background:linear-gradient(180deg,#ffffff0b,#ffffff04); box-shadow:inset 0 1px #ffffff0a,0 2px 5px #0002; transition:background .18s ease,border-color .18s ease,color .18s ease,box-shadow .18s ease,transform .18s ease; }
.session-tab:hover { color:#e8eef3; border-color:#ffffff20; background:linear-gradient(180deg,#ffffff14,#ffffff08); box-shadow:inset 0 1px #ffffff14,0 4px 9px #0003; }
.session-tab-active,.app-titlebar .session-tab-active { color:#f2f8f2; border-color:#55d77a45; background:linear-gradient(180deg,#16472fd9,#102b20d9); box-shadow:inset 0 1px #a4ffc124,inset 0 -1px #0004,0 3px 10px #00a73d20; }
.session-tab-active:hover,.app-titlebar .session-tab-active:hover { color:#fff; border-color:#69ec8a70; background:linear-gradient(180deg,#1b5739e8,#123824e8); }
.session-tab-active { box-shadow:inset 0 1px #a4ffc124,inset 0 -1px #0004,0 3px 10px #00a73d20; }
.session-tab button { width:21px; height:21px; flex:0 0 21px; border:0; border-radius:6px; color:#82908a; background:transparent; }
.session-tab button:hover { color:#fff; background:#ffffff18; }
.tab-live-dot.connected { box-shadow:0 0 9px #66f28a99,0 0 0 3px #66f28a20; }
.new-tab-button,.app-titlebar .new-tab-button { width:31px; height:31px; flex:0 0 31px; align-self:center; margin:0 4px; border:1px solid #ffffff14; border-radius:9px; color:#c0ccd2; background:linear-gradient(180deg,#ffffff11,#ffffff06); box-shadow:inset 0 1px #ffffff12,0 2px 6px #0003; transition:all .18s ease; }
.new-tab-button:hover,.app-titlebar .new-tab-button:hover { color:#fff; border-color:#66e58a60; background:linear-gradient(180deg,#ffffff20,#ffffff0a); box-shadow:inset 0 1px #ffffff20,0 4px 10px #0004; }
.window-menu-button,.app-titlebar .window-menu-button { border:1px solid transparent; border-radius:8px; color:#b9c4cc; }
.window-menu-button:hover,.window-menu-button[aria-expanded="true"],.app-titlebar .window-menu-button:hover,.app-titlebar .window-menu-button[aria-expanded="true"] { border-color:#ffffff16; color:#f1f6f8; background:#ffffff0c; }
.window-menu-dropdown,.app-titlebar .window-menu-dropdown { border-color:#ffffff24; border-radius:11px; color:#e3ebef; background:linear-gradient(145deg,#242c32f2,#14191ff2); box-shadow:0 18px 44px #0009,inset 0 1px #ffffff12; -webkit-backdrop-filter:blur(24px) saturate(145%); backdrop-filter:blur(24px) saturate(145%); }
.window-menu-dropdown button,.app-titlebar .window-menu-dropdown button { color:#c0cbd1; }
.window-menu-dropdown button:hover,.app-titlebar .window-menu-dropdown button:hover { color:#fff; background:#ffffff10; }
.app-shell .modal-backdrop { background:#030605a6; -webkit-backdrop-filter:blur(8px) saturate(115%); backdrop-filter:blur(8px) saturate(115%); }
.app-shell .sessions-popover,.app-shell .settings-popover,.app-shell .connection-modal,.app-shell .about-modal,.app-shell .sftp-modal { border-color:#ffffff24; background:linear-gradient(145deg,#283039ed 0%,#1a2027ed 48%,#14191fed 100%); box-shadow:0 30px 90px #000b,0 8px 24px #0006,inset 0 1px #ffffff1c,inset 1px 0 #ffffff08; -webkit-backdrop-filter:blur(28px) saturate(150%); backdrop-filter:blur(28px) saturate(150%); }
.app-shell .sessions-popover,.app-shell .settings-popover { color:#e3eaf0; }
.app-shell .sessions-popover-heading,.app-shell .settings-title,.app-shell .settings-select { color:#e9eef2; }
.app-shell .sessions-popover-heading button,.app-shell .settings-select select,.app-shell .sessions-popover .search-box { border-color:#ffffff20; background:linear-gradient(180deg,#ffffff0d,#ffffff06); box-shadow:inset 0 1px #ffffff0a; }
.app-shell .sessions-popover-heading button:hover,.app-shell .settings-select select:focus { border-color:#69d78b70; background:#ffffff12; }
.app-shell .saved-session-row:hover,.app-shell .saved-session.active { background:#ffffff0d; box-shadow:inset 0 1px #ffffff08; }
.app-shell .connection-modal,.app-shell .about-modal,.app-shell .sftp-modal { border-radius:18px; }
.app-shell .modal-icon,.app-shell .about-mark { border:1px solid #ffffff16; color:#b9f1c8; background:linear-gradient(145deg,#ffffff18,#ffffff08); box-shadow:inset 0 1px #ffffff14,0 5px 12px #0003; }
.app-shell .field-label input,.app-shell .secondary-button,.app-shell .wallpaper-actions button { border-color:#ffffff20; background:linear-gradient(180deg,#ffffff0d,#ffffff06); box-shadow:inset 0 1px #ffffff0a; }
.app-shell .field-label input:focus { border-color:#69d78b85; box-shadow:0 0 0 3px #69d78b20,inset 0 1px #ffffff0a; }
.app-shell .secondary-button:hover,.app-shell .wallpaper-actions button:hover { border-color:#ffffff38; background:#ffffff14; }
.app-shell .connection-modal .primary-button,.app-shell .modal-submit { color:#07140b; border:1px solid #8bffad80; background:linear-gradient(180deg,#9dffb5,#61da85); box-shadow:inset 0 1px #ffffffa6,0 5px 14px #00a63d35; }
.app-shell .connection-modal .primary-button:hover,.app-shell .modal-submit:hover { color:#041008; background:linear-gradient(180deg,#b4ffc5,#7be99a); box-shadow:inset 0 1px #ffffffc7,0 7px 18px #00a63d45; }
.app-shell .modal-close { border-color:#ffffff20; background:linear-gradient(180deg,#ffffff12,#ffffff07); box-shadow:inset 0 1px #ffffff0c; }
.app-shell .modal-close:hover { border-color:#ffffff38; background:#ffffff18; }
.about-update { display:flex; flex-direction:column; align-items:center; gap:9px; margin:15px 0; }
.about-update .secondary-button,.about-update .primary-button { min-height:34px; }
.update-message,.update-error { max-width:280px; margin:0; font-size:10px; line-height:1.6; overflow-wrap:anywhere; }
.update-message { color:var(--text-soft); }
.update-error { color:#ffaaa3; }
.update-toast {
  position:fixed;
  z-index:4;
  top:58px;
  right:18px;
  width:min(360px,calc(100vw - 28px));
  display:grid;
  grid-template-columns:34px minmax(0,1fr) 24px;
  align-items:start;
  gap:11px;
  padding:13px;
  border:1px solid var(--border);
  border-radius:14px;
  color:var(--text);
  background:var(--sidebar);
  box-shadow:0 18px 48px #0007,inset 0 1px #ffffff18;
  -webkit-backdrop-filter:blur(24px) saturate(145%);
  backdrop-filter:blur(24px) saturate(145%);
}
.update-toast-icon { width:34px; height:34px; display:grid; place-items:center; border-radius:10px; color:var(--accent); background:var(--surface-muted); }
.update-toast-copy { min-width:0; display:flex; flex-direction:column; gap:4px; padding-top:1px; }
.update-toast-copy strong { color:var(--text); font-size:11px; font-weight:600; }
.update-toast-copy span { color:var(--text-soft); font-size:10px; }
.update-toast-action { align-self:flex-start; margin:3px 0 0; padding:0; border:0; color:var(--accent); background:transparent; font-size:10px; font-weight:600; }
.update-toast-action:hover { text-decoration:underline; }
.update-toast-close { width:24px; height:24px; display:grid; place-items:center; padding:0; border:0; border-radius:7px; color:var(--text-faint); background:transparent; }
.update-toast-close:hover { color:var(--text); background:var(--surface-muted); }
.app-shell.theme-macos-glass {
  --canvas:#111722;
  --sidebar:#202a38e8;
  --surface:#2a3545d9;
  --surface-muted:#46546a70;
  --text:#eaf1fa;
  --text-soft:#c2ccda;
  --text-faint:#92a0b2;
  --border:#dce8f226;
  --accent:#a9c8ff;
  --accent-bright:#d1e2ff;
  background:
    radial-gradient(ellipse at 17% 3%,#779bd52b,transparent 42%),
    radial-gradient(ellipse at 86% 96%,#a481db22,transparent 40%),
    linear-gradient(135deg,#171e2a,#10151e 62%,#1b202b);
}
.theme-macos-glass .main-panel {
  background:
    radial-gradient(ellipse at 20% 12%,#799bd31c,transparent 40%),
    radial-gradient(ellipse at 82% 86%,#a481db16,transparent 38%),
    linear-gradient(135deg,#151c27,#10151e 62%,#191e29);
}
.theme-macos-glass .session-strip {
  border-color:#dce8f21c;
  background:linear-gradient(180deg,#313d50d9,#18212cdb);
}
.theme-macos-glass .terminal-stage {
  margin:14px 16px 10px;
  border-color:#dce8f229;
  border-radius:16px;
  background:linear-gradient(145deg,#202a39,#141b26);
  box-shadow:0 12px 36px #05091280,inset 0 1px #ffffff18;
  -webkit-backdrop-filter:blur(26px) saturate(155%);
  backdrop-filter:blur(26px) saturate(155%);
}
.theme-macos-glass .session-tab-active,.theme-macos-glass .app-titlebar .session-tab-active {
  color:#f3f7ff;
  border-color:#9bbcff65;
  background:linear-gradient(180deg,#627da34d,#4053714d);
  box-shadow:inset 0 1px #e3edff24,0 3px 12px #6699f326;
}
.theme-macos-glass .session-tab-active:hover,.theme-macos-glass .app-titlebar .session-tab-active:hover {
  border-color:#b5ceff85;
  background:linear-gradient(180deg,#718db65c,#4c61805c);
}
.theme-macos-glass .welcome-stage {
  background:
    radial-gradient(ellipse at 22% 12%,#779bd52b,transparent 40%),
    radial-gradient(ellipse at 78% 88%,#a481db24,transparent 42%),
    linear-gradient(135deg,#171e2a,#10151e 62%,#1b202b);
}
.theme-macos-glass .welcome-content h2 { color:#e7eef9; }
.theme-macos-glass .welcome-content h2 em { color:#a9c8ff; }
.theme-macos-glass .welcome-content p,.theme-macos-glass .welcome-footnote { color:#adbacb; }
.theme-macos-glass .welcome-terminal {
  border-color:#a9c8ff6b;
  color:#edf4ff;
  background:linear-gradient(145deg,#667fa966,#35445b99);
  box-shadow:0 9px 24px #060a1380,inset 0 1px #ffffff42;
  -webkit-backdrop-filter:blur(16px);
  backdrop-filter:blur(16px);
}
.theme-macos-glass .welcome-terminal i { background:#c7dcff; }
.theme-macos-glass .update-toast {
  border-color:#dce8f240;
  background:linear-gradient(145deg,#283547ed,#182130ed);
}
.theme-macos-glass .connection-modal .primary-button,.theme-macos-glass .modal-submit {
  color:#111722;
  border-color:#d2e2ff8c;
  background:linear-gradient(180deg,#d9e7ff,#9dbdf4);
  box-shadow:inset 0 1px #ffffffb3,0 5px 14px #608ed63b;
}
.theme-macos-glass .connection-modal .primary-button:hover,.theme-macos-glass .modal-submit:hover {
  background:linear-gradient(180deg,#e7efff,#b2ccfa);
}
@media (prefers-reduced-motion: reduce) {
  .session-tab,.new-tab-button,.app-titlebar .new-tab-button { transition:none; }
}

</style>
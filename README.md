# Apex Term

一个轻量、无需账号、开箱即用的桌面 SSH 终端：把本机 Shell、SSH 会话管理和 SFTP 文件传输装进一个小工具里，够用就好。

基于 Tauri 2、Vue 3 和 Rust 构建，支持 macOS、Windows 和 Linux。

## 为什么是 Apex Term

常见的终端功能越来越多，但很多对日常使用并无帮助；功能齐全的远程工具又往往要求注册账号、同步云端、体积臃肿。Apex Term 刻意只做三件事：**本地终端、SSH 连接、SFTP 文件管理**，并把它们做到顺手、安全、不打扰。如果它刚好适合你，那就最好不过了。

- **无需账号**：不注册、不登录、不云同步，所有配置留在你的设备上。
- **凭据安全**：SSH 密码、私钥和口令存系统凭据管理器，不写进主机资料；主机指纹首次确认、变更即拒绝。
- **轻量直接**：单个安装包，启动即进本机 Shell，连接 SSH 新建页签而非替换当前终端。
- **刻意保持精简**：不打算堆叠功能——这是设计选择，而不是未完成。

### 适合谁

- 只想快速连上服务器、传个文件，不想为终端配置花时间的人
- 偏好本地存储、不希望凭据上云的人
- 需要 macOS / Windows / Linux 跨平台、行为一致的 SSH 客户端的人
- 在找 Termius 的"免账号、轻量"替代品的人

### 不适合谁

- 需要多机同步会话、团队共享、跳板机/隧道等企业级功能 → 更适合 Termius、Tabby
- 追求极致终端模拟体验、重度插件生态 → 更适合 iTerm2、WezTerm、Kitty
- 需要 SFTP 之外的传输协议（如 S3、WebDAV）→ 更适合专用文件工具

## 功能

- SSH 密码认证与私钥认证（粘贴 PEM/OpenSSH 私钥，可选口令）及交互式 xterm.js 终端
- SSH 连接会逐个尝试 DNS 解析出的多个地址（如 IPv6/IPv4 双栈），提升弱网络下的连接成功率
- SFTP 复用已连接 SSH 会话的认证连接，无需每次操作重新握手认证；服务器未开启 SFTP 子系统时不影响 SSH 终端
- SFTP 采用分块传输，单个文件无大小限制
- 启动时默认打开本机系统 Shell
- 首次连接确认服务器指纹，已信任主机指纹变化时拒绝连接
- 应用菜单（macOS、Windows 下拉菜单；Linux 系统菜单）：本机终端、外观、会话管理、SFTP 和关于；外观菜单可分别设置应用外观、终端主题与字体
- 多终端页签：本机 Shell 默认启动，添加 SSH 主机会新建页签，不会替换现有终端；可在会话列表切换本机终端
- 终端支持 Ctrl/Command+C 复制选中文本、Ctrl/Command+V 粘贴；未选中文本时 Ctrl+C 仍发送中断信号
- 使用 Command+R（macOS）或 Ctrl+R（Linux）重新连接/启动当前会话
- SSH 主机连接测试，首次连接前核对服务器指纹
- 系统原生 SFTP 菜单：连接 SSH 主机后可从系统菜单打开文件管理
- SFTP 远程文件浏览、上传、下载、新建目录和删除，传输过程显示进度
- 应用外观支持浅色、深色和跟随系统；终端区域主题（Homebrew、macOS 玻璃、Dracula、Nord、Solarized Dark、Monokai、午夜、浅色）独立于应用外观，并可设置字体和字号
- 应用启动时自动检查 GitHub 最新正式版，有更新时可在关于窗口直接下载适用于当前系统的安装包到下载目录，并显示下载进度；下载完成后需手动运行安装包
- 应用外观默认深色，桌面应用启动后全屏进入本机 Shell
- macOS 默认使用 MesloLGS NF 字体并启用 256 色/真彩色终端环境，兼容 Powerlevel10k 提示符
- 自适应终端尺寸
- 终端优先使用 WebGL GPU 渲染；不支持 GPU 或 WebGL 上下文丢失时自动回退到 CPU/DOM 渲染

启动应用后默认打开本机 Shell。主机资料、应用外观、终端主题和字体设置保存在当前 WebView 的本地存储中；SSH 密码、私钥及私钥口令保存在系统凭据管理器，不写入主机资料。SFTP 需要在已连接的 SSH 终端页签上使用。

SSH 优先协商现代算法，并兼容服务端支持的旧算法。浏览器预览仅用于检查界面；本机 Shell 和 SSH 连接需要在 Tauri 桌面应用中运行。本机 Shell 从用户主目录启动。

## 开发

```sh
npm install
npm run tauri dev
```

## 发布

GitHub 是唯一代码仓库。日常开发在 `dev` 分支进行，通过 PR 合入受保护的 `main`。每个指向 `main` 的 PR 都会构建 macOS、Windows x64 和 Linux x64 安装包用于验证，不会创建 Release。合并 PR 后，单独推送 `v*` tag 才会构建并创建 GitHub Release；产物包括 macOS DMG、Windows MSI/NSIS，以及 Linux AppImage/DEB/RPM。

首个版本基准为 `0.1.0`。PR 中无需手动更新版本文件：正式构建时会从 tag 设置应用版本。

PR 合并后，在本地更新 `main` 并运行发布命令。第一次不带参数会创建 `v0.1.0`；之后默认自动升 patch（如 `v0.1.0` → `v0.1.1`）：

```shell
git switch main
git pull --ff-only origin main
npm run release
```

需要发布 minor 或 major 版本时，可运行 `npm run release -- minor` 或 `npm run release -- major`。发布命令只检查 `main` 与 GitHub `origin/main` 完全一致、工作区干净且 tag 尚不存在，然后创建并推送 tag；它不会修改或推送 `main`。推送 tag 后会触发三平台正式构建和 GitHub Release。确保 GitHub 仓库的 `main` 分支保护规则要求 PR，并将 PR 工作流的三个构建检查设为必需状态检查。

## 安装

从 Releases 下载对应平台的安装包即可。应用使用 macOS ad-hoc 签名，不需要 Apple Developer ID 证书。ad-hoc 签名不等于 Developer ID 签名或 Apple 公证；从浏览器下载并安装后，如果 Gatekeeper 仍阻止打开，可在系统终端执行以下命令移除隔离属性，然后重新打开应用。

```shell
sudo xattr -dr com.apple.quarantine /Applications/ApexTerm.app
```

## 许可证

[MIT](LICENSE) © 2026 zxkbazinga

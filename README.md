
# Apex Term

基于 Tauri 2、Vue 3 和 Rust 的桌面 SSH 客户端原型，目前仅适用于macOS Apple芯片。

## 当前功能

- SSH 密码认证及交互式 xterm.js 终端
- 启动时默认打开本机系统 Shell（macOS）
- 首次连接确认服务器指纹，已信任主机指纹变化时拒绝连接
- 系统原生菜单：外观、会话管理、SFTP 和关于；外观菜单可打开主题与字体设置
- 多终端页签：本机 Shell 默认启动，添加 SSH 主机会新建页签，不会替换现有终端；可在会话列表切换本机终端
- 终端支持 Ctrl/Command+C 复制选中文本、Ctrl/Command+V 粘贴；未选中文本时 Ctrl+C 仍发送中断信号
- 使用 Command+R（macOS）或 Ctrl+R（Linux）重新连接/启动当前会话
- SSH 主机连接测试，首次连接前核对服务器指纹
- 系统原生 SFTP 菜单：连接 SSH 主机后可从系统菜单打开文件管理
- SFTP 远程文件浏览、上传、下载、新建目录和删除
- 多种终端主题（Homebrew、Dracula、Nord、Solarized Dark、Monokai、午夜、浅色），可设置字体和字号
- 默认黑色界面，桌面应用启动后全屏进入本机 Shell
- macOS 默认使用 MesloLGS NF 字体并启用 256 色/真彩色终端环境，兼容 Powerlevel10k 提示符
- 自适应终端尺寸

启动应用后默认打开本机 Shell。主机资料和外观设置保存在当前 WebView 的本地存储中；SSH 密码保存在系统凭据管理器
SSH 优先协商现代算法，并兼容服务端支持的旧算法。SFTP 单个文件最大支持 32 MB。
浏览器预览仅用于检查界面；本机 Shell 和 SSH 连接需要在 Tauri 桌面应用中运行。本机 Shell 从用户主目录启动。

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

应用使用 macOS ad-hoc 签名，不需要 Apple Developer ID 证书。ad-hoc 签名不等于 Developer ID 签名或 Apple 公证；从浏览器下载并安装后，如果 Gatekeeper 仍阻止打开，可在系统终端执行以下命令移除隔离属性，然后重新打开应用。

```shell
sudo xattr -dr com.apple.quarantine /Applications/ApexTerm.app
```

## 诞生原因

 现在常见的终端功能越来越多，但是对于部分人来说，其实很多功能都用不上。我尝试用很多终端，但都很复杂不太符合我的需求，所以我就借助AI帮忙生成了它。它没有账号信息同步、没有AI等功能，只有本地终端、SSH会话管理和SFTP等功能，后续也不打算开发更多功能，够用就行。
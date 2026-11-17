# PhotoSys

PhotoSys 是一个使用 Tauri 2、Vue 3、TypeScript 和 Rust 的桌面应用，用于复制和按月归档照片与视频。

## 归档照片与视频

打开 PhotoSys，选择源目录和目标目录，点击「开始复制」。应用递归扫描源目录，按文件名里的日期或媒体内嵌拍摄时间，将原文件复制到目标目录的 `yyyy/yyyy-MM/` 下。没有可用日期的媒体进入 `其它/`。目标目录中同名且内容相同的文件会跳过，同名但内容不同的文件会以 `名称_1.扩展名` 等名称保存。

每次任务结束后，目标目录根目录会生成一份 `复制说明_yyyyMMdd_HHmmss.txt`。界面会显示本次统计和说明文件路径。

## 开发环境

安装 Node.js 24 LTS 和 npm，再通过 [rustup](https://rustup.rs/) 安装 Rust stable。安装后重新打开终端，确认 `node --version`、`npm --version` 和 `rustc --version` 可用。

**Windows** 安装 Microsoft Edge WebView2 Runtime 和 Visual Studio Build Tools 的「使用 C++ 的桌面开发」工作负载，包含 MSVC x64/x86 工具和 Windows SDK。用 `rustup default stable-msvc` 选择 Rust 工具链，安装后重新打开终端。

**Debian 13 / Ubuntu 24.04** 在宿主机安装 Tauri 开发和打包依赖。

```sh
sudo apt-get update
sudo apt-get install -y build-essential ca-certificates curl wget file pkg-config \
  libwebkit2gtk-4.1-dev libxdo-dev libssl-dev libayatana-appindicator3-dev \
  librsvg2-dev patchelf xdg-utils
```

**macOS** 运行 `xcode-select --install` 安装命令行开发工具。若要构建供 Apple Silicon 和 Intel 共用的安装包，安装两个 Rust 目标。

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

各系统完整安装步骤见 [Tauri 平台依赖](https://v2.tauri.app/start/prerequisites/)。

## 运行和打包

在项目根目录执行以下命令。`tauri dev` 启动 Vite 和桌面窗口，关闭开发进程后可以执行打包。

```sh
npm ci
npm run tauri dev
npm run tauri build -- -- --locked
```

单独检查前端类型和生产构建，以及 Rust 测试。

```sh
npm run build
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

在 macOS 上用 `npm run tauri build -- --target universal-apple-darwin -- --locked` 构建通用包。本机打包产物位于 `src-tauri/target/release/bundle/`，指定 `--target` 后位于 `src-tauri/target/<target>/release/bundle/`。

## GitHub Actions

将项目推送到启用了 Actions 的 GitHub 仓库。推送到 `main`、打开或更新 PR 时，三个平台会并行运行测试并打包。工作流进入默认分支后，也可以在 **Actions → Desktop builds → Run workflow** 手动触发。在运行详情页的 **Artifacts** 下载对应安装包。

| Artifact | 构建环境 | 文件 |
| --- | --- | --- |
| `PhotoSys-windows-x64` | Windows Server 2025 x64 | 未签名的 Windows x64 NSIS `.exe` |
| `PhotoSys-linux-x64` | Ubuntu 24.04 x64 | Linux x64 `.deb` 和 `.AppImage` |
| `PhotoSys-macos-universal` | macOS 15 arm64 | 通用 `.dmg`，包含 `arm64` 和 `x86_64`，采用 ad-hoc 签名 |

安装后打开 PhotoSys，用临时目录试复制几张照片，检查归档子目录和复制说明，再用相同目录运行一次并核对重复跳过数量。Windows 和 macOS 的内部测试包可能触发 SmartScreen 或 Gatekeeper 提示。

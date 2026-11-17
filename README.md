# PhotoSys

PhotoSys 是一个使用 Tauri 2、Vue 3、TypeScript 和 Rust 的桌面应用，用于浏览、收藏、查重和归档本地照片与视频。

## 便携媒体库

将 Linux x64 的 `PhotoSys.AppImage` 放到相册根目录。在文件属性中授予执行权限，或运行 `chmod +x PhotoSys.AppImage`，然后双击。启动时，PhotoSys 优先打开 AppImage 同目录中已有的媒体库，然后尝试恢复上次手动选定的目录。两处都没有可用媒体库时，在「媒体库」中点击「选择目录」。在外部添加、删除或改名文件后，点击「刷新」更新列表。

如果 Linux 环境无法挂载 AppImage，可在相册目录运行 `APPIMAGE_EXTRACT_AND_RUN=1 ./PhotoSys.AppImage`。

媒体库按拍摄日期从新到旧排列，支持按类型、日期范围、日期未知和收藏筛选。可以在应用内预览 JPG/JPEG、PNG、WebP、TIFF、HEIC/HEIF 和 `.livp` 封面。支持解码的 MOV、MP4、M4V、3GP、MKV 和 WebM 视频会显示首帧缩略图，点击可查看大图。照片和视频都可以用系统默认应用打开。在 `.livp` 详情中点击「播放实况」，可在应用内播放包内的 MOV 或 MP4。从媒体库切换到归档、查重或相似照片时，会带入当前目录。

手动选择目录时，PhotoSys 会读取其中已有的 `.photosys/library.sqlite`，或在数据库缺失时创建并初始化它。初始化失败时可查看错误并重选目录。已有有效数据库的只读目录可以浏览，初始化和保存收藏需要写入权限。

搬家时先关闭 PhotoSys，再复制整个相册目录，包括隐藏目录 `.photosys` 和随相册存放的 AppImage。直接启动搬迁后的 AppImage，或在安装版中选择搬迁后的目录。收藏按相对路径保存，整个相册换位置后仍可使用。单独移动或重命名媒体文件后，收藏仍对应原相对路径。

## 归档照片与视频

打开 PhotoSys，选择源目录和目标目录，点击「开始复制」。应用递归扫描源目录，按文件名里的日期或媒体内嵌拍摄时间，将原文件复制到目标目录的 `yyyy/yyyy-MM/` 下。没有可用日期的媒体进入 `其它/`。目标目录中同名且内容相同的文件会跳过，同名但内容不同的文件会以 `名称_1.扩展名` 等名称保存。

每次任务结束后，目标目录根目录会生成一份 `复制说明_yyyyMMdd_HHmmss.txt`。界面会显示本次统计和说明文件路径。

## 查找相似照片

打开「相似照片」，选择目录后开始扫描。应用递归查找 JPG/JPEG、PNG、WebP、TIFF 和 HEIC/HEIF，可按「同图变体」或「疑似连拍」筛选结果。展开相似组后，可任选两张照片并排查看预览和文件信息。照片仅供查看。

同图变体包括压缩、缩放、格式转换和轻微调色后的版本。疑似连拍还会检查 EXIF 拍摄时间，要求相差不超过 5 秒。复扫会使用本地 SQLite 索引复用未变化照片的图像特征。暂未支持的照片格式和读图失败会列在「问题明细」中。

## 开发环境

安装 Node.js 24 LTS 和 npm，再通过 [rustup](https://rustup.rs/) 安装 Rust stable。安装后重新打开终端，确认 `node --version`、`npm --version` 和 `rustc --version` 可用。

**Windows** 安装 Microsoft Edge WebView2 Runtime 和 Visual Studio Build Tools 的「使用 C++ 的桌面开发」工作负载，包含 MSVC x64/x86 工具和 Windows SDK。用 `rustup default stable-msvc` 选择 Rust 工具链，安装后重新打开终端。

**Debian 13 / Ubuntu 24.04** 在宿主机安装 Tauri 开发和打包依赖。

```sh
sudo apt-get update
sudo apt-get install -y build-essential ca-certificates curl wget file pkg-config \
  libwebkit2gtk-4.1-dev libxdo-dev libssl-dev libayatana-appindicator3-dev \
  librsvg2-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  nasm patchelf xdg-utils
```

**macOS** 运行 `xcode-select --install` 安装命令行开发工具。若要构建供 Apple Silicon 和 Intel 共用的安装包，安装两个 Rust 目标。

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

各系统完整安装步骤见 [Tauri 平台依赖](https://v2.tauri.app/start/prerequisites/)。

## 运行和打包

首次构建、开发或运行 Rust 测试前，在项目根目录执行 `bash scripts/build-media-codec.sh`，生成随包提供的 FFmpeg。脚本会校验固定版本的源码并复用构建产物。macOS 先安装 `brew install pkgconf nasm`。Windows 在 MSYS2 的 UCRT64 终端安装下列工具，并在该终端运行脚本。

```sh
pacman -S --needed curl diffutils make mingw-w64-ucrt-x86_64-gcc \
  mingw-w64-ucrt-x86_64-nasm mingw-w64-ucrt-x86_64-pkgconf
```

准备好 FFmpeg 后，在项目根目录执行以下命令。`tauri dev` 启动 Vite 和桌面窗口，关闭开发进程后可以执行打包。

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

在 macOS 上先运行 `bash scripts/build-media-codec.sh universal-apple-darwin`，再用 `npm run tauri build -- --target universal-apple-darwin -- --locked` 构建通用包。本机打包产物位于 `src-tauri/target/release/bundle/`，指定 `--target` 后位于 `src-tauri/target/<target>/release/bundle/`。

## GitHub Actions

将项目推送到启用了 Actions 的 GitHub 仓库。推送到 `main`、打开或更新 PR 时，三个平台会并行运行测试并打包。工作流进入默认分支后，也可以在 **Actions → Desktop builds → Run workflow** 手动触发。在运行详情页的 **Artifacts** 下载对应安装包。

| Artifact | 构建环境 | 文件 |
| --- | --- | --- |
| `PhotoSys-windows-x64` | Windows Server 2025 x64 | 未签名的 Windows x64 NSIS `.exe` |
| `PhotoSys-linux-x64` | Ubuntu 24.04 x64 | Linux x64 `.deb` 和 `.AppImage` |
| `PhotoSys-macos-universal` | macOS 15 arm64 | 通用 `.dmg`，包含 `arm64` 和 `x86_64`，采用 ad-hoc 签名 |

安装后打开 PhotoSys，用临时目录试复制几张照片，检查归档子目录和复制说明，再用相同目录运行一次并核对重复跳过数量。Windows 和 macOS 的内部测试包可能触发 SmartScreen 或 Gatekeeper 提示。

各构建产物附带 `media-codec-sources.tar.gz`，其中包含媒体解码器的对应源码和构建脚本。安装包中包含许可文本。详见 [媒体解码器许可](scripts/MEDIA-CODEC-NOTICES.md)。

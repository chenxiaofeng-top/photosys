# PhotoSys 三平台打包教程

这份教程教你用 GitHub Actions 打出 Windows、Linux、macOS 安装包，并通过版本标签发布到 GitHub Releases。日常云端打包只需要浏览器和有写权限的 GitHub 账号，无需在自己电脑上安装三套操作系统或构建环境。

## 1. 第一次使用前准备什么

1. 将项目放到自己的 GitHub 仓库。当前仓库是 [chenxiaofeng-top/photosys](https://github.com/chenxiaofeng-top/photosys)。如果使用 Fork，请把下文网址和命令中的仓库名换成自己的。
2. 确保 `.github/workflows/desktop-builds.yml` 和它使用的脚本已经提交，并合入仓库默认分支（本项目为 `main`）。手动运行按钮要求默认分支中存在支持 `workflow_dispatch` 的工作流。
3. 打开仓库的 **Actions** 页面；如果 GitHub 提示工作流尚未启用，点击启用。
4. 如果仓库或组织限制了 Actions，在 **Settings → Actions → General** 中允许工作流使用的 Actions，并允许 Release 作业获得 `contents: write` 权限。普通构建只读取仓库。

发布使用 GitHub 自动提供的 `GITHUB_TOKEN`，不需要新建 PAT，也不需要配置签名证书 Secrets。Windows 包未签名；macOS 使用 ad-hoc 签名，没有 Apple 公证。

## 2. 手动打一次三平台包

1. 打开 **Actions → Desktop builds**。当前仓库的入口是 [Desktop builds](https://github.com/chenxiaofeng-top/photosys/actions/workflows/desktop-builds.yml)。
2. 点击 **Run workflow**，在 **Branch** 中选择要打包的分支，通常选择 `main`。
3. 日常打包保持 `full_ai_validation` 不勾选。需要验证所有 AI 模型时再勾选；它会在三个平台分别下载五套模型，运行真实推理、离线安装和模型切换测试，耗时和流量明显增加。
4. 点击 **Run workflow**，刷新运行列表，进入刚创建的运行。
5. 等待配置校验、三个平台构建和 Intel macOS 运行时检查完成。构建任务可能排队；FFmpeg 编译和 Rust 首次编译也需要时间，以实际运行结果为准。
6. 在运行详情页下方 **Artifacts** 下载需要的平台包。每个平台成功后，摘要也会显示它的下载链接；Artifacts 保留 30 天。

默认打包仍会执行常规 Rust 测试、前端类型检查和生产构建，检查包内 FFmpeg、ONNX Runtime，以及 macOS 通用架构。**关闭完整 AI 验证不会删除应用的 AI 功能。** 安装包包含 CPU 运行时，模型权重由用户在应用中另行安装。

手动运行只生成 Artifacts。即使手动选择了版本标签，也不会发布 Release；自动发布要求通过 `git push` 推送版本标签触发。

也可以使用已登录的 [GitHub CLI](https://cli.github.com/)：

```sh
gh workflow run desktop-builds.yml --repo chenxiaofeng-top/photosys --ref main
# 如需完整 AI 验证，改用这一条：
gh workflow run desktop-builds.yml --repo chenxiaofeng-top/photosys --ref main -f full_ai_validation=true
gh run list --repo chenxiaofeng-top/photosys --workflow desktop-builds.yml
```

记下实际运行 ID，用它查看进度、下载对应产物：

```sh
gh run watch RUN_ID --repo chenxiaofeng-top/photosys
gh run download RUN_ID --repo chenxiaofeng-top/photosys --dir downloads
```

## 3. 下载后选择哪个文件

Artifacts 下载得到 ZIP，先解压，再按扩展名找到安装包。ZIP 中可能保留 `target/`、`binaries/` 等目录层级，可以递归查找。

| Artifact 名称 | 适用系统 | 安装文件 |
| --- | --- | --- |
| `PhotoSys-windows-x64` | Windows x64 | NSIS `.exe` 安装程序 |
| `PhotoSys-linux-x64` | Linux x64 | Debian/Ubuntu 的 `.deb`，以及便携 `.AppImage` |
| `PhotoSys-macos-universal` | Apple Silicon、Intel Mac | 通用 `.dmg` |

**Windows：** 双击 `.exe`，按安装向导操作。未签名程序可能出现 SmartScreen 提示；确认来源和版本后按系统提供的选项处理。应用依赖 Microsoft Edge WebView2 Runtime，缺失时安装程序可能需要联网安装它。

**Linux deb：** 在安装包目录执行，文件名替换成实际文件名：

```sh
sudo apt install ./PhotoSys_0.1.1_amd64.deb
```

**Linux AppImage：** 文件名同样替换成实际文件名：

```sh
chmod +x ./PhotoSys_0.1.1_amd64.AppImage
./PhotoSys_0.1.1_amd64.AppImage
# 无法挂载 AppImage 时：
APPIMAGE_EXTRACT_AND_RUN=1 ./PhotoSys_0.1.1_amd64.AppImage
```

**macOS：** 打开 DMG，将 PhotoSys 拖入 Applications 后启动。ad-hoc 签名不能消除 Gatekeeper 提示；确认来源后，按系统提示在「系统设置 → 隐私与安全性」中允许打开。

每个平台的 Artifact 还附带 `media-codec-sources.tar.gz`，里面是 FFmpeg 等媒体解码器的源码和构建脚本，供许可要求及复现构建使用，**不需要解压安装**。安装包已包含使用所需的解码器和许可文本，详见 [媒体解码器许可](../scripts/MEDIA-CODEC-NOTICES.md)。

安装后建议使用临时相册检查：应用能打开、照片和视频能预览、几张照片能归档复制、重复复制能跳过。需要测试 AI 功能时，先在应用中安装模型，再检查断网使用。

## 4. 发布一个新版本（以 0.1.1 为例）

普通推送到 `main` 和 PR 会构建 Artifacts；**推送 `v0.1.1` 这样的标签才会自动发布 Release**。只接受 `v主版本.次版本.修订号`，不接受 `v0.1`、`v01.1.1` 或 `v0.1.1-beta.1`。

下面在已经包含打包工作流的本地项目根目录操作，需要 Git、Node.js/npm、Python 3.11+ 和 Rust。发布时请使用实际的新版本号；项目当前版本不会由工作流自动增加。

### 第一步：同步项目并修改版本

确保本地修改已经提交或妥善保存，然后同步 `main`：

```sh
git switch main
git pull --ff-only origin main
npm version 0.1.1 --no-git-tag-version
```

上面的 npm 命令更新 `package.json` 和 `package-lock.json` 中的项目版本，不创建 Git 标签。随后手动编辑：

- `src-tauri/tauri.conf.json`：顶层 `version` 改为 `0.1.1`。
- `src-tauri/Cargo.toml`：`[package]` 下的 `version` 改为 `0.1.1`。

更新 Rust 锁文件，再运行与 CI 相同的版本检查：

```sh
cargo update --workspace --manifest-path src-tauri/Cargo.toml
python3 scripts/check-release-version.py v0.1.1
git diff
```

在 Windows PowerShell 中，Python 命令通常写成 `python`。检查器会核对前端、Tauri、Rust，以及 npm 锁文件的两个根版本和 Cargo 锁文件中的应用版本。它只检查，不替你修改版本。查看 diff，确认变更符合本次发布的预期。

### 第二步：提交并推送版本修改

```sh
git add package.json package-lock.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "chore: release 0.1.1"
git push origin main
```

如果仓库保护 `main`，通过 PR 合并版本修改，再同步本地 `main`。等待该提交的常规构建成功。需要发布前完整 AI 验证时，对这个分支手动运行并勾选 `full_ai_validation`，等待全部成功后再打标签；验证后不要混入未验证的功能修改。

### 第三步：给待发布提交打标签并推送

```sh
git tag -a v0.1.1 -m "PhotoSys 0.1.1"
git push origin v0.1.1
```

这会自动触发一次标签构建。标签所指提交必须包含版本修改和工作流。标签与任意项目版本不一致时，会在编译前失败。

### 第四步：等待并下载 Release

进入标签触发的 Actions 运行。三个平台及 Intel macOS 运行时检查全部成功后，`Publish GitHub Release` 会执行：

1. 下载本次运行的三个 Artifact，核对四个安装包和 Linux 构建附带的源码归档均存在且非空。
2. 生成 `SHA256SUMS`，先将所有附件上传至同一标签的草稿 Release。
3. 检查上传完成状态、文件名和大小，附件齐全后自动公开发布。

打开仓库首页右侧 **Releases**，或当前仓库的 [Releases 页面](https://github.com/chenxiaofeng-top/photosys/releases)。选择 `v0.1.1`，展开 **Assets**，直接下载安装包，无需下载 Actions ZIP。发布作业的运行摘要也会给出 Release 链接。

预期有六个上传附件：`.exe`、`.deb`、`.AppImage`、`.dmg`、`media-codec-sources.tar.gz`、`SHA256SUMS`。GitHub 另外生成的 “Source code (zip/tar.gz)” 是应用仓库源码，不能代替媒体解码器源码归档。Release 附件不受 Actions 的 30 天保留设置影响。

需要校验时，把五个附件和 `SHA256SUMS` 放在同一目录：

```sh
# Linux
sha256sum -c SHA256SUMS
# macOS
shasum -a 256 -c SHA256SUMS
```

Windows PowerShell 可用 `Get-FileHash .\实际安装包.exe -Algorithm SHA256`，与 `SHA256SUMS` 对应行对照。只下载一个安装包时，也可只核对该文件的哈希；上面的批量校验命令要求五个文件都在。

如果上传或附件检查失败，Release 留在草稿状态；重跑失败作业会更新同一草稿。**已公开版本不会被工作流覆盖**，重复执行会保留原附件。修订已发布内容请使用 `0.1.2` 等新版本和新标签。

## 5. 本地打包备用流程

本地打包需要在对应系统执行：Windows 上打 Windows 包，Linux 上打 Linux 包，macOS 上打 macOS 包。这里只在 macOS 内组合 arm64 与 x86_64，不通过一台 Linux 电脑交叉生成三平台安装包。

### 共同依赖

- Node.js 24、npm、Rust stable（通过 [rustup](https://rustup.rs/) 安装）。
- Python 3.11+，用于构建时准备原生 CPU 运行时；最终用户无需 Python。
- 从网络下载 npm/Rust 依赖、固定版本的媒体解码器源码和 CPU 运行时。

完整平台依赖可参考 [Tauri 官方说明](https://v2.tauri.app/start/prerequisites/)。以下命令均在项目根目录执行。

### Linux（Ubuntu 24.04 / Debian 13，x64）

```sh
sudo apt-get update
sudo apt-get install -y build-essential ca-certificates curl wget file pkg-config \
  libwebkit2gtk-4.1-dev libxdo-dev libssl-dev libayatana-appindicator3-dev \
  librsvg2-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  nasm patchelf xdg-utils
bash scripts/build-media-codec.sh
python3 scripts/prepare-ai-runtime.py
python3 scripts/check-ai-runtime.py src-tauri/binaries/onnxruntime
npm ci
cargo test --locked --manifest-path src-tauri/Cargo.toml
APPIMAGE_EXTRACT_AND_RUN=1 npm run tauri build -- -- --locked
```

产物位于 `src-tauri/target/release/bundle/deb/` 和 `src-tauri/target/release/bundle/appimage/`。

### Windows x64

1. 安装 Visual Studio Build Tools 的「使用 C++ 的桌面开发」，包含 MSVC x64/x86 工具及 Windows SDK；安装 Microsoft Edge WebView2 Runtime。
2. 安装 [MSYS2](https://www.msys2.org/)，打开 **MSYS2 UCRT64** 终端，进入项目目录（例如 `cd /c/code/photosys`），运行：

```sh
pacman -S --needed curl diffutils make perl mingw-w64-ucrt-x86_64-gcc \
  mingw-w64-ucrt-x86_64-nasm mingw-w64-ucrt-x86_64-pkgconf
bash scripts/build-media-codec.sh
```

3. 在能找到 Node.js、Python 和 Rust 的 **PowerShell** 中进入项目根目录，运行：

```powershell
rustup default stable-msvc
python scripts/prepare-ai-runtime.py
python scripts/check-ai-runtime.py src-tauri/binaries/onnxruntime
npm ci
cargo test --locked --manifest-path src-tauri/Cargo.toml
npm run tauri build -- -- --locked
```

产物位于 `src-tauri/target/release/bundle/nsis/`。FFmpeg 脚本内部使用 UCRT64 编译器；桌面应用使用 MSVC Rust 工具链。安装依赖后若命令不可用，重新打开对应终端。

### macOS 通用包

首次运行 `xcode-select --install`，按提示安装命令行开发工具。安装 [Homebrew](https://brew.sh/) 后，在终端执行：

```sh
brew install pkgconf nasm
rustup target add aarch64-apple-darwin x86_64-apple-darwin
bash scripts/build-media-codec.sh universal-apple-darwin
python3 scripts/prepare-ai-runtime.py --universal
python3 scripts/check-ai-runtime.py src-tauri/binaries/onnxruntime
npm ci
export TMPDIR="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
cargo test --locked --manifest-path src-tauri/Cargo.toml
npm run tauri build -- --target universal-apple-darwin -- --locked
```

产物位于 `src-tauri/target/universal-apple-darwin/release/bundle/dmg/`，同级 `macos/` 中有 `.app`。`TMPDIR` 命令将 macOS 临时目录转换为实际路径，避免系统的 `/var` 符号链接触发 SQLite 的防链接检查。准备运行时的检查只加载本机架构；CI 另用 Intel Mac 验证 Intel 运行时，并检查通用应用及 FFmpeg 的两个架构。

本地打包不会自动上传 GitHub，也不会生成 Release。源码归档位于 `src-tauri/binaries/media-codec-sources.tar.gz`，分发安装包时应一并提供。模型权重不随这些命令打包。

## 6. 打包失败怎么查

打开失败的 Actions 运行，点击红色任务，再展开第一个失败步骤，读取末尾错误。其他步骤的 “Skipped” 往往是上游失败的结果。修改代码或配置后，需要提交并推送后重新触发；直接重跑旧运行仍然使用旧提交。

| 现象或失败步骤 | 处理方法 |
| --- | --- |
| 看不到 Run workflow | 检查 Actions 是否启用、账号是否有写权限、工作流是否已合入默认分支。 |
| Check release tag and application versions | 核对标签和五个版本文件；错误中会列出不一致项。尚未发布时可以在正确提交上使用新的版本标签，不要移动已发布标签。 |
| 下载失败、超时、HTTP 404 | 看清具体 URL 和步骤。暂时性网络故障可重跑；404 需要核对固定源地址，持续失败不能靠跳过依赖解决。CPU 运行时使用官方 NuGet 固定版本包并校验 SHA-256。 |
| SHA-256 mismatch | 不要删除校验或随意改哈希。先确认上游来源和实际文件；本地可删掉对应缓存文件后重新下载。运行时缓存为 `src-tauri/target/ai-runtime/`，解码器缓存为 `src-tauri/target/media-codec/sources/`。 |
| 完整 AI 验证下载或推理失败 | 只有手动勾选时才运行。网络问题可重跑；功能错误需修复后重新验证。日常打包可在新的手动运行中取消勾选，不代表完整 AI 验证已经通过。 |
| FFmpeg、WebKitGTK、MSVC 或 pkg-config 缺失 | 核对对应平台的依赖安装步骤；本地优先按第 5 节准备，Windows 特别注意 UCRT64 与 PowerShell 的区别。 |
| Check expected packages / Expected exactly one nonempty | 检查 Build desktop packages 是否成功、产物扩展名和目录是否与 Tauri 平台配置一致；缺少安装包不能发布。 |
| Artifacts 没有下载项 | 检查该平台任务和 Upload desktop packages 是否成功；过期文件需要重新构建。Artifact 下载需要登录且有仓库读权限。 |
| Publish GitHub Release 未运行 | 确认是推送版本标签触发，且配置检查、三平台构建、Intel macOS 检查全部成功。main、PR 和手动运行均不发布。 |
| Release 发布返回 403 / Resource not accessible | 检查发布作业的 `contents: write`、组织或仓库的 Actions 权限策略，以及发布相关仓库规则；不需要把 PAT 写入代码。 |
| Release 留在草稿 | 上传或附件检查没有完成。查看发布作业日志，修复临时问题后重跑；不要在附件不齐时手动发布草稿。 |

对于同一提交的临时故障，在运行页面右上角选择 **Re-run jobs → Re-run failed jobs**。如果 Artifact 已过期或缺失，应选择 **Re-run all jobs** 重新生成全部文件，再发布。已公开版本不会重新上传附件。

## 参考

- [GitHub：手动运行工作流](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)
- [GitHub：下载构建产物](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/download-workflow-artifacts)
- [Tauri：macOS 签名](https://v2.tauri.app/distribute/sign/macos/)
- [AI 搜索验证记录](../scripts/AI-SEARCH-VALIDATION.md)、[模型选择验证记录](../scripts/MODEL-OPTIONS-VALIDATION.md)

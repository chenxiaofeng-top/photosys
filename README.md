# PhotoSys

PhotoSys 是一个使用 Tauri 2、Vue 3、TypeScript 和 Rust 的桌面应用，用于浏览、收藏、查重和归档本地照片与视频。

## 便携媒体库

将 Linux x64 的 `PhotoSys.AppImage` 放到相册根目录。在文件属性中授予执行权限，或运行 `chmod +x PhotoSys.AppImage`，然后双击。启动时，PhotoSys 优先打开 AppImage 同目录中已有的媒体库，然后尝试恢复上次手动选定的目录。两处都没有可用媒体库时，在「媒体库」中点击「选择目录」。在外部添加、删除或改名文件后，点击「刷新」更新列表。

如果 Linux 环境无法挂载 AppImage，可在相册目录运行 `APPIMAGE_EXTRACT_AND_RUN=1 ./PhotoSys.AppImage`。

媒体库默认按拍摄日期从新到旧排列，每页显示 24 项。搜索框按文件名或相对路径进行不区分大小写的包含匹配，可与类型、日期范围、日期未知和收藏筛选叠加使用。输入停止 250 毫秒后更新结果，中文输入法组词期间不查询。可按拍摄日期、名称或文件大小升降序排序，日期未知始终排在日期排序末尾。更改搜索、筛选或排序后回到第一页；刷新当前目录保留条件，更换目录恢复默认条件。搜索和排序条件不跨应用重启保存。

可以在应用内预览 JPG/JPEG、PNG、WebP、TIFF、HEIC/HEIF 和 `.livp` 封面。支持解码的 MOV、MP4、M4V、3GP、MKV 和 WebM 视频会显示首帧缩略图，点击可查看大图。详情中的「上一项」「下一项」或左右方向键沿当前结果顺序跨页浏览全部媒体，首尾不循环，Esc 关闭。焦点位于视频播放控件时，方向键由播放器处理。关闭详情后，列表定位到最后查看项。照片和视频都可以用系统默认应用打开；在 `.livp` 详情中点击「播放实况」，可在应用内播放包内的 MOV 或 MP4，切换媒体会停止播放。

图片详情中可点击「向左旋转 90°」「向右旋转 90°」，自动保存浏览方向并同步列表缩略图。方向在图片已有的 EXIF 校正基础上叠加，连续同向旋转四次恢复原方向。支持所有可预览照片和 `.livp` 静态封面，视频及实况视频播放保持原方向。旋转记录按相对路径保存在 `.photosys/library.sqlite` 中，刷新、重启和整体搬迁相册后保留，不修改原图片；单独移动或重命名图片后不迁移记录。旧数据库首次保存旋转时自动增加旋转表，只读媒体库仍可浏览，但无法保存方向，保存失败会保留原角度。

详情中可点击「在文件管理器中显示」定位原文件，定位失败时尝试打开所在文件夹并提示无法选中文件；也可点击「复制完整路径」复制系统格式的绝对路径。文件在扫描后被移动、替换或删除时，需要刷新媒体库后重试。无法准确转换为文本的路径不会被复制。从媒体库切换到归档、查重或相似照片时，会带入当前目录。

手动选择目录时，PhotoSys 会读取其中已有的 `.photosys/library.sqlite`，或在数据库缺失时创建并初始化它。初始化失败时可查看错误并重选目录。已有有效数据库的只读目录可以浏览，初始化和保存收藏需要写入权限。

搬家时先关闭 PhotoSys，再复制整个相册目录，包括隐藏目录 `.photosys` 和随相册存放的 AppImage。直接启动搬迁后的 AppImage，或在安装版中选择搬迁后的目录。收藏按相对路径保存，整个相册换位置后仍可使用。单独移动或重命名媒体文件后，收藏仍对应原相对路径。

## 归档照片与视频

打开 PhotoSys，选择源目录和目标目录，点击「开始复制」。应用递归扫描源目录，按文件名里的日期或媒体内嵌拍摄时间，将原文件复制到目标目录的 `yyyy/yyyy-MM/` 下。没有可用日期的媒体进入 `其它/`。目标目录中同名且内容相同的文件会跳过，同名但内容不同的文件会以 `名称_1.扩展名` 等名称保存。

每次任务结束后，目标目录根目录会生成一份 `复制说明_yyyyMMdd_HHmmss.txt`。界面会显示本次统计和说明文件路径。

## 查找相似照片

打开「相似照片」，选择目录后开始扫描。应用递归查找 JPG/JPEG、PNG、WebP、TIFF 和 HEIC/HEIF，可按「同图变体」或「疑似连拍」筛选结果。展开相似组后，可任选两张照片并排查看预览和文件信息。照片仅供查看。

同图变体包括压缩、缩放、格式转换和轻微调色后的版本。疑似连拍还会检查 EXIF 拍摄时间，要求相差不超过 5 秒。复扫会使用本地 SQLite 索引复用未变化照片的图像特征。暂未支持的照片格式和读图失败会列在「问题明细」中。

## 本地 AI 内容搜索

在媒体库打开「AI 搜索设置」，选择模型及安装目录，再点击「下载并安装模型」。内容搜索提供三种 CPU 模型：

| 模型 | 下载量 | 使用建议 |
| --- | --- | --- |
| Chinese-CLIP B/16 · 均衡版（默认） | 约 755 MB | 资源开销较低，适合日常中文照片搜索 |
| Chinese-CLIP L/14 · 增强版 | 约 1.63 GB | 公开中文检索测试整体更好，分析更慢、更占内存 |
| Chinese-CLIP L/14@336px · 高分辨率版 | 约 1.63 GB | 更高输入分辨率，计算开销进一步增加 |

每个模型的设置中提供用途、限制、下载大小、CPU 开销、本机小样本性能、许可和来源说明；公开测试成绩不等于家庭相册准确率。模型按独立子目录存放，多个相册可以共用。应用记住所选目录，换电脑或路径失效后需要重新选择。更换目录不会搬动或删除旧模型。

默认通过第三方 HF-Mirror 镜像下载，失败后尝试 Hugging Face 原源，也可手动切换。下载支持取消、重试和断点续传；服务端不支持续传时重新下载对应文件。所选模型固定版本的全部文件通过大小、SHA-256 校验和本地推理自检后才完成安装。修复安装会保留旧目录为 `.photosys-model-backup-*`，确认新模型可用后可自行清理备份。

选择和安装不会改变当前模型；已安装的其它模型需要点击「使用此模型」，阅读当前相册待处理数量和影响后确认。选择对所有相册统一生效。切换时停止该类分析任务，切换后提示手动「重建内容索引」；刷新、重启或打开其它相册不会自动开始新模型的首次重建。重建前仅搜索与当前模型兼容的已索引照片，文件名浏览与搜索继续可用。重建逐批保存，可停止后继续；切回旧模型时已被覆盖的特征需要重新计算。原照片、收藏、浏览方向和旧模型文件保留。同模型换目录可复用有效索引。

模型就绪后，首次手动点击「建立索引」。之后打开或刷新相册时，自动分析新增、修改及旋转方向变化的照片。处理过程不锁定照片浏览；可点击「停止索引」，已保存部分会保留。失败项列在「AI 索引问题」，手动「更新索引 / 重试失败项」会重新尝试。只读相册可搜索已有有效索引，不能建立或更新索引。

将「搜索方式」切换到「照片内容」，输入“海边日落”“雪地里的小狗”等描述，再按回车或点击「搜索照片」。中文输入法组词时不会提交查询。结果按内容相关度排列，最多 200 张，每页 24 张，可结合拍摄日期、日期未知及收藏筛选；输入中的日期不会自动转换为筛选条件。索引未完成时只搜索已保存部分，并显示覆盖情况。文件名模式保持原有搜索、筛选与排序行为。

AI 索引保存在相册的 `.photosys/ai.sqlite`，使用相对路径，可随整个相册目录搬迁。索引不修改原照片、收藏或现有相似照片缓存。支持 JPG/JPEG、PNG、WebP、TIFF、HEIC/HEIF，以及 `.livp` 静态封面；沿用现有解码大小限制。首版不分析视频、不识别人名或图片文字。索引表示内容相似程度，不保证每张返回照片都符合描述。

模型安装后，建立索引和搜索可以完全断网。AI 运行时随软件打包，已关闭运行时遥测；照片、文件路径和搜索描述均不上传，无需另装 Python、Ollama 或显卡驱动。模型与原始资料的来源、固定版本和许可见 [AI 模型说明](scripts/AI-MODEL-NOTICES.md)。

无网络的电脑可使用「导入离线模型包」。在已安装模型的电脑上，用以下开发工具导出兼容 ZIP，再复制到目标电脑导入；ZIP 包含对应模型需要的 ONNX 文件、词表（内容搜索模型）、许可和模型标识；导入前选择匹配的模型，导入执行相同的校验与自检。原有不含模型标识的离线包仍通过固定文件校验兼容。

```sh
python3 scripts/package-ai-model.py /path/to/models/chinese-clip-vit-b16-fp32-v1 /path/to/photosys-model-v1.zip
# 导出其它模型时指定设置中对应的模型 ID
python3 scripts/package-ai-model.py --model-id chinese-clip-vit-l14-336px-fp32-v1 /path/to/models/chinese-clip-vit-l14-336px-fp32-v1 /path/to/photosys-model-l14-336px.zip
```

## 本地人物相册与家庭时间线

在顶部打开「人物」，沿用当前媒体库目录。在「人物模型设置」中可选择 YuNet + SFace 轻量版（默认，约 39 MB）或 YuNet + AuraFace 大模型版（约 261 MB），也可以导入对应离线 ZIP；内容搜索模型与人物模型独立管理，原有内容搜索设置继续有效。人物模型未单独指定目录时，默认使用已选的 AI 模型目录。安装后，点击「建立人物索引」。仅首次需要手动启用，此后打开或刷新相册会增量分析，处理期间可以浏览，可停止后继续，并重试失败项。

当前版本自动归组尚未通过家庭相册的独立标注验收，默认关闭。已检出的人脸进入「待整理」：选择人脸，归入已有的人物，或输入名称新建人物。小尺寸人脸也可手动整理。进入人物相册，在「管理人物与人脸」中改名、合并人物或纠正归属，也可以忽略误检及恢复已忽略的人脸。最近 100 次人工操作可依次撤销，重启后保留。照片详情显示人物名称，并提供人脸整理入口。

人物相册默认按拍摄日期从早到晚排列，可切换顺序，按年月导航、指定日期范围或仅看收藏；日期未知单列。同一照片含多张该人物的人脸时只显示一次。时间线沿用媒体库的拍摄日期，不推断年龄、亲属关系或事件。当前仅处理照片与实况静态封面，不分析视频。

人物名称、归属、纠错及人脸特征保存在 `.photosys/people.sqlite`，不修改原照片，可随整个相册目录搬迁。更换人物模型后，手动点击「重建人物索引」重新计算特征。同一照片内容、方向和检测流程下能唯一对应的人脸保留 ID、人工归属、忽略、纠错及撤销记录，只更新特征；无法可靠对应时进入待核对。有效的人工人物相册仍可浏览。AuraFace 沿用相同 YuNet 检测器，仅替换识别权重，不安装其它 InsightFace 模型，也不因更换模型开启自动归组。

文件被替换或旋转后，不自动把旧身份套用到新内容：旧人工标注出现在待整理中，提示核对当前人脸，旧标注可忽略。删除文件后不再显示其照片；单独移动或改名文件不迁移标注。只读相册可查看已有结果，不能分析或编辑。模型安装后可完全断网使用。

导出人物离线模型包：

```sh
python3 scripts/package-ai-model.py --kind people /path/to/yunet-sface-fp32-v1 /path/to/photosys-people.zip
```

人物数据库在需要写入时自动升级到版本 2；已有旧库可直接读取，只读模式不执行升级。旧版 PhotoSys 无法读取升级后的版本 2 人物数据库。

实现验证、性能范围和自动归组发布门槛见 [人物相册验证记录](scripts/PEOPLE-VALIDATION.md)，模型来源与许可见 [人物模型说明](scripts/PEOPLE-MODEL-NOTICES.md)。

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

准备好 FFmpeg 后，安装 Python 3.11 或更新版本，并执行 `python3 scripts/prepare-ai-runtime.py`，下载固定版本、校验 SHA-256 的官方 ONNX Runtime CPU 库与许可文件。Python 只供开发和构建使用，最终用户无需安装。在项目根目录执行以下命令。`tauri dev` 启动 Vite 和桌面窗口，关闭开发进程后可以执行打包。

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

在 macOS 上先运行 `bash scripts/build-media-codec.sh universal-apple-darwin` 和 `python3 scripts/prepare-ai-runtime.py --universal`，再用 `npm run tauri build -- --target universal-apple-darwin -- --locked` 构建通用包。通用包分别提供 Apple Silicon 和 Intel 的 CPU 运行时。本机打包产物位于 `src-tauri/target/release/bundle/`，指定 `--target` 后位于 `src-tauri/target/<target>/release/bundle/`。

AI 模型实测方法及当前验证范围见 [AI 搜索验证记录](scripts/AI-SEARCH-VALIDATION.md) 和 [模型选择验证记录](scripts/MODEL-OPTIONS-VALIDATION.md)。

## GitHub Actions

将项目推送到启用了 Actions 的 GitHub 仓库。推送到 `main`、打开或更新 PR 时，三个平台会并行运行测试并打包。工作流进入默认分支后，也可以在 **Actions → Desktop builds → Run workflow** 手动触发。在运行详情页的 **Artifacts** 下载对应安装包。

| Artifact | 构建环境 | 文件 |
| --- | --- | --- |
| `PhotoSys-windows-x64` | Windows Server 2025 x64 | 未签名的 Windows x64 NSIS `.exe` |
| `PhotoSys-linux-x64` | Ubuntu 24.04 x64 | Linux x64 `.deb` 和 `.AppImage` |
| `PhotoSys-macos-universal` | macOS 15 arm64 | 通用 `.dmg`，包含 `arm64` 和 `x86_64`，采用 ad-hoc 签名 |

安装后打开 PhotoSys，用临时目录试复制几张照片，检查归档子目录和复制说明，再用相同目录运行一次并核对重复跳过数量。Windows 和 macOS 的内部测试包可能触发 SmartScreen 或 Gatekeeper 提示。

各构建产物附带 `media-codec-sources.tar.gz`，其中包含媒体解码器的对应源码和构建脚本。安装包中包含许可文本。详见 [媒体解码器许可](scripts/MEDIA-CODEC-NOTICES.md)。

# 本地 AI 搜索验证记录

日期：2026-11-18。以下为本机实测及可复现步骤，不代表所有电脑或大型相册的性能。

## 环境与模型

- Linux x86_64，Intel Core i7-9750H，12 个逻辑线程，约 23 GiB 内存。
- ONNX Runtime CPU 1.24.2；推理限制为 4 个内部线程、1 个交互线程。
- Chinese-CLIP ViT-B/16 FP32 图文编码器，版本与 SHA-256 固定在安装器清单中。
- 测试照片均下载到临时目录，不随软件或仓库分发：PyTorch Hub 的白色小狗、Hugging Face 文档的沙发猫、Chinese-CLIP 上游的春节红灯笼。

## 已验证

- 中文、英文、标点、重音字符及超长中文的分词与固定 Python BERT 参考实现一致。
- 同一输入张量的 Rust / Python ONNX 输出余弦相似度大于 0.99999；包含 Rust / Pillow 缩放差异后，三张图分别为 0.99999523、0.999986、0.99998915。
- 三条人工描述“一只白色的狗”“沙发上的猫”“春节红灯笼”均将对应图片排在三张图片的第一位。这是小样本正确性检查，不能作为通用准确率。
- 真实 ZIP 离线导入、大小及 SHA-256 校验、双编码器自检、安装后模型复用通过。
- 真实安装器复用两个已下载编码器，从 5000 字节的部分词表恢复在线下载安装，通过固定文件校验。
- 真实模型相册流程覆盖：首次索引、无变化复扫、旋转后重算、新增及删除、旧会话拒绝、中途停止后的索引保存和恢复、中文内容排名。
- 单元测试覆盖：HTTP 续传、服务器忽略 Range、错误范围、取消、损坏模型、异常 ZIP、分页前筛选、前 200 项、日期与收藏、索引版本及旋转失效、相册移动及保留 mtime 的复制复用、只读索引搜索。
- 浏览器使用 Tauri IPC 模拟进行界面联调：选择目录、安装、首次索引、中文输入法输入后提交、收藏筛选。输入过程未触发内容搜索。此项不替代原生桌面端文件选择器测试。

## 小样本性能

以下为开发测试构建，已加载模型，样本数仅 3 张：

| 指标 | 本机测量 |
| --- | --- |
| 首次分析并保存 3 张图片 | 约 1.60 秒 |
| 无变化增量扫描，复用 3 张 | 约 1.59 毫秒 |
| 三条查询的文字编码与小样本匹配 | 约 74–102 毫秒 / 条 |
| 原生测试进程峰值 RSS | 约 945 MiB |

峰值内存来自独立运行真实相册测试进程，包含模型加载、SQLite、解码和多次更新，不包含 Vue / WebView。查询数字来自模型对照测试，不是完整桌面交互延迟。十万张以上照片的速度与内存尚需在真实相册上测量。

## 大图库查询基准

### 存储布局与首次搜索

仍使用旧布局、可写且未启用 WAL 的索引，可在「AI 搜索设置」中运行一次「优化搜索速度」，使用 SQLite `VACUUM` 改为 16 KiB 页。搜索仍逐条校验并精确排名。回归用例覆盖索引数据逐项比对、无模型的维护流程、重复执行、只读与 WAL 索引、空间不足、过期会话、维护互斥及 ZIP 备份恢复。

当前基准构造 1 万和 10 万条媒体与 512、768 维向量，交替运行 4 KiB 与 16 KiB 布局。对两种布局均用原有全排序流程核对前 200 项、组合筛选、列表及地图顺序和覆盖统计。计时从新建读取器开始，分别采样无筛选首查、日期／收藏／位置组合首查及缓存翻页。两种布局运行相同的精确评分器，4 KiB 基线关闭分页用的 SQL 语句缓存。

2026-11-24，Windows x64，Intel Core i7-9750H，默认 Rust 测试构建。各组采样 12 次首查和 96 次缓存翻页。下表为没有并行模型任务时的计时，单位为毫秒，格式为 P50 / P95。

| 媒体数 | 维度 | 筛选 | 4 KiB 首查 | 16 KiB 首查 | 4 KiB 翻页 | 16 KiB 翻页 |
| --- | ---: | --- | ---: | ---: | ---: | ---: |
| 1 万 | 512 | 无 | 112.76 / 121.98 | 54.33 / 69.02 | 0.273 / 0.465 | 0.268 / 0.447 |
| 1 万 | 512 | 组合 | 97.92 / 115.90 | 41.07 / 53.69 | 0.281 / 0.474 | 0.274 / 0.476 |
| 1 万 | 768 | 无 | 117.60 / 130.63 | 66.74 / 81.88 | 0.273 / 0.481 | 0.265 / 0.452 |
| 1 万 | 768 | 组合 | 91.19 / 111.02 | 45.12 / 52.17 | 0.274 / 0.469 | 0.265 / 0.475 |
| 10 万 | 512 | 无 | 1123.57 / 1224.04 | 541.37 / 570.75 | 0.270 / 0.485 | 0.265 / 0.555 |
| 10 万 | 512 | 组合 | 1003.25 / 1195.84 | 392.37 / 475.15 | 0.279 / 0.481 | 0.265 / 0.465 |
| 10 万 | 768 | 无 | 1134.96 / 1162.29 | 626.45 / 670.58 | 0.268 / 0.549 | 0.259 / 0.449 |
| 10 万 | 768 | 组合 | 917.55 / 953.80 | 421.18 / 437.01 | 0.267 / 0.496 | 0.260 / 0.473 |

10 万张无筛选首查 P95 分别降低 53.4% 和 42.3%。1 万张各组的缓存翻页 P95 最大回退 1.3%。10 万张、512 维、无筛选的缓存翻页 P95 增加 0.070 毫秒，其余三组下降。

真实模型另测了已加载的 Chinese-CLIP L/14@336px CPU 文本编码，四条查询共 24 次，P50 为 2.54 秒，P95 为 5.27 秒。该次测量有其他本机任务并行。桌面预览连接超时，本次未取得包含 IPC 和渲染的端到端耗时。

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib benchmark_content_search_ranking_and_pagination -- --ignored --nocapture --test-threads=1
PHOTOSYS_AI_MODEL_DIR=/path/to/installed/model-id cargo test --locked --manifest-path src-tauri/Cargo.toml --lib benchmark_content_search_text_encoding -- --ignored --nocapture --test-threads=1
```

### 2026-11-20 历史基准

合成 1 万和 10 万张媒体记录及 512 维 B/16 向量。逐项对照旧版全排序的前 200 个结果，覆盖同分排序、日期与收藏和地图的组合筛选，以及列表与地图复用排名。另有回归测试验证索引写入、收藏、旋转和相册切换后的缓存失效。

2026-11-20，Windows x64，Intel Core i7-9750H，Rust 默认测试构建。每种查询重复 12 次，下面是 Rust 数据库查询和排名耗时。测试直接提供查询向量，未计入模型推理、桌面 IPC 和界面渲染。

| 合成索引 | 查询 | 优化前 P50 / P95 | 优化后 P50 / P95 |
| --- | --- | ---: | ---: |
| 1 万张 | 首查 | 817.61 / 903.71 ms | 693.99 / 755.90 ms |
| 1 万张 | 翻页 | 802.89 / 880.26 ms | 0.47 / 0.66 ms |
| 10 万张 | 首查 | 7800.91 / 7915.06 ms | 6749.53 / 6916.19 ms |
| 10 万张 | 翻页 | 7820.28 / 8077.85 ms | 0.46 / 0.51 ms |

## 复现

普通回归检查：

```sh
python3 scripts/prepare-ai-runtime.py
python3 scripts/check-ai-runtime.py src-tauri/binaries/onnxruntime
npm run build
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

真实模型检查由开发者显式运行，默认测试不联网、不下载模型。先通过应用安装模型，或取得安装器清单指定的三个文件。独立 Python 参考工具仅供开发，不是应用依赖：

```sh
python3 -m pip install onnxruntime==1.24.2 transformers==4.57.6 pillow numpy
python3 scripts/verify-ai-reference.py /path/to/model /tmp/photosys-reference \
  --case '一只白色的狗=/path/to/dog.jpg' \
  --case '沙发上的猫=/path/to/cats.png' \
  --case '春节红灯笼=/path/to/festival.jpg'
PHOTOSYS_AI_REFERENCE=/tmp/photosys-reference cargo test --locked --manifest-path src-tauri/Cargo.toml real_model_reference -- --ignored --nocapture
PHOTOSYS_AI_REFERENCE=/tmp/photosys-reference cargo test --locked --manifest-path src-tauri/Cargo.toml real_album_incremental_cancel_and_search -- --ignored --nocapture
python3 scripts/package-ai-model.py /path/to/model /tmp/photosys-model.zip
PHOTOSYS_AI_PACKAGE=/tmp/photosys-model.zip cargo test --locked --manifest-path src-tauri/Cargo.toml real_offline_install_and_reuse -- --ignored --nocapture
PHOTOSYS_AI_MODEL_DIR=/path/to/model cargo test --locked --manifest-path src-tauri/Cargo.toml real_download_resumes_pinned_vocabulary -- --ignored --nocapture
```

## 高分辨率 HEIC 回归

部分 48 MP HEIC 在转成临时 JPEG 时，会因 FFmpeg 默认的 optimal Huffman 表缓冲区超过 256 MiB 单次分配限制而失败。共用 HEIC 转换改用 `-huffman default`，保留原始尺寸、画质参数、旋转及现有资源限制。

普通测试中的 `decodes_high_resolution_heic_for_indexing_and_preview` 使用合成 8064 × 6048 网格，验证 AI 解码后的尺寸与方向、详情预览及取消；测试样例不包含私人照片，可用 `python3 scripts/generate-heic-regression-fixture.py` 重新生成。

真实相册回归从已有索引中读取失败的 HEIC，复制到临时相册，使用已安装的 Chinese-CLIP B/16 模型运行应用索引入口。测试验证自动更新保留失败记录、手动更新重试并清除错误、已有成功向量逐字节保持一致，以及再次更新全部复用。原相册与索引只读；默认测试不运行这一项，也不下载模型。单独运行：

```sh
PHOTOSYS_HEIC_ALBUM=/path/to/album \
PHOTOSYS_AI_MODEL_DIR=/path/to/chinese-clip-vit-b16-fp32-v1 \
cargo test --locked --manifest-path src-tauri/Cargo.toml \
  real_failed_heic_retry_preserves_successful_index -- --ignored --nocapture --test-threads=1
```

本机实测：样例库 211 条 HEIC 索引中有 201 条成功、10 条失败；这 10 张照片在临时相册中全部完成解码与真实模型索引，原有成功向量保持不变，再次更新复用全部 11 条记录（10 张 HEIC 和 1 张成功对照图）。普通 Rust 回归为 86 项通过，15 项按设计忽略；上述真实 HEIC 测试另外单独运行通过。

安装包含修复的应用后，手动点击「更新索引 / 重试失败项」，无需删除数据库或重建成功项。

## 平台与限制

本机已验证 Linux 原生推理。三平台工作流已增加运行时下载、校验和安装包内加载检查；macOS 通用包检查两个架构，并为 Intel macOS 增加独立原生加载任务。Windows 和 macOS 的实际 CI 结果及原生交互仍需在相应系统运行后确认。

本机已生成 Linux deb（约 35 MB）和 AppImage（约 204 MB），从两种安装包解出后，AI 运行时均成功加载；同时检查了运行时许可文件、现有 FFmpeg 和 AppImage 的 VPX 插件。权重不随安装包分发。AppImage 构建需 patchelf；不支持 FUSE 的构建环境使用 `APPIMAGE_EXTRACT_AND_RUN=1`。

Intel macOS 使用上游 1.22.0 CPU 库，其他平台使用 1.24.2；Rust 绑定固定使用 C API 22。所有运行时均来自微软官方发布包，校验和固定，许可证随安装包提供，运行时遥测关闭。

## 内容与人物索引并发基准

基准每轮生成一个临时相册，包含 12 张单人 JPEG、12 张合成多人 JPEG、12 张图案 JPEG 和 4 张 48 MP HEIC。模型预热后，测量两种索引同时有任务时的总耗时、各阶段耗时、缩略图和内容查询 P95，以及无变化复扫。缩略图和查询计时覆盖原生处理流程。各模式使用同一套写入校验，同一后端的索引数据指纹可直接比较。

2026-11-20 在 Windows x64 上用发布配置实测，Intel Core i7-9750H（12 个逻辑线程）、NVIDIA GTX 1650（4 GiB）。每组运行三轮。工作集为测试进程峰值，显存为采样期间整张 NVIDIA 显卡的峰值占用。

| 后端与模式 | 两项总耗时中位数 | 缩略图 P95 | 搜索 P95 | 工作集 / 显存 |
| --- | ---: | ---: | ---: | ---: |
| CPU 逐张串行 | 20.993 s | 14.229 ms | 74.595 ms | 1081 MiB / — |
| CPU 单任务流水线 | 18.465 s | 15.490 ms | 80.085 ms | 1220 MiB / — |
| CPU 双任务 | 15.289 s | 20.477 ms | 100.700 ms | 1240 MiB / — |
| DirectML 逐张串行 | 10.133 s | 14.467 ms | 28.296 ms | 1440 MiB / 881 MiB |
| DirectML 单任务流水线 | 8.563 s | 19.176 ms | 33.756 ms | 1439 MiB / 881 MiB |
| DirectML 双任务 | 6.592 s | 22.984 ms | 32.200 ms | 1465 MiB / 881 MiB |

同一后端各模式均索引全部 40 张照片、检测到 48 个人脸，索引与人物归组指纹一致。无变化复扫复用全部结果，耗时 53–98 ms。CPU 双任务比单任务流水线节省 17.2%，缩略图和搜索 P95 分别增加 32.2% 和 25.7%，因此 CPU 默认使用单任务流水线。DirectML 双任务节省 23.0%，缩略图 P95 增加 19.9%，搜索 P95 下降 4.6%，达到发布门槛。首版默认开启范围限定为至少 12 个逻辑线程且实际使用 DirectML 的默认模型组合。

在 PowerShell 中设置已下载模型的上级目录和肖像样本路径。每个后端和模式分别启动一个测试进程，内存测试可按输出的 PID 采样进程工作集，显存测试按实际推理适配器采样。`PHOTOSYS_AI_BENCHMARK_BACKEND` 可设为 `cpu` 或 `directml`，`PHOTOSYS_AI_BENCHMARK_MODE` 可设为 `serial`、`pipeline` 或 `concurrent`。每个组合默认运行三轮。

```powershell
$env:PHOTOSYS_MODEL_OPTIONS_DIR = 'D:\path\to\model-options'
$env:PHOTOSYS_MODEL_OPTIONS_PORTRAIT = 'D:\path\to\portrait.jpg'
$env:PHOTOSYS_AI_BENCHMARK_BACKEND = 'cpu'
$env:PHOTOSYS_AI_BENCHMARK_MODE = 'pipeline'
$env:PHOTOSYS_AI_BENCHMARK_WORKLOAD = 'both'
$env:PHOTOSYS_AI_BENCHMARK_PHOTOS = '1000'
$env:PHOTOSYS_AI_BENCHMARK_TRIALS = '3'
cargo test --locked --release --manifest-path src-tauri/Cargo.toml --lib benchmark_index_pipeline -- --ignored --nocapture --test-threads=1
cargo test --locked --release --manifest-path src-tauri/Cargo.toml --lib maintenance_stops_both_indexes_and_preserves_completed_work -- --ignored --nocapture --test-threads=1
```

`PHOTOSYS_AI_BENCHMARK_WORKLOAD` 可设为 `both`、`search` 或 `people`。`PHOTOSYS_AI_BENCHMARK_PHOTOS` 默认为 40，使用 1,000 张时保持单人、多人、图案和 48 MP HEIC 的比例。每轮从空白索引开始，随后复扫并验证所有结果均被复用。输出包括模型加载与预热时间、实际解码次数、共享次数、解码与 GPU 等待时间、向量及人脸结果指纹。更改线程数时使用独立测试进程，可用 `PHOTOSYS_AI_BENCHMARK_CPU_THREADS` 在至少 12 个逻辑线程的 CPU 测试机上试跑 2、4、6 或 8 个推理线程。

### 真实图库前后对照

设置 `PHOTOSYS_AI_BENCHMARK_ALBUM` 为原片目录，设置 `PHOTOSYS_AI_BENCHMARK_OUTPUT` 为图库之外已创建的结果目录。基准直接读取原片，每轮在结果目录下新建 `trial-N`，存放 SQLite 索引和 `photos.json`。每次前后对照使用两个结果目录。基准在结束后复查符合索引条件的照片清单。结果目录中的照片清单包含原片的相对路径。

```powershell
$env:PHOTOSYS_AI_BENCHMARK_ALBUM = 'E:\path\to\album'
$env:PHOTOSYS_AI_BENCHMARK_OUTPUT = (New-Item -ItemType Directory src-tauri/target/real-ai-baseline).FullName
$env:PHOTOSYS_MODEL_OPTIONS_DIR = 'E:\path\to\models'
$env:PHOTOSYS_AI_BENCHMARK_SEARCH_MODEL = 'chinese-clip-vit-l14-336px-fp32-v1'
$env:PHOTOSYS_AI_BENCHMARK_PEOPLE_MODEL = 'yunet-auraface-fp32-v1'
$env:PHOTOSYS_AI_BENCHMARK_BACKEND = 'directml'
$env:PHOTOSYS_AI_BENCHMARK_MODE = 'pipeline'
$env:PHOTOSYS_AI_BENCHMARK_WORKLOAD = 'both'
$env:PHOTOSYS_AI_BENCHMARK_TRIALS = '1'
cargo test --locked --release --manifest-path src-tauri/Cargo.toml --lib --no-run
$testExecutable = Get-ChildItem src-tauri/target/release/deps -Filter 'photosys_lib-*.exe' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
# 保留基线构建时，将测试程序副本放在原来的 release/deps 目录。
./scripts/measure-ai-index.ps1 -Runner $testExecutable -OutputDirectory $env:PHOTOSYS_AI_BENCHMARK_OUTPUT
```

真实图库计时覆盖已预热模型对待索引照片的处理和写入，目录扫描、模型加载和无变化复扫分别计时。前台缩略图及查询 P95 使用合成图库测量。日志记录选用的模型、实际推理后端和每项索引的成功数、失败数、复用数。Windows 测量脚本单独记录主进程 CPU 时间、工作集和 NVIDIA GPU 利用率及显存，基准日志另外记录 FFmpeg 子进程的累计 CPU 时间。

对照实验每次只改一个选项。`PHOTOSYS_AI_BENCHMARK_FIXED_CLIP=1` 为合并的 L/14 模型尝试 DirectML 固定形状，日志中的 `fixed_clip_shape` 表示最终是否使用该会话。`PHOTOSYS_AI_BENCHMARK_HEIC_THREADS=4` 将 FFmpeg 的 HEIC 输入解码线程从 2 改为 4。CPU 单任务快速预处理使用 `PHOTOSYS_AI_BENCHMARK_OPTIMIZED_CPU=1` 和 `pipeline` 模式。

2026-11-21 在 i7-9750H / GTX 1650 上使用 Release 构建，150 张混合照片每组运行三轮。固定形状使用 L/14 336px 单项内容索引，其他两组使用 B/16 和 SFace 双项索引。

| 实验 | 建库耗时中位数（秒） | 缩略图 P95（毫秒） | 搜索 P95（毫秒） |
| --- | ---: | ---: | ---: |
| L/14 336px DirectML 固定形状 | 84.702 → 83.644 | 14.579 → 14.737 | 525.934 → 516.244 |
| HEIC 输入解码 2 → 4 线程 | 13.964 → 14.193 | 23.341 → 23.535 | 38.751 → 33.788 |
| CPU 单任务快速预处理 | 68.958 → 54.622 | 28.962 → 28.288 | 90.709 → 101.551 |

同一对照组前后索引指纹均相同。固定形状节省 1.25%，HEIC 4 线程增加 1.64%，CPU 快速预处理节省 20.79% 且搜索 P95 增加 11.95%。三个方案均未达到建库至少快 10% 且两项前台 P95 增幅均至多 10% 的门槛。

同机对真实图库中的 633 张可索引照片做两次完整建库，使用 L/14 336px 与 YuNet + AuraFace。两轮的模型配置和 DirectML 执行路径相同，差值反映同配置的重复测量波动。

| 任务 | 基线（秒） | 验收复测（秒） | 耗时差（秒） |
| --- | ---: | ---: | ---: |
| 媒体库 AI | 239.751 | 230.940 | 8.811 |
| 人物 AI | 183.782 | 176.326 | 7.456 |
| 两项总耗时 | 423.533 | 407.267 | 16.266（3.84%） |

两轮的照片清单和索引指纹相同。每项各成功 633 张、失败 0 张，共检出 793 个人脸。目录扫描用时 1.089 秒与 0.927 秒，模型加载和预热用时 8.718 秒与 7.271 秒。建库期间 GPU 平均占用 49.7% 与 47.8%，显存峰值分别为 2,328 MiB 与 2,034 MiB。FFmpeg 子进程分别累计使用 641.094 与 633.812 CPU 秒。

### 等价缩放、共享解码与推理调度

在同一台 Windows x64 测试机上，使用发布构建、默认模型和同一组 40 张照片各测三轮。CPU 对照在单任务流水线中试验快速预处理，DirectML 允许两项索引同时运行。耗时为三轮中位数，P95 合并三轮采样。可设置 `PHOTOSYS_AI_BENCHMARK_OPTIMIZED_CPU=1` 复现 CPU 试验路径。

| 后端与任务 | 建库耗时（秒） | 缩略图 P95（毫秒） | 搜索 P95（毫秒） | 进程峰值工作集（MiB） |
| --- | ---: | ---: | ---: | ---: |
| CPU 内容 | 15.44 → 11.08 | 20.70 → 23.29 | 107.53 → 97.38 | 1,042 → 1,022 |
| CPU 人物 | 6.24 → 5.06 | 27.39 → 26.61 | 139.84 → 136.84 | 1,192 → 1,021 |
| CPU 两项 | 22.44 → 16.55 | 18.58 → 22.80 | 134.19 → 118.10 | 1,220 → 1,032 |
| DirectML 内容 | 5.56 → 3.18 | 20.87 → 21.99 | 35.33 → 29.83 | 1,439 → 1,440 |
| DirectML 人物 | 3.75 → 2.82 | 26.36 → 23.00 | 31.72 → 31.54 | 1,439 → 1,439 |
| DirectML 两项（最终配对复测） | 6.68 → 4.11 | 23.20 → 24.45 | 31.24 → 32.28 | 1,466 → 1,440 |

每个后端和任务的前后索引指纹逐项一致。部分 40 张 CPU 场景的缩略图 P95 增幅超过 10%。该规模会反复浏览同一组已缓存的缩略图，建库时间会影响采样中缓存命中的比例。

在加入 Windows 后台线程优先级调整之前，2、4、6、8 个 CPU 推理线程的双项建库中位数分别为 23.72、17.86、17.41、18.23 秒。6 线程的搜索 P95 为 154.88 毫秒，超过基线 134.19 毫秒的 110%。8 线程的搜索 P95 为 212.87 毫秒。默认继续使用最多 4 个线程。

CPU 双索引扩大到 150 张照片再测三轮，缩略图循环超出缓存容量。试验路径的建库中位耗时从 63.91 秒降至 62.08 秒，缩略图 P95 从 26.60 升至 28.03 毫秒，搜索 P95 从 95.29 升至 133.45 毫秒。CPU 默认保留原流水线，快速路径随既有 CPU 并发开关试用。最终默认 CPU 配置在 40 张复测中的建库中位耗时为 21.62 秒，缩略图和搜索 P95 为 20.37 和 128.52 毫秒。

同机另测 1,000 张照片的 DirectML 双索引，其中 100 张为 48 MP HEIC。前后各运行三轮。

| 指标 | 基线 | 优化后 |
| --- | ---: | ---: |
| 建库中位耗时 | 166.24 秒 | 96.12 秒 |
| 缩略图 P95 | 24.89 毫秒 | 22.47 毫秒 |
| 搜索 P95 | 35.08 毫秒 | 36.46 毫秒 |
| 进程峰值工作集 | 1,478 MiB | 1,440 MiB |
| 采样显存峰值减去测试开始时的显存 | 1,011 MiB | 1,091 MiB |
| 实际解码次数（每轮） | 2,000 | 1,898 |

纯索引提速阶段的搜索 P95 曾达到 46.61 毫秒。加入前台搜索优先调度后，GPU 索引会让已开始的文字推理先完成，每次等待有 60 毫秒上限。最终三轮均分析 1,000 张照片并得到同一指纹，含 1,200 个人脸及其归组。无变化复扫复用全部结果，耗时 110–126 毫秒。人物写入和归组的累计耗时最多占当轮墙钟时间的 13.6%，SQLite 继续逐张提交。

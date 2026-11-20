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

合成 1 万和 10 万张媒体记录及 512 维 B/16 向量。逐项对照旧版全排序的前 200 个结果，覆盖同分排序、日期与收藏和地图的组合筛选，以及列表与地图复用排名。另有回归测试验证索引写入、收藏、旋转和相册切换后的缓存失效。

2026-11-20，Windows x64，Intel Core i7-9750H，Rust 默认测试构建。每种查询重复 12 次，下面是 Rust 数据库查询和排名耗时。测试直接提供查询向量，未计入模型推理、桌面 IPC 和界面渲染。

| 合成索引 | 查询 | 优化前 P50 / P95 | 优化后 P50 / P95 |
| --- | --- | ---: | ---: |
| 1 万张 | 首查 | 817.61 / 903.71 ms | 693.99 / 755.90 ms |
| 1 万张 | 翻页 | 802.89 / 880.26 ms | 0.47 / 0.66 ms |
| 10 万张 | 首查 | 7800.91 / 7915.06 ms | 6749.53 / 6916.19 ms |
| 10 万张 | 翻页 | 7820.28 / 8077.85 ms | 0.46 / 0.51 ms |

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib benchmark_content_search_ranking_and_pagination -- --ignored --nocapture --test-threads=1
```

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

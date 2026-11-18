# 模型选择验证记录

日期：2026-11-18。本记录区分安装与推理正确性、少量样例表现、尚未完成的家庭相册准确率和跨平台验收。

## 本机已验证

Linux x86_64，Intel Core i7-9750H（12 个逻辑线程），ONNX Runtime CPU 1.24.2，4 个内部线程，Rust 开发构建。

- 五种固定模型的原始文件大小、SHA-256、离线 ZIP 导入、包模型标识、安装前推理自检及安装后使用通过；错误包不替换现有安装。
- 三种搜索模型的中文、英文、标点、长文本与独立 Python BERT/ONNX 参考特征余弦相似度大于 0.99999。同一输入像素张量的图像特征对照大于 0.99999；Rust/Pillow 图像缩放后的对照大于 0.995。
- 白色小狗、沙发猫、春节红灯笼三条描述在三个样本中均将对应图片排第一。该检查不代表真实相册准确率。
- SFace 与 OpenCV 特征输出对照通过，AuraFace 与 Python ONNX 的 RGB、112×112、(value-127.5)/127.5 输入对照通过，余弦相似度均大于 0.99999。输入为独立对齐的人脸样例。
- 三种搜索模型在真实索引流程中验证首次手动开始、切换后拒绝自动重建、部分保存、中止续跑、复扫复用、切回旧模型以及 768 维两个不同模型的严格隔离。
- YuNet + SFace → YuNet + AuraFace 在公开肖像样例中验证：启用新模型后仍可浏览人工人物相册；自动分析拒绝执行；手动重建后人脸 ID、人工人物归属保留，向量变为 512 维，自动归组保持关闭。
- 最终普通 Rust 回归 84 项通过、14 项显式实测／规模测试默认忽略；前端类型检查和生产构建通过。五种模型的新增实测以及原有搜索和人物真实流程已单独运行。
- 普通回归覆盖多人脸唯一对应、歧义人脸待核对、替换照片与旋转不迁移身份、忽略和纠错保留、跨重启撤销、失败后的重试、旧人物库只读兼容与原子升级。

## 小样本性能

模型已加载，搜索使用单张 96×64 合成彩色图片（含旋转），人物使用单张已对齐的 112×112 彩色图片。每个计时仅一次，存在波动，不包含完整桌面交互、照片解码或大相册检索。合并的 L/14 图在文字查询时也有较高开销。

| 模型 | 文字编码 | 单图编码 / 单人脸特征 |
| --- | ---: | ---: |
| B/16 | 59 ms | 195 ms |
| L/14 | 579 ms | 598 ms |
| L/14@336px | 1846 ms | 2824 ms |
| YuNet + SFace | — | 18 ms（不含检测） |
| YuNet + AuraFace | — | 71 ms（不含检测） |

顺序执行五种模型完整离线安装、推理与参考测试的子进程峰值 RSS 为 2,069,620 KiB（约 2021 MiB），不含 WebView，不是各模型分别测量的峰值，也不是十万张相册的内存测量。权重未随安装包打包。

## 复现

普通回归不联网、不读取用户相册：

```sh
npm run build
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

下载校验与三平台原生检查（约 4.3 GB 文件，测试过程中需要额外 ZIP 和临时安装空间）：

```sh
python3 scripts/prepare-model-options-test.py /path/to/model-test
PHOTOSYS_MODEL_OPTIONS_DIR=/path/to/model-test cargo test --locked --manifest-path src-tauri/Cargo.toml real_model_options_install_infer_and_reference -- --ignored --nocapture --test-threads=1
PHOTOSYS_MODEL_OPTIONS_DIR=/path/to/model-test cargo test --locked --manifest-path src-tauri/Cargo.toml real_search_model_selection_manual_gate_cancel_resume_and_switch_back -- --ignored --nocapture --test-threads=1
PHOTOSYS_MODEL_OPTIONS_DIR=/path/to/model-test PHOTOSYS_MODEL_OPTIONS_PORTRAIT=/path/to/lena.jpg cargo test --locked --manifest-path src-tauri/Cargo.toml real_people_model_selection_keeps_manual_identity_and_requires_manual_start -- --ignored --nocapture --test-threads=1
```

独立参考工具使用已有开发 Python 环境：

```sh
python3 -m pip install numpy pillow transformers==4.57.6 onnxruntime==1.24.2 opencv-python-headless==4.12.0.88
python3 scripts/verify-model-options.py /path/to/model-test /path/to/reference --case '一只白色的狗=/path/to/dog.png' --aligned-face /path/to/aligned-112.png
PHOTOSYS_MODEL_OPTIONS_DIR=/path/to/model-test PHOTOSYS_MODEL_OPTIONS_REFERENCE=/path/to/reference PHOTOSYS_MODEL_OPTIONS_METRICS=/tmp/model-metrics.json cargo test --locked --manifest-path src-tauri/Cargo.toml real_model_options_install_infer_and_reference -- --ignored --nocapture --test-threads=1
```

## 验证范围

本机真实模型检查为 Linux。三平台工作流新增全部五种模型原生离线安装、推理和模型切换流程；Windows/macOS 的实际结果仍需对应 CI 执行后确认。Intel macOS 独立任务仍仅验证运行时加载。

界面以 Tauri IPC 模拟检查三种搜索与两种人物选项、说明、安装不启用、待选择与当前使用、切换确认、切换失败后保留原选择、刷新后不自动重建、手动重建、只读禁用以及 1280／390 像素布局；此项不替代原生目录选择器或跨平台交互验收。家庭相册准确率、跨年龄识别和自动归组独立验收尚未完成。

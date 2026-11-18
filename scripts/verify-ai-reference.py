#!/usr/bin/env python3
"""Generate independent Python ONNX/BERT/Pillow reference data (developer-only).

pip install onnxruntime==1.24.2 transformers==4.57.6 pillow numpy
python scripts/verify-ai-reference.py MODEL_DIR OUTPUT_DIR --case '小狗=/path/dog.jpg'
PHOTOSYS_AI_REFERENCE=OUTPUT_DIR cargo test --manifest-path src-tauri/Cargo.toml real_model_reference -- --ignored --nocapture
"""
import argparse
import json
from pathlib import Path
import time

import numpy as np
import onnxruntime as ort
from PIL import Image, ImageOps
from transformers import BertTokenizer, ChineseCLIPImageProcessor

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("model_directory", type=Path)
parser.add_argument("output_directory", type=Path)
parser.add_argument("--case", action="append", default=[], help="Chinese description=image path")
args = parser.parse_args()
args.output_directory.mkdir(parents=True, exist_ok=True)
tokenizer = BertTokenizer(vocab_file=str(args.model_directory / "vocab.txt"), do_lower_case=True)
processor = ChineseCLIPImageProcessor(size={"height": 224, "width": 224}, do_center_crop=False)
options = ort.SessionOptions()
options.intra_op_num_threads = 4
options.inter_op_num_threads = 1
vision = ort.InferenceSession(str(args.model_directory / "cn_clip_vision.onnx"), options, providers=["CPUExecutionProvider"])
text = ort.InferenceSession(str(args.model_directory / "cn_clip_text.onnx"), options, providers=["CPUExecutionProvider"])
queries = ["海边日落", "雪地里的小狗", "Hello，世界！Café", "2025年春节全家福", "猫" * 100]
cases = [case.split("=", 1) for case in args.case]
queries.extend(query for query, _ in cases)
reference = {"model_directory": str(args.model_directory.resolve()), "texts": [], "images": []}
for query in queries:
    inputs = tokenizer(query, return_tensors="np", max_length=52, padding="max_length", truncation=True)
    inputs = {input.name: inputs[input.name].astype(np.int64) for input in text.get_inputs()}
    vector = text.run(None, inputs)[0][0]
    vector /= np.linalg.norm(vector)
    reference["texts"].append({"query": query, "ids": inputs["input_ids"][0].tolist(), "vector": vector.tolist()})
for index, (query, path) in enumerate(cases):
    image = ImageOps.exif_transpose(Image.open(path)).convert("RGB")
    # Save the oriented RGB pixels losslessly so decoder differences cannot obscure the comparison.
    rgb_path = args.output_directory / f"image-{index}.png"
    image.save(rgb_path)
    inputs = processor(image, return_tensors="np")["pixel_values"].astype(np.float32)
    inputs.tofile(args.output_directory / f"pixels-{index}.f32")
    started = time.perf_counter()
    vector = vision.run(None, {"pixel_values": inputs})[0][0]
    elapsed = time.perf_counter() - started
    vector /= np.linalg.norm(vector)
    reference["images"].append({"query": query, "path": str(rgb_path.resolve()), "pixels": f"pixels-{index}.f32", "vector": vector.tolist(), "seconds": elapsed})
scores = np.array([t["vector"] for t in reference["texts"][-len(cases):]]) @ np.array([i["vector"] for i in reference["images"]]).T if cases else None
if scores is not None:
    reference["ranking"] = scores.tolist()
    for index, case in enumerate(cases):
        print(case[0], "=>", int(scores[index].argmax()), "expected", index, flush=True)
        assert int(scores[index].argmax()) == index, f"Unexpected semantic ranking: {case[0]}"
output = args.output_directory / "reference.json"
output.write_text(json.dumps(reference, ensure_ascii=False), encoding="utf-8")
print(output.resolve())

#!/usr/bin/env python3
"""Independent Python ONNX/BERT/Pillow references for all five model contracts.
Developer-only: numpy, pillow, transformers, onnxruntime, opencv-python-headless.
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
parser.add_argument("models", type=Path)
parser.add_argument("output", type=Path)
parser.add_argument("--case", action="append", default=[], help="Chinese description=image path")
parser.add_argument("--aligned-face", type=Path, help="independently aligned 112x112 RGB face PNG")
args = parser.parse_args()
catalog = json.loads((Path(__file__).parent / "ai-model-catalog.json").read_text())
args.output.mkdir(parents=True, exist_ok=True)
options = ort.SessionOptions()
options.intra_op_num_threads = 4
options.inter_op_num_threads = 1


def session(path):
    return ort.InferenceSession(str(path), options, providers=["CPUExecutionProvider"])


def normalized(value):
    value = value.reshape(-1)
    return (value / np.linalg.norm(value)).tolist()


for model in catalog:
    directory = args.models / model["id"]
    output = args.output / model["id"]
    output.mkdir(exist_ok=True)
    report = {"modelId": model["id"], "texts": [], "images": [], "faces": []}
    if model["kind"] == "search":
        combined = model["backend"] == "combined"
        vision = session(directory / ("model.onnx" if combined else "cn_clip_vision.onnx"))
        text = vision if combined else session(directory / "cn_clip_text.onnx")
        tokenizer = BertTokenizer(vocab_file=str(directory / "vocab.txt"), do_lower_case=True)
        edge = model["imageSize"]
        processor = ChineseCLIPImageProcessor(size={"height": edge, "width": edge}, do_center_crop=False)
        cases = [case.split("=", 1) for case in args.case]
        for query in ["海边日落", "Hello，世界！Café", "猫" * 100] + [q for q, _ in cases]:
            tokenized = tokenizer(query, return_tensors="np", max_length=52, padding="max_length", truncation=True)
            feeds = {i.name: tokenized[i.name].astype(np.int64) for i in text.get_inputs() if i.name != "pixel_values"}
            if combined:
                feeds["pixel_values"] = np.zeros((1, 3, edge, edge), np.float32)
            vector = text.run(["text_embeds"] if combined else None, feeds)[0]
            report["texts"].append({"query": query, "vector": normalized(vector)})
        for i, (query, path) in enumerate(cases):
            image = ImageOps.exif_transpose(Image.open(path)).convert("RGB")
            image_path = output / f"image-{i}.png"
            image.save(image_path)
            pixels = processor(image, return_tensors="np")["pixel_values"].astype(np.float32)
            pixel_path = output / f"pixels-{i}.f32"
            pixels.tofile(pixel_path)
            feeds = {"pixel_values": pixels}
            if combined:
                for entry in vision.get_inputs():
                    if entry.name != "pixel_values":
                        feeds[entry.name] = np.ones((1, 52), np.int64) if entry.name == "attention_mask" else np.zeros((1, 52), np.int64)
            started = time.perf_counter()
            vector = vision.run(["image_embeds"] if combined else None, feeds)[0]
            report["images"].append({"query": query, "path": str(image_path.resolve()), "pixels": str(pixel_path.resolve()), "vector": normalized(vector), "seconds": time.perf_counter() - started})
        del vision, text
    elif args.aligned_face:
        image = Image.open(args.aligned_face).convert("RGB")
        assert image.size == (112, 112)
        face_path = output / "aligned.png"
        image.save(face_path)
        if model["backend"] == "sface":
            import cv2
            recognizer = cv2.FaceRecognizerSF.create(str(directory / "face_recognition_sface_2021dec.onnx"), "")
            vector = recognizer.feature(np.array(image)[:, :, ::-1])
            del recognizer
        else:
            recognizer = session(directory / "glintr100.onnx")
            pixels = ((np.array(image).astype(np.float32) - 127.5) / 127.5).transpose(2, 0, 1)[None]
            vector = recognizer.run(None, {recognizer.get_inputs()[0].name: pixels})[0]
            del recognizer
        report["faces"].append({"path": str(face_path.resolve()), "vector": normalized(vector)})
    (output / "reference.json").write_text(json.dumps(report, ensure_ascii=False))
    print("Reference ready:", model["id"], flush=True)

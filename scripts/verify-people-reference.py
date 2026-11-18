#!/usr/bin/env python3
"""Generate upstream OpenCV reference output for Rust face pipeline verification.
Development dependencies only: numpy, pillow, opencv-python-headless==4.12.0.88.
"""
import argparse
import hashlib
import json
from pathlib import Path
import cv2
import numpy as np
from PIL import Image, ImageOps

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("models", type=Path)
parser.add_argument("output", type=Path)
parser.add_argument("images", type=Path, nargs="+")
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
detector = cv2.FaceDetectorYN.create(str(args.models / "face_detection_yunet_2023mar.onnx"), "", (640, 640), .9, .3, 5000)
recognizer = cv2.FaceRecognizerSF.create(str(args.models / "face_recognition_sface_2021dec.onnx"), "")
report = {"model_directory": str(args.models.resolve()), "cases": []}
for i, path in enumerate(args.images):
    image = np.array(ImageOps.exif_transpose(Image.open(path)).convert("RGB"))
    h, w = image.shape[:2]
    scale = min(640 / w, 640 / h)
    rw, rh = max(1, int(w * scale + .5)), max(1, int(h * scale + .5))
    # Feed the exact Rust resize tensor too; this separates interpolation from decoding.
    resized = cv2.resize(image, (rw, rh), interpolation=cv2.INTER_LINEAR)
    bgr = np.zeros((640, 640, 3), np.uint8)
    bgr[:rh, :rw] = resized[:, :, ::-1]
    _, detections = detector.detect(bgr)
    faces = []
    for d in [] if detections is None else detections[:100]:
        d = d.copy(); d[:14] /= scale
        aligned_bgr = recognizer.alignCrop(image[:, :, ::-1], d)
        vector = recognizer.feature(aligned_bgr).reshape(-1)
        vector /= np.linalg.norm(vector)
        aligned_name = f"{i}-{len(faces)}-aligned.png"
        Image.fromarray(aligned_bgr[:, :, ::-1]).save(args.output / aligned_name)
        faces.append({"box": [float(x) for x in d[:4]], "points": [[float(d[4+j*2]), float(d[5+j*2])] for j in range(5)], "score": float(d[14]), "vector": vector.tolist(), "aligned": aligned_name})
    image_name = f"{i}-image.png"
    Image.fromarray(image).save(args.output / image_name)
    report["cases"].append({"path": str((args.output / image_name).resolve()), "faces": faces, "source_sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
(args.output / "reference.json").write_text(json.dumps(report, indent=2))
print(args.output / "reference.json")

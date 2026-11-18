# PhotoSys AI search model

Chinese-CLIP ViT-B/16: https://github.com/OFA-Sys/Chinese-CLIP
Original weights/vocabulary: OFA-Sys/chinese-clip-vit-base-patch16,
revision 36e679e65c2a2fead755ae21162091293ad37834.
FP32 ONNX encoders: felixdu/chinese-clip-vit-base-patch16-onnx,
revision 47080d16c631d8416d2e6b155c59f8fd2c322e98.
The converted weights originate from the upstream Chinese-CLIP model.

Preprocessing v1: EXIF orientation, saved library rotation, white alpha
compositing, bicubic resize to 224x224 RGB, divide by 255,
mean [0.48145466, 0.4578275, 0.40821073],
std [0.26862954, 0.26130258, 0.27577711]. No center crop.
Text: BERT uncased basic tokenizer and WordPiece, CLS/SEP, 52 tokens,
zero padding, attention mask. Outputs: normalized 512-dimensional vectors.

Copyright (c) 2022-2023 OFA-Sys Team

Copyright (c) 2012-2022 Gabriel Ilharco, Mitchell Wortsman, Nicholas Carlini,
Rohan Taori, Achal Dave, Vaishaal Shankar, John Miller, Hongseok Namkoong,
Hannaneh Hajishirzi, Ali Farhadi, Ludwig Schmidt

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

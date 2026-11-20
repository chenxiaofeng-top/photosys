Generated ONNX Runtime libraries and their licenses are placed here by
python3 scripts/prepare-ai-runtime.py. They are bundled as application resources.
Model weights are downloaded separately by the application.
The script extracts native CPU libraries from SHA-256-pinned Microsoft NuGet
packages. Windows x64 also bundles ONNX Runtime DirectML and the DirectML
redistributable in directml/x86_64. Neither .NET nor Python is required to run
the packaged application.

"""Offline checks for release gates and draft-to-public publishing behavior."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SCRIPTS = Path(__file__).resolve().parents[1]


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


versions = load_script("check-release-version")
publisher = load_script("publish-desktop-release")
runtime = load_script("prepare-ai-runtime")
windows_runtime = load_script("prepare-windows-runtime")


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.archive = Path(temporary.name) / "runtime.nupkg"
        with zipfile.ZipFile(self.archive, "w") as source:
            for name in ["LICENSE", "ThirdPartyNotices.txt",
                         "runtimes/linux-x64/native/libonnxruntime.so",
                         "runtimes/linux-x64/native/libonnxruntime_providers_shared.so",
                         "runtimes/linux-arm64/native/libonnxruntime.so",
                         "runtimes/win-x64/native/onnxruntime.dll",
                         "runtimes/win-x64/native/onnxruntime.lib",
                         "runtimes/osx-arm64/native/libonnxruntime.dylib",
                         "runtimes/osx-x64/native/libonnxruntime.dylib"]:
                source.writestr(name, name)

    def test_only_selected_platform_native_libraries_and_licenses(self):
        expected = {
            "linux-x64": {"libonnxruntime.so", "libonnxruntime_providers_shared.so"},
            "win-x64": {"onnxruntime.dll"},
            "osx-arm64": {"libonnxruntime.dylib"},
            "osx-x64": {"libonnxruntime.dylib"},
        }
        for target, libraries in expected.items():
            with self.subTest(target=target):
                files = runtime.extract_runtime(self.archive, target)
                self.assertEqual(set(files), libraries | {"LICENSE", "ThirdPartyNotices.txt"})
                for library in libraries:
                    self.assertEqual(files[library].decode(), f"runtimes/{target}/native/{library}")

    def test_missing_library_or_license_is_rejected(self):
        with self.assertRaises(RuntimeError):
            runtime.extract_runtime(self.archive, "win-arm64")
        with zipfile.ZipFile(self.archive, "w") as source:
            source.writestr("runtimes/win-x64/native/onnxruntime.dll", b"native")
        with self.assertRaises(RuntimeError):
            runtime.extract_runtime(self.archive, "win-x64")

    def test_mismatched_download_hash_never_installs_runtime(self):
        with patch.object(runtime, "ROOT", self.archive.parent), \
                patch.object(runtime.urllib.request, "urlopen") as urlopen:
            urlopen.return_value.__enter__.return_value.read.side_effect = [b"wrong archive", b""]
            with self.assertRaisesRegex(RuntimeError, "SHA-256 mismatch"):
                runtime.prepare("Linux", "x86_64")
        self.assertFalse((self.archive.parent / "src-tauri/binaries/onnxruntime").exists())

    def test_directml_installs_only_the_x64_windows_redistributable_and_licenses(self):
        with zipfile.ZipFile(self.archive, "w") as source:
            for name in ("bin/x64-win/DirectML.dll", "bin/arm64-win/DirectML.dll",
                         "bin/x64-win/DirectML.Debug.dll", "LICENSE.txt", "LICENSE-CODE.txt",
                         "ThirdPartyNotices.txt"):
                source.writestr(name, name)
        self.assertEqual(runtime.extract_directml(self.archive), {
            "DirectML.dll": b"bin/x64-win/DirectML.dll",
            "DirectML-LICENSE.txt": b"LICENSE.txt",
            "DirectML-LICENSE-CODE.txt": b"LICENSE-CODE.txt",
            "DirectML-ThirdPartyNotices.txt": b"ThirdPartyNotices.txt",
        })
        with zipfile.ZipFile(self.archive, "w") as source:
            source.writestr("bin/x64-win/DirectML.dll", b"native")
        with self.assertRaisesRegex(RuntimeError, "Incomplete DirectML"):
            runtime.extract_directml(self.archive)


class WindowsRuntimeTests(unittest.TestCase):
    def test_staging_rejects_missing_dll_and_copies_only_installer_resources(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "redist"
            source.mkdir()
            output = root / "staged"
            files = list(windows_runtime.DLLS)
            for name in files[:-1] + ["concrt140.dll"]:
                (source / name).write_bytes(name.encode())
            with self.assertRaisesRegex(RuntimeError, files[-1]):
                windows_runtime.stage_crt(source, output)
            self.assertFalse(output.exists())

            (source / files[-1]).write_bytes(files[-1].encode())
            windows_runtime.stage_crt(source, output)
            self.assertEqual({path.name: path.read_bytes() for path in output.iterdir()},
                             {name: name.encode() for name in files})
            config = json.loads((SCRIPTS.parent / "src-tauri/tauri.windows.conf.json").read_text())
            self.assertEqual(config["bundle"]["resources"],
                             {f"target/windows-runtime/x64/{name}": name for name in files})


class VersionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "src-tauri").mkdir()
        self.files = {
            "package.json": '{"version":"0.1.1"}',
            "package-lock.json": '{"version":"0.1.1","packages":{"":{"version":"0.1.1"}}}',
            "src-tauri/tauri.conf.json": '{"version":"0.1.1"}',
            "src-tauri/Cargo.toml": '[package]\nname="photosys"\nversion="0.1.1"\n',
            "src-tauri/Cargo.lock": '[[package]]\nname="photosys"\nversion="0.1.1"\n',
        }
        self.write_files()

    def write_files(self):
        for name, content in self.files.items():
            (self.root / name).write_text(content, encoding="utf-8")

    def test_matching_versions(self):
        self.assertEqual(versions.check_versions(self.root, "v0.1.1"), "0.1.1")

    def test_each_manifest_and_lock_mismatch(self):
        for name, content in self.files.items():
            with self.subTest(file=name):
                (self.root / name).write_text(content.replace("0.1.1", "0.1.0", 1), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, name.split("/")[-1].replace(".", r"\.")):
                    versions.check_versions(self.root, "v0.1.1")
                self.write_files()

    def test_npm_root_package_mismatch(self):
        lock = json.loads(self.files["package-lock.json"])
        lock["packages"][""]["version"] = "0.1.0"
        (self.root / "package-lock.json").write_text(json.dumps(lock), encoding="utf-8")
        with self.assertRaises(ValueError):
            versions.check_versions(self.root, "v0.1.1")

    def test_invalid_and_prerelease_tags(self):
        for tag in ["0.1.1", "v0.1", "v0.1.1-beta.1", "v01.1.1", "v0.1.1\n", "v０.1.1"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                versions.check_versions(self.root, tag)

    def test_missing_local_cargo_package(self):
        (self.root / "src-tauri/Cargo.lock").write_text('package=[]\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "exactly one"):
            versions.check_versions(self.root, "v0.1.1")


class AssetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.artifacts = self.root / "artifacts"
        self.output = self.root / "assets"
        self.sources = []
        for artifact, name in [
            ("PhotoSys-windows-x64", "PhotoSys_0.1.1_x64-setup.exe"),
            ("PhotoSys-linux-x64", "PhotoSys_0.1.1_amd64.deb"),
            ("PhotoSys-linux-x64", "PhotoSys_0.1.1_amd64.AppImage"),
            ("PhotoSys-macos-universal", "PhotoSys_0.1.1_universal.dmg"),
            ("PhotoSys-linux-x64", "media-codec-sources.tar.gz"),
        ]:
            # upload-artifact preserves paths relative to their common ancestor.
            source = self.artifacts / artifact / "target/release/bundle" / name
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_bytes(name.encode())
            self.sources.append(source)
        extra = self.artifacts / "PhotoSys-windows-x64/binaries/media-codec-sources.tar.gz"
        extra.parent.mkdir(parents=True)
        extra.write_bytes(b"Windows source archive is not selected")

    def test_four_packages_linux_sources_and_checksums(self):
        assets = publisher.stage_assets(self.artifacts, self.output)
        self.assertEqual(len(assets), 6)
        for source in self.sources:
            self.assertEqual((self.output / source.name).read_bytes(), source.read_bytes())
        lines = (self.output / "SHA256SUMS").read_text().splitlines()
        self.assertEqual(len(lines), 5)
        for line in lines:
            digest, name = line.split("  ")
            self.assertEqual(digest, hashlib.sha256((self.output / name).read_bytes()).hexdigest())

    def test_missing_empty_and_duplicate_packages(self):
        package = self.sources[0]
        original = package.read_bytes()
        package.unlink()
        with self.assertRaises(ValueError):
            publisher.stage_assets(self.artifacts, self.output)
        package.touch()
        with self.assertRaises(ValueError):
            publisher.stage_assets(self.artifacts, self.output)
        package.write_bytes(original)
        package.with_name("duplicate.exe").write_bytes(b"duplicate")
        with self.assertRaises(ValueError):
            publisher.stage_assets(self.artifacts, self.output)


class PublishTests(unittest.TestCase):
    setUp = AssetTests.setUp

    def release(self, assets, draft=True):
        return {"draft": draft, "html_url": "https://github.com/example/photosys/releases/tag/v0.1.1",
                "assets": [{"name": p.name, "size": p.stat().st_size, "state": "uploaded"} for p in assets]}

    def test_create_upload_verify_then_publish(self):
        assets = publisher.stage_assets(self.artifacts, self.output)
        release = self.release(assets)
        with patch.object(publisher, "get_release", side_effect=[None, release, self.release(assets, draft=False)]), \
                patch.object(publisher, "gh") as gh:
            url, published = publisher.publish("example/photosys", "v0.1.1", assets)
        self.assertTrue(published)
        self.assertEqual(url, release["html_url"])
        self.assertEqual([call.args[:2] for call in gh.call_args_list],
                         [("release", "create"), ("release", "upload"), ("release", "edit")])
        self.assertIn("--draft", gh.call_args_list[0].args)
        self.assertIn("--verify-tag", gh.call_args_list[0].args)
        self.assertIn("--draft=false", gh.call_args_list[-1].args)

    def test_retry_updates_draft_without_creating_another(self):
        assets = publisher.stage_assets(self.artifacts, self.output)
        release = self.release(assets)
        with patch.object(publisher, "get_release", side_effect=[release, release, self.release(assets, draft=False)]), \
                patch.object(publisher, "gh") as gh:
            publisher.publish("example/photosys", "v0.1.1", assets)
        self.assertEqual([c.args[1] for c in gh.call_args_list], ["upload", "edit"])
        self.assertIn("--clobber", gh.call_args_list[0].args)

    def test_upload_failure_never_publishes(self):
        assets = publisher.stage_assets(self.artifacts, self.output)
        error = subprocess.CalledProcessError(1, ["gh"], stderr="upload interrupted")
        with patch.object(publisher, "get_release", return_value=self.release(assets)), \
                patch.object(publisher, "gh", side_effect=error) as gh:
            with self.assertRaises(subprocess.CalledProcessError):
                publisher.publish("example/photosys", "v0.1.1", assets)
        self.assertEqual(gh.call_count, 1)
        self.assertEqual(gh.call_args.args[:2], ("release", "upload"))

    def test_incomplete_remote_assets_remain_draft(self):
        assets = publisher.stage_assets(self.artifacts, self.output)
        for broken in [self.release(assets[:-1]), self.release(assets)]:
            if len(broken["assets"]) == len(assets):
                broken["assets"][0]["size"] = 0
            with self.subTest(assets=broken["assets"]), \
                    patch.object(publisher, "get_release", side_effect=[self.release(assets), broken]), \
                    patch.object(publisher, "gh") as gh:
                with self.assertRaises(ValueError):
                    publisher.publish("example/photosys", "v0.1.1", assets)
                self.assertEqual(gh.call_count, 1)

    def test_public_release_never_changes(self):
        with patch.object(publisher, "get_release", return_value=self.release([], draft=False)), \
                patch.object(publisher, "gh") as gh:
            _, published = publisher.publish("example/photosys", "v0.1.1", [])
        self.assertFalse(published)
        gh.assert_not_called()

    def test_draft_lookup_uses_cli_pending_tag_support(self):
        release = {"isDraft": True, "url": "https://github.com/example/photosys/releases/tag/v0.1.1",
                   "assets": []}
        with patch.object(publisher, "gh", return_value=json.dumps(release)) as gh:
            found = publisher.get_release("example/photosys", "v0.1.1")
        self.assertTrue(found["draft"])
        self.assertEqual(found["html_url"], release["url"])
        self.assertEqual(gh.call_args.args[:3], ("release", "view", "v0.1.1"))

    def test_only_missing_release_is_treated_as_missing(self):
        for message in ["release not found\n", "gh: Forbidden (HTTP 403)", "network error"]:
            with self.subTest(message=message), patch.object(publisher, "gh", side_effect=
                    subprocess.CalledProcessError(1, ["gh"], stderr=message)):
                if message.strip() == "release not found":
                    self.assertIsNone(publisher.get_release("example/photosys", "v0.1.1"))
                else:
                    with self.assertRaises(subprocess.CalledProcessError):
                        publisher.get_release("example/photosys", "v0.1.1")


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Generate xtool staging from the Xcode iOS app, share and widget targets.

Requires Python 3 and Pillow. Only generated files under apple/xtool/generated
are replaced. Xcode sources, package manifests and canonical icon stay intact.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import re
import shutil
import sys
import tempfile

APPLE = Path(__file__).resolve().parents[1]
PROJECT = APPLE / "NanocodexInbox.xcodeproj/project.pbxproj"
GENERATED = APPLE / "xtool/generated"
TARGETS = ("NanocodexInbox", "NanocodexShare", "NanocodexWidgets")


def require(condition, message):
    if not condition:
        raise ValueError(message)


class OpenStep:
    """The OpenStep plist grammar, without Apple's plutil or dependencies."""
    token = re.compile(r'\s+|//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|[{}()=;,]|[^\s{}()=;,]+', re.S)

    def __init__(self, text):
        self.tokens = [m.group() for m in self.token.finditer(text)
                       if not m.group().isspace() and not m.group().startswith(("//", "/*"))]
        self.index = 0

    def pop(self):
        require(self.index < len(self.tokens), "Unexpected end of Xcode project")
        token = self.tokens[self.index]
        self.index += 1
        return token

    def expect(self, wanted):
        actual = self.pop()
        require(actual == wanted, f"Xcode project: expected {wanted!r}, got {actual!r}")

    def value(self):
        token = self.pop()
        if token == "{":
            result = {}
            while self.tokens[self.index] != "}":
                key = self.value()
                self.expect("=")
                require(key not in result, f"Duplicate Xcode key: {key}")
                result[key] = self.value()
                self.expect(";")
            self.expect("}")
            return result
        if token == "(":
            result = []
            while self.tokens[self.index] != ")":
                result.append(self.value())
                if self.tokens[self.index] == ",":
                    self.pop()
                else:
                    break
            self.expect(")")
            return result
        return json.loads(token) if token.startswith('"') else token

    def read(self):
        value = self.value()
        require(self.index == len(self.tokens), "Trailing Xcode project data")
        return value


def local_path(path):
    result = (APPLE / path).resolve()
    require(result.is_relative_to(APPLE), f"Path escapes apple/: {path}")
    require(result.exists(), f"Missing Xcode input: {path}")
    return result


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def expand(value, settings):
    if isinstance(value, dict):
        return {k: expand(v, settings) for k, v in value.items()}
    if isinstance(value, list):
        return [expand(v, settings) for v in value]
    if not isinstance(value, str):
        return value
    def replace(match):
        key = match.group(1) or match.group(2)
        require(key in settings, f"Unknown plist substitution {key}")
        return str(settings[key])
    for _ in range(8):
        if not re.search(r'\$\(|\$\{', value):
            return value
        value = re.sub(r'\$\(([^)]+)\)|\$\{([^}]+)\}', replace, value)
    raise ValueError(f"Recursive plist substitution: {value}")


def icons(catalog, destination):
    """Linux has no actool. Preserve actual artwork with legacy icon PNG images."""
    from PIL import Image
    contents = {p.relative_to(catalog).as_posix() for p in catalog.rglob("*") if p.is_file()}
    allowed = {"Contents.json", "AppIcon.appiconset/Contents.json", "AppIcon.appiconset/AppIcon.png",
               "GoogleG.imageset/Contents.json", "GoogleG.imageset/google-g.png"}
    require(contents == allowed, "Asset catalog changed: add explicit Linux handling; refusing to omit assets")
    info = json.loads((catalog / "AppIcon.appiconset/Contents.json").read_text())
    require(info["images"] == [{"filename": "AppIcon.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"}],
            "AppIcon catalog changed: review Linux legacy icon conversion")
    with Image.open(catalog / "AppIcon.appiconset/AppIcon.png") as source:
        require(source.size == (1024, 1024), "AppIcon must be 1024x1024")
        for base, size in (("AppIcon60x60@2x", 120), ("AppIcon60x60@3x", 180),
                           ("AppIcon76x76", 76), ("AppIcon76x76@2x", 152),
                           ("AppIcon83.5x83.5@2x", 167)):
            source.resize((size, size), getattr(Image, "Resampling", Image).LANCZOS).save(destination / f"{base}.png")
    google = json.loads((catalog / "GoogleG.imageset/Contents.json").read_text())
    require(google["images"] == [{"filename": "google-g.png", "idiom": "universal"}],
            "GoogleG catalog changed: review Linux image handling")
    # SwiftUI Image("GoogleG") also resolves this named bundle PNG without actool.
    shutil.copyfile(catalog / "GoogleG.imageset/google-g.png", destination / "GoogleG.png")
    return {
        "CFBundleIcons": {"CFBundlePrimaryIcon": {"CFBundleIconFiles": ["AppIcon60x60"], "UIPrerenderedIcon": False}},
        "CFBundleIcons~ipad": {"CFBundlePrimaryIcon": {"CFBundleIconFiles": ["AppIcon60x60", "AppIcon76x76", "AppIcon83.5x83.5"], "UIPrerenderedIcon": False}},
    }


def build(stage, args):
    project_bytes = PROJECT.read_bytes()
    project = OpenStep(project_bytes.decode()).read()
    objects = project["objects"]
    native = {o["name"]: o for o in objects.values() if o.get("isa") == "PBXNativeTarget"}
    actual = {n for n, o in native.items() if o["productType"] in
              ("com.apple.product-type.application", "com.apple.product-type.app-extension")}
    require(actual == set(TARGETS), f"iOS targets changed: expected {TARGETS}, got {sorted(actual)}")
    package = {"deploymentTarget": None, "dependencies": [], "targets": []}
    report = {"projectSHA256": hashlib.sha256(project_bytes).hexdigest(), "targets": {}}
    package_refs = {}
    config = {"version": 1, "skipLSP": True, "extensions": []}
    for name in TARGETS:
        target = native[name]
        choices = [objects[c] for c in objects[target["buildConfigurationList"]]["buildConfigurations"]]
        settings = next(c["buildSettings"].copy() for c in choices if c["name"] == args.configuration.capitalize())
        require(settings["SWIFT_VERSION"] == "5.0", "Update Package.swift for changed Swift language mode")
        require(settings["TARGETED_DEVICE_FAMILY"] == "1,2", "Review changed device families")
        if package["deploymentTarget"] is None:
            package["deploymentTarget"] = settings["IPHONEOS_DEPLOYMENT_TARGET"]
        require(package["deploymentTarget"] == settings["IPHONEOS_DEPLOYMENT_TARGET"], "Mixed deployment targets need explicit SwiftPM support")
        settings["TARGET_NAME"] = name
        settings["PRODUCT_NAME"] = expand(settings["PRODUCT_NAME"], settings)
        settings["PRODUCT_MODULE_NAME"] = settings.get("PRODUCT_MODULE_NAME", name)
        if args.version:
            settings["MARKETING_VERSION"] = args.version
        if args.build_number:
            settings["CURRENT_PROJECT_VERSION"] = args.build_number
        require(settings["PRODUCT_MODULE_NAME"] == name, f"Swift module name changed for {name}")
        extension = target["productType"] == "com.apple.product-type.app-extension"
        dependencies = []
        for dependency_id in target.get("packageProductDependencies", []):
            product = objects[dependency_id]
            reference = objects[product["package"]]
            if reference["isa"] == "XCLocalSwiftPackageReference":
                path = reference["relativePath"]
                local_path(path + "/Package.swift")
                identity = Path(path).name.lower()
                dep = {"identity": identity, "path": path}
            else:
                require(reference["isa"] == "XCRemoteSwiftPackageReference", f"Unknown package reference {reference}")
                require(reference["requirement"]["kind"] == "exactVersion", "Remote dependency changed: implement its requirement explicitly")
                url = reference["repositoryURL"]
                identity = url.rstrip("/").rsplit("/", 1)[-1].removesuffix(".git").lower()
                dep = {"identity": identity, "url": url, "version": reference["requirement"]["version"]}
            if identity in package_refs:
                require(package_refs[identity] == dep, f"Conflicting package {identity}")
            package_refs[identity] = dep
            dependencies.append({"name": product["productName"], "package": identity})
        sources, resources, linked_products = [], [], []
        for phase_id in target["buildPhases"]:
            phase = objects[phase_id]
            kind = phase["isa"]
            require(kind in {"PBXSourcesBuildPhase", "PBXResourcesBuildPhase", "PBXFrameworksBuildPhase", "PBXCopyFilesBuildPhase"},
                    f"Unsupported build phase {kind} in {name}")
            if kind == "PBXCopyFilesBuildPhase":
                require(name == "NanocodexInbox" and phase["dstSubfolderSpec"] == "13", "Unknown copy phase; review Linux packaging")
                embedded = {objects[objects[b]["fileRef"]]["path"] for b in phase["files"]}
                require(embedded == {"NanocodexShare.appex", "NanocodexWidgets.appex"}, "Embedded extensions changed")
                continue
            for file_id in phase.get("files", []):
                item = objects[file_id]
                filters = item.get("platformFilters", [])
                if filters and "ios" not in filters:
                    continue
                require(not item.get("settings"), f"Untranslated per-file settings in {file_id}")
                if kind == "PBXFrameworksBuildPhase":
                    require("productRef" in item, "New native framework requires Linux linker/embed handling")
                    linked_products.append(objects[item["productRef"]]["productName"])
                    continue
                file = objects[item["fileRef"]]
                require(file.get("sourceTree") == "SOURCE_ROOT", f"Unsupported source tree for {file}")
                path = file["path"]
                local_path(path)
                (sources if kind == "PBXSourcesBuildPhase" else resources).append(path)
        require(set(linked_products) == {d["name"] for d in dependencies}, f"Framework and package dependencies differ for {name}")
        source_root = stage / "Sources" / name
        source_root.mkdir(parents=True)
        for source in sources:
            require(source.endswith(".swift"), f"Unsupported source language: {source}")
            output = source_root / source
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(local_path(source), output)
        resource_root = stage / "Bundles" / name
        resource_root.mkdir(parents=True)
        plist_path = settings.get("INFOPLIST_FILE", settings.get("INFOPLIST_FILE[sdk=iphoneos*]"))
        require(plist_path, f"No iOS plist for {name}")
        info = expand(plistlib.loads(local_path(plist_path).read_bytes()), settings)
        for key, value in settings.items():
            if key.startswith("INFOPLIST_KEY_"):
                key = key.removeprefix("INFOPLIST_KEY_")
                if key == "UIApplicationSceneManifest_Generation":
                    require(value == "YES", "Review scene manifest generation")
                    info.setdefault("UIApplicationSceneManifest", {"UIApplicationSupportsMultipleScenes": True})
                elif key == "UILaunchScreen_Generation":
                    require(value == "YES", "Review launch screen generation")
                    info.setdefault("UILaunchScreen", {})
                elif key == "UISupportedInterfaceOrientations_iPhone":
                    info.setdefault("UISupportedInterfaceOrientations", value.split())
                else:
                    info[key] = expand(value, settings)
        info.update({"CFBundleIdentifier": settings["PRODUCT_BUNDLE_IDENTIFIER"],
                     "CFBundleName": settings["PRODUCT_NAME"], "CFBundleExecutable": settings["PRODUCT_NAME"],
                     "CFBundleVersion": settings["CURRENT_PROJECT_VERSION"],
                     "CFBundleShortVersionString": settings["MARKETING_VERSION"],
                     "CFBundlePackageType": "XPC!" if extension else "APPL", "CFBundleInfoDictionaryVersion": "6.0",
                     "MinimumOSVersion": settings["IPHONEOS_DEPLOYMENT_TARGET"],
                     "UIDeviceFamily": [1, 2], "CFBundleSupportedPlatforms": ["iPhoneOS"], "LSRequiresIPhoneOS": True})
        for resource in resources:
            path = local_path(resource)
            if path.suffix == ".xcassets":
                info.update(icons(path, resource_root))
            else:
                require(path.is_file(), f"Directory resource needs explicit packaging: {resource}")
                output = resource_root / path.name
                require(not output.exists(), f"Resource filename collision: {resource}")
                shutil.copyfile(path, output)
        (stage / f"{name}.plist").write_bytes(plistlib.dumps(info))
        bundle_config = {"product": settings["PRODUCT_NAME"], "bundleID": settings["PRODUCT_BUNDLE_IDENTIFIER"],
                         "infoPath": f"xtool/generated/{name}.plist",
                         "resources": [f"xtool/generated/Bundles/{name}/{p.name}" for p in sorted(resource_root.iterdir())]}
        entitlement = settings.get("CODE_SIGN_ENTITLEMENTS")
        if entitlement:
            # Exact capability keys, no guessed or stripped entitlements.
            shutil.copyfile(local_path(entitlement), stage / f"{name}.entitlements")
            bundle_config["entitlementsPath"] = f"xtool/generated/{name}.entitlements"
        if extension:
            config["extensions"].append(bundle_config)
        else:
            config.update(bundle_config)
        defines = [d for d in settings.get("SWIFT_ACTIVE_COMPILATION_CONDITIONS", "").split() if d not in ("DEBUG", "$(inherited)")]
        conditional = {k: v for k, v in settings.items() if k.startswith("SWIFT_ACTIVE_COMPILATION_CONDITIONS[")}
        require(set(conditional).issubset({"SWIFT_ACTIVE_COMPILATION_CONDITIONS[sdk=iphoneos27.*]", "SWIFT_ACTIVE_COMPILATION_CONDITIONS[sdk=iphonesimulator27.*]"}),
                "New conditional Swift flags need Linux SDK handling")
        if args.sdk_version.startswith("27."):
            defines.extend(d for d in conditional.get("SWIFT_ACTIVE_COMPILATION_CONDITIONS[sdk=iphoneos27.*]", "").split() if d != "$(inherited)")
        linker_flags = ["-Xlinker", "-rpath", "-Xlinker", "@executable_path/Frameworks"]
        if extension:
            # Principal classes are discovered by name, not referenced by the stub.
            linker_flags += ["-Xlinker", "-ObjC", "-Xlinker", "-rpath", "-Xlinker", "@executable_path/../../Frameworks", "-Xlinker", "-application_extension"]
            if info.get("NSExtension", {}).get("NSExtensionPointIdentifier") == "com.apple.widgetkit-extension":
                # xtool defaults all extensions to Foundation's NSExtensionMain;
                # WidgetBundle's @main must run its generated Swift entry point.
                linker_flags += ["-Xlinker", "-e", "-Xlinker", "_main"]
        package["targets"].append({"name": name, "product": settings["PRODUCT_NAME"], "dependencies": dependencies,
                                   "defines": sorted(set(defines)), "extensionTarget": extension, "linkerFlags": linker_flags})
        report["targets"][name] = {"module": name, "product": settings["PRODUCT_NAME"], "sources": sources,
                                  "resources": resources, "dependencies": dependencies, "entitlements": entitlement,
                                  "defines": sorted(set(defines))}
    package["dependencies"] = list(package_refs.values())
    # Use the same dependency pins as the canonical Xcode app without allowing
    # SwiftPM to rewrite Xcode's lockfile when building this separate package.
    lockfile = APPLE / "NanocodexInbox.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved"
    require(lockfile.is_file(), "The app dependency lockfile is missing")
    shutil.copyfile(lockfile, stage / "Package.resolved")
    write_json(stage / "package.json", package)
    write_json(stage / "membership.json", report)
    # JSON is a YAML subset; avoid adding a YAML serializer dependency.
    write_json(stage / "xtool.yml", config)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--configuration", choices=("debug", "release"), default="release")
    parser.add_argument("--version", help="Override MARKETING_VERSION in all bundles")
    parser.add_argument("--build-number", help="Override CURRENT_PROJECT_VERSION in all bundles")
    parser.add_argument("--sdk-version", default="26.0", help="Actual Apple SDK version, controls SDK-conditional Swift flags")
    args = parser.parse_args()
    # Reject malformed overrides before touching the last successful staging.
    if args.version is not None and not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version):
        parser.error("--version must be major.minor.patch using decimal integers")
    if args.build_number is not None and not re.fullmatch(r"[1-9][0-9]*", args.build_number):
        parser.error("--build-number must be a positive decimal integer")
    if not re.fullmatch(r"[1-9][0-9]*\.[0-9]+(?:\.[0-9]+)?", args.sdk_version):
        parser.error("--sdk-version must be major.minor or major.minor.patch using decimal integers")
    GENERATED.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix=".prepare-", dir=GENERATED.parent))
    try:
        build(temporary, args)
        if GENERATED.exists():
            shutil.rmtree(GENERATED)
        temporary.rename(GENERATED)
        report = json.loads((GENERATED / "membership.json").read_text())
        for name, target in report["targets"].items():
            print(f"{name}: {len(target['sources'])} Xcode sources, {len(target['resources'])} resources, {len(target['dependencies'])} packages")
        print(f"Staged {GENERATED.relative_to(APPLE)}; no compilation or signing performed.")
    except (ValueError, KeyError, ImportError, OSError) as error:
        print(f"prepare-xtool: {error}", file=sys.stderr)
        return 1
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)
    return 0


if __name__ == "__main__":
    sys.exit(main())

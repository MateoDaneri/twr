#!/usr/bin/env python3
"""Build and operate this local prototype without changing user dotfiles."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import zipfile
import stat

ROOT = Path(__file__).resolve().parents[1]
SOCKET_DIRECTORY = Path("/private/tmp") / ("terminal-workspace-%s-%s" % (os.getuid(), hashlib.sha256(str(ROOT).encode()).hexdigest()[:12]))
SOCKET = SOCKET_DIRECTORY / "tmux.sock"
APP = SOCKET_DIRECTORY / "Terminal Workspace Native.app"
ARCHIVE = ROOT/"dist/Terminal Workspace Prototype.zip"
TMUX = Path("/opt/homebrew/bin/tmux")

def run(args, **kwargs):
    return subprocess.run([str(x) for x in args], check=True, **kwargs)

def snapshot():
    runtime = ROOT / "runtime"
    runtime.mkdir(mode=0o700, parents=True, exist_ok=True)
    files = [Path.home()/".config/ghostty/config", Path.home()/".config/ghostty/ghostty.conf", Path.home()/".config/tmux/tmux.conf", Path.home()/".config/tmux/theme.conf", Path.home()/".config/nvim/init.lua"]
    files += sorted((Path.home()/".config/nvim/lua").rglob("*.lua"))
    files += sorted((Path.home()/".config/tmux").glob("*.conf"))
    files += [Path.home()/".config/nvim/lazy-lock.json"]
    data = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in files if p.is_file()}
    target = runtime / "configuration-baseline.json"
    if not target.exists():
        target.write_text(json.dumps(data, indent=2) + "\n")
    return data

def build():
    run(["swift", "build", "--package-path", ROOT, "--scratch-path", ROOT/".build/mac-native", "--disable-sandbox"])
    package()

def rebuild_ui(package_result=True):
    """Reuse pinned SwiftTerm and SDK modules from this host's completed build."""
    product = ROOT/".build/mac-native/out/Products/Debug"
    maps = sorted((ROOT/".build/mac-native/out/Intermediates.noindex/TerminalWorkspace.build/Debug/TerminalWorkspace-p.build/Objects-normal").glob("*/TerminalWorkspace-dependencies-1.json"))
    if not (product/"SwiftTerm.o").is_file() or not maps:
        raise SystemExit("Primero ejecutar build completo para compilar SwiftTerm y los módulos del SDK.")
    sdk = subprocess.check_output(["/usr/bin/xcrun", "--show-sdk-path"], text=True).strip()
    sdk_version = subprocess.check_output(["/usr/bin/xcrun", "--show-sdk-version"], text=True).strip()
    compiler = Path(subprocess.check_output(["/usr/bin/xcrun", "--find", "swiftc"], text=True).strip())
    target = os.uname().machine+"-apple-macos14.0"
    ui_object = ROOT/".build/TerminalWorkspace-ui.o"
    run([compiler.with_name("swift-frontend"), "-frontend", "-c", "-whole-module-optimization", "-parse-as-library", "-swift-version", "5", "-module-name", "TerminalWorkspace", "-target", target, "-clang-target", os.uname().machine+"-apple-macos"+sdk_version, "-sdk", sdk, "-I", product, "-disable-cross-import-overlay-search", "-disable-implicit-swift-modules", "-module-cache-path", ROOT/".build/mac-native/out/Intermediates.noindex/SwiftExplicitPrecompiledModules", "-validate-clang-modules-once", "-clang-build-session-file", ROOT/".build/mac-native/out/ModuleCache.noindex/Session.modulevalidation", "-explicit-swift-module-map-file", maps[0], "-enable-objc-interop", "-Xcc", "-fno-implicit-modules", "-Xcc", "-fno-implicit-module-maps", *sorted((ROOT/"Sources/TerminalWorkspace").glob("*.swift")), "-o", ui_object])
    run([compiler, "-target", target, "-sdk", sdk, ui_object, product/"SwiftTerm.o", "-o", product/"TerminalWorkspace"])
    if package_result:
        package()

def package():
    SOCKET_DIRECTORY.mkdir(mode=0o700, parents=False, exist_ok=True)
    mode = SOCKET_DIRECTORY.lstat()
    if not stat.S_ISDIR(mode.st_mode) or mode.st_uid != os.getuid() or stat.S_IMODE(mode.st_mode) != 0o700:
        raise SystemExit("El directorio del prototipo debe ser privado, propio y sin symlink.")
    binary = ROOT / ".build/mac-native/debug/TerminalWorkspace"
    macos = APP / "Contents/MacOS"
    macos.mkdir(parents=True, exist_ok=True)
    # A running GUI may still map the old inode. Replace atomically so rebuilding
    # cannot change executable pages beneath that instance.
    replacement = macos / "TerminalWorkspace.next"
    shutil.copy2(binary, replacement)
    os.replace(replacement, macos / "TerminalWorkspace")
    shutil.copytree(ROOT/"Resources", APP/"Contents/Resources", dirs_exist_ok=True)
    info = {"CFBundleExecutable":"TerminalWorkspace", "CFBundleIdentifier":"local.terminalworkspace.prototype.native", "CFBundleName":"Terminal Workspace", "CFBundleDisplayName":"Terminal Workspace", "CFBundlePackageType":"APPL", "CFBundleShortVersionString":"0.1.1", "LSMinimumSystemVersion":"14.0", "NSHighResolutionCapable":True, "PrototypeSourceRoot":str(ROOT)}
    with (APP/"Contents/Info.plist").open("wb") as f:
        plistlib.dump(info, f)
    # Finder can mark a generated .app folder after it is opened. That cosmetic
    # attribute blocks signing; retain provenance and all source file metadata.
    subprocess.run(["/usr/bin/xattr", "-d", "com.apple.FinderInfo", str(APP)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    run(["/usr/bin/codesign", "--force", "--sign", "-", APP])
    run([APP/"Contents/MacOS/TerminalWorkspace", "--self-test"])
    ARCHIVE.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(ARCHIVE, "w", zipfile.ZIP_DEFLATED) as archive:
        for file in sorted(APP.rglob("*")):
            if file.is_file(): archive.write(file, file.relative_to(APP.parent))
    print(APP)
    print(ARCHIVE)

def package_app(demo=False):
    stage = Path("/private/tmp") / ("terminal-workspace-bundle-%s" % os.getuid())
    stage.mkdir(mode=0o700, exist_ok=True)
    mode = stage.lstat()
    if not stat.S_ISDIR(mode.st_mode) or mode.st_uid != os.getuid() or stat.S_IMODE(mode.st_mode) != 0o700:
        raise SystemExit("El directorio del bundle debe ser privado y propio.")
    label = "Terminal Workspace Demo" if demo else "Terminal Workspace"
    app = stage / (label + ".app")
    macos = app / "Contents/MacOS"
    macos.mkdir(parents=True, exist_ok=True)
    replacement = macos / "TerminalWorkspace.next"
    shutil.copy2(ROOT/".build/mac-native/debug/TerminalWorkspace", replacement)
    os.replace(replacement, macos/"TerminalWorkspace")
    shutil.copytree(ROOT/"Resources", app/"Contents/Resources", dirs_exist_ok=True)
    guides = app/"Contents/Resources/Guides"
    guides.mkdir(exist_ok=True)
    for document in (ROOT/"docs").glob("*.html"):
        if ".template." not in document.name:
            shutil.copy2(document, guides/document.name)
    info = {"CFBundleExecutable":"TerminalWorkspace",
        "CFBundleIdentifier":"local.terminalworkspace.demo" if demo else "local.terminalworkspace.app",
        "CFBundleName":label, "CFBundleDisplayName":label, "CFBundlePackageType":"APPL",
        "CFBundleShortVersionString":"0.2.0", "LSMinimumSystemVersion":"14.0",
        "NSHighResolutionCapable":True, "WorkspaceDemoMode":demo}
    if demo:
        info["WorkspaceDemoDataRoot"] = str(ROOT/"runtime/workspace-demo")
    with (app/"Contents/Info.plist").open("wb") as stream:
        plistlib.dump(info, stream)
    subprocess.run(["/usr/bin/xattr","-d","com.apple.FinderInfo",str(app)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    run(["/usr/bin/codesign","--force","--sign","-",app])
    run([app/"Contents/MacOS/TerminalWorkspace","--self-test"])
    run(["/usr/bin/codesign","--verify","--strict",app])
    archive_path = ROOT/"dist"/(label+".zip")
    archive_path.parent.mkdir(exist_ok=True)
    with zipfile.ZipFile(archive_path,"w",zipfile.ZIP_DEFLATED) as archive:
        for file in sorted(app.rglob("*")):
            if file.is_file():
                archive.write(file,file.relative_to(app.parent))
    print(app)
    print(archive_path)

def doctor(inspect_config=False):
    changed = []
    result = {"application":str(APP), "built":APP.exists(), "sessionSocket":str(SOCKET), "configurationInspection":"not_requested", "paths":{"tmux":str(TMUX),"nvim":"/opt/homebrew/bin/nvim"}}
    if inspect_config:
        observed = snapshot()
        baseline = json.loads((ROOT/"runtime/configuration-baseline.json").read_text())
        changed = [p for p, digest in baseline.items() if observed.get(p) != digest]
        result["configurationInspection"] = "explicit_opt_in"
        result["configurationChangesSinceBaseline"] = changed
    health = ROOT/"runtime/health.json"
    if health.exists():
        result["lastApplicationSnapshot"] = json.loads(health.read_text())
        result["snapshotIsLiveProof"] = False
    if SOCKET.exists():
        p = subprocess.run([str(TMUX),"-S",str(SOCKET),"list-panes","-a","-F","#{pane_id} #{pane_current_command} #{pane_width}x#{pane_height}"], capture_output=True, text=True)
        result["livePanes"] = p.stdout.splitlines()
        result["tmuxExitCode"] = p.returncode
    print(json.dumps(result, indent=2))
    return bool(changed)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["build", "rebuild-ui", "compile-ui", "package", "package-app", "launch", "doctor", "attach", "socket"])
    parser.add_argument("--inspect-config", action="store_true", help="Solo doctor: inspeccionar configs personales explícitamente.")
    parser.add_argument("--demo", action="store_true", help="Empaquetar app con perfil sintético.")
    args = parser.parse_args()
    if args.inspect_config and args.command != "doctor":
        parser.error("--inspect-config solo se admite con doctor")
    if args.command == "build": build()
    elif args.command == "rebuild-ui": rebuild_ui()
    elif args.command == "compile-ui": rebuild_ui(package_result=False)
    elif args.command == "package": package()
    elif args.command == "package-app": package_app(demo=args.demo)
    elif args.command == "launch":
        if not APP.exists(): raise SystemExit("Primero ejecutar build.")
        run(["/usr/bin/open", "-n", APP])
    elif args.command == "doctor": return int(doctor(inspect_config=args.inspect_config))
    elif args.command == "socket": print(SOCKET)
    elif args.command == "attach": os.execv(str(TMUX), [str(TMUX), "-S", str(SOCKET), "attach-session", "-t", "prototype"])
    return 0

if __name__ == "__main__":
    sys.exit(main())

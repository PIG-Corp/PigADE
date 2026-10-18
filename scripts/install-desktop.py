#!/usr/bin/env python3
"""Install or refresh PigADE's per-user desktop launcher."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys


MARKER = "# Managed by PigADE scripts/install-desktop.py"


def desktop_quote(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    escaped = escaped.replace("`", "\\`").replace("$", "\\$")
    return f'"{escaped}"'


def safe_install_file(path: Path, contents: bytes, marker: bytes) -> None:
    if path.is_symlink():
        raise RuntimeError(f"Refusing to replace symlink: {path}")
    if path.exists():
        current = path.read_bytes()
        if current != contents and marker not in current:
            raise RuntimeError(f"Refusing to replace unrelated file: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(f".{path.name}.pigade-tmp")
    temp.write_bytes(contents)
    temp.replace(path)


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    home = Path.home()
    data_home = Path(os.environ.get("XDG_DATA_HOME", home / ".local/share"))
    if not data_home.is_absolute():
        raise RuntimeError("XDG_DATA_HOME must be an absolute path")

    wrapper_source = repo / "scripts/run-desktop.sh"
    icon_source = repo / "crates/app/assets/desktop/org.pigade.app.svg"
    for required in (wrapper_source, icon_source):
        if not required.is_file():
            raise RuntimeError(f"Required launcher asset is missing: {required}")

    bin_dir = home / ".local/bin"
    wrapper = bin_dir / "pigade"
    bin_dir.mkdir(parents=True, exist_ok=True)
    if wrapper.is_symlink():
        if wrapper.resolve() != wrapper_source.resolve():
            raise RuntimeError(f"Refusing to replace unrelated symlink: {wrapper}")
    elif wrapper.exists():
        raise RuntimeError(f"Refusing to replace existing file: {wrapper}")
    else:
        wrapper.symlink_to(wrapper_source)

    apps_dir = data_home / "applications"
    icons_dir = data_home / "icons/hicolor/scalable/apps"
    desktop = apps_dir / "org.pigade.app.desktop"
    icon = icons_dir / "org.pigade.app.svg"
    desktop_text = "\n".join(
        (
            MARKER,
            "[Desktop Entry]",
            "Type=Application",
            "Version=1.0",
            "Name=PigADE",
            "Comment=PigADE dev 0.0.1 development environment",
            "Exec=" + desktop_quote(str(wrapper)),
            "TryExec=" + str(wrapper),
            "Icon=org.pigade.app",
            "Terminal=false",
            "Categories=Development;",
            "StartupWMClass=org.pigade.app",
            "",
        )
    ).encode()
    icon_bytes = icon_source.read_bytes()
    safe_install_file(desktop, desktop_text, MARKER.encode())
    safe_install_file(icon, icon_bytes, b"PigADE desktop icon")

    updater = shutil.which("update-desktop-database")
    if updater:
        subprocess.run([updater, str(apps_dir)], check=True)
    print(f"Installed PigADE launcher: {desktop}")
    print(f"Stable command: {wrapper}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as exc:
        print(f"install-desktop: {exc}", file=sys.stderr)
        raise SystemExit(1)

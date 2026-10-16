#!/usr/bin/env python3
"""Verify canonical first-party development version without altering vendor pins."""
import pathlib
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]
expected = (root / "VERSION").read_text().strip()
workspace = tomllib.loads((root / "Cargo.toml").read_text())
assert workspace["workspace"]["package"]["version"] == expected
app = tomllib.loads((root / "crates/app/Cargo.toml").read_text())
assert app["package"]["version"] == expected
for name in ("terminal", "storage", "session-host", "layout"):
    package = tomllib.loads((root / f"crates/{name}/Cargo.toml").read_text())["package"]
    assert package["version"] == {"workspace": True}
print(f"First-party versions aligned: {expected}")

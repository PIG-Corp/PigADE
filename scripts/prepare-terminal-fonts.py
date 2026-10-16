#!/usr/bin/env python3
"""Create deterministic, uniquely named PigADE terminal font assets.

Only OpenType naming metadata changes. Upstream files stay untouched in their
versioned directories. Requires the already available developer utility
fontTools 4.63.0; this adds no runtime or Cargo dependency.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile

from fontTools import version as fonttools_version
from fontTools.pens.recordingPen import RecordingPen
from fontTools.ttLib import TTFont


ROOT = Path(__file__).resolve().parents[1]
FONTS = ROOT / "crates/app/assets/fonts"
DEFAULT_OUTPUT = FONTS / "derived-terminal-v1"
EXPECTED_FONTTOOLS = "4.63.0"
PROFILE_ID = "pigade-terminal-render-profile-aliases-v1"

FONTS_TO_DERIVE = (
    {
        "source_manifest": FONTS / "jetbrains-mono-v2.304/asset-manifest.json",
        "source_path": "JetBrainsMono-Regular.ttf",
        "source_family": "JetBrains Mono",
        "family": "PigADE Terminal Mono v1",
        "subfamily": "Regular",
        "postscript": "PigADETerminalMonoV1-Regular",
        "output": "PigADETerminalMonoV1-Regular.ttf",
        "upstream": "JetBrains Mono v2.304",
        "legal_prefix": "JetBrainsMono",
        "license_files": (
            ("OFL.txt", "LICENSE-JetBrainsMono-OFL.txt"),
            ("AUTHORS.txt", "JetBrainsMono-AUTHORS.txt"),
        ),
    },
    {
        "source_manifest": FONTS / "jetbrains-mono-v2.304/asset-manifest.json",
        "source_path": "JetBrainsMono-Bold.ttf",
        "source_family": "JetBrains Mono",
        "family": "PigADE Terminal Mono v1",
        "subfamily": "Bold",
        "postscript": "PigADETerminalMonoV1-Bold",
        "output": "PigADETerminalMonoV1-Bold.ttf",
        "upstream": "JetBrains Mono v2.304",
        "legal_prefix": "JetBrainsMono",
        "license_files": (),
    },
    {
        "source_manifest": FONTS / "jetbrains-mono-v2.304/asset-manifest.json",
        "source_path": "JetBrainsMono-Italic.ttf",
        "source_family": "JetBrains Mono",
        "family": "PigADE Terminal Mono v1",
        "subfamily": "Italic",
        "postscript": "PigADETerminalMonoV1-Italic",
        "output": "PigADETerminalMonoV1-Italic.ttf",
        "upstream": "JetBrains Mono v2.304",
        "legal_prefix": "JetBrainsMono",
        "license_files": (),
    },
    {
        "source_manifest": FONTS / "jetbrains-mono-v2.304/asset-manifest.json",
        "source_path": "JetBrainsMono-BoldItalic.ttf",
        "source_family": "JetBrains Mono",
        "family": "PigADE Terminal Mono v1",
        "subfamily": "Bold Italic",
        "postscript": "PigADETerminalMonoV1-BoldItalic",
        "output": "PigADETerminalMonoV1-BoldItalic.ttf",
        "upstream": "JetBrains Mono v2.304",
        "legal_prefix": "JetBrainsMono",
        "license_files": (),
    },
    {
        "source_manifest": FONTS / "noto-sans-mono-cjk-jp-v2.004/asset-manifest.json",
        "source_path": "NotoSansMonoCJKjp-Regular.otf",
        "source_family": "Noto Sans Mono CJK JP",
        "family": "PigADE Terminal CJK JP v1",
        "subfamily": "Regular",
        "postscript": "PigADETerminalCJKJPV1-Regular",
        "output": "PigADETerminalCJKJPV1-Regular.otf",
        "upstream": "Noto Sans Mono CJK JP v2.004",
        "legal_prefix": "NotoSansCJK",
        "license_files": (
            ("OFL.txt", "LICENSE-NotoSansCJK-OFL.txt"),
        ),
    },
)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def json_sha256(value: object) -> str:
    raw = json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
    return sha256(raw.encode("ascii"))


def source_info(spec: dict) -> tuple[Path, str, str]:
    manifest_path = spec["source_manifest"]
    manifest_raw = manifest_path.read_bytes()
    manifest = json.loads(manifest_raw)
    entry = next(
        (item for item in manifest["files"] if item["path"] == spec["source_path"]),
        None,
    )
    if entry is None:
        raise ValueError(f"{spec['source_path']} is not listed in {manifest_path}")
    source = manifest_path.parent / spec["source_path"]
    source_raw = source.read_bytes()
    if len(source_raw) != entry["bytes"] or sha256(source_raw) != entry["sha256"]:
        raise ValueError(f"source font does not match its manifest: {source}")
    return source, sha256(manifest_raw), sha256(source_raw)


def outline_fingerprint(font: TTFont) -> str:
    glyph_set = font.getGlyphSet()
    digest = hashlib.sha256()
    for glyph_name in font.getGlyphOrder():
        pen = RecordingPen()
        glyph_set[glyph_name].draw(pen)
        item = json.dumps(
            [glyph_name, pen.value], ensure_ascii=True, separators=(",", ":")
        ).encode("ascii")
        digest.update(len(item).to_bytes(8, "big"))
        digest.update(item)
    return digest.hexdigest()


def metrics_fingerprint(font: TTFont) -> str:
    glyph_order = font.getGlyphOrder()
    metrics: dict[str, object] = {
        "unitsPerEm": font["head"].unitsPerEm,
        "glyphOrder": glyph_order,
        "hmtx": [(name, font["hmtx"].metrics[name]) for name in glyph_order],
        "hhea": {
            name: getattr(font["hhea"], name)
            for name in ("ascent", "descent", "lineGap", "numberOfHMetrics")
        },
        "OS/2": {
            name: getattr(font["OS/2"], name)
            for name in (
                "xAvgCharWidth",
                "sTypoAscender",
                "sTypoDescender",
                "sTypoLineGap",
                "usWinAscent",
                "usWinDescent",
                "sxHeight",
                "sCapHeight",
            )
            if hasattr(font["OS/2"], name)
        },
    }
    if "vmtx" in font:
        metrics["vmtx"] = [(name, font["vmtx"].metrics[name]) for name in glyph_order]
    if "vhea" in font:
        metrics["vhea"] = {
            name: getattr(font["vhea"], name)
            for name in ("ascent", "descent", "lineGap", "numberOfVMetrics")
        }
    return json_sha256(metrics)


def cmap_fingerprint(font: TTFont) -> str:
    cmap = sorted(font.getBestCmap().items())
    return json_sha256(cmap)


def set_name_record(record, value: str) -> None:
    record.string = value.encode(record.getEncoding(), errors="replace")


def set_family_names(font: TTFont, spec: dict) -> None:
    family = spec["family"]
    subfamily = spec["subfamily"]
    full_name = f"{family} {subfamily}"
    postscript = spec["postscript"]
    unique_id = f"PigADE Terminal Render v1; {family}; {subfamily}"
    names = font["name"]
    values = {
        1: family,   # legacy family
        2: subfamily,
        3: unique_id,
        4: full_name,
        6: postscript,
        16: family,  # typographic family (preferred by fontdb)
        17: subfamily,
        18: full_name,
        21: family,  # WWS family
        22: subfamily,
        25: postscript.removesuffix("-" + subfamily.replace(" ", "")),
    }
    existing_ids = {record.nameID for record in names.names}
    for record in names.names:
        if record.nameID in values:
            set_name_record(record, values[record.nameID])
    # Some source faces omit typographic/WWS IDs. Add English Unicode records
    # so platform text databases see the same alias regardless of name-ID path.
    for name_id, value in values.items():
        if name_id in (25,) or name_id in existing_ids:
            continue
        names.setName(value, name_id, 3, 1, 0x409)

    if "CFF " in font:
        cff = font["CFF "].cff
        cff.fontNames = [postscript]
        top = cff.topDictIndex[0]
        top.FamilyName = family
        top.FullName = full_name
        # Non-CID CFF faces expose FontName on the top dict. The JP source is
        # CID-keyed and instead uses the CFF Name INDEX plus FamilyName/FullName.
        if hasattr(top, "FontName"):
            top.FontName = postscript
        if hasattr(top, "CIDFontName"):
            top.CIDFontName = postscript


def verify_alias(font: TTFont, spec: dict) -> None:
    name = font["name"]
    for name_id in (1, 16, 21):
        aliases = {
            record.toUnicode()
            for record in name.names
            if record.nameID == name_id and record.isUnicode()
        }
        if aliases != {spec["family"]}:
            raise ValueError(f"name ID {name_id} has unexpected family names: {aliases}")
    postscript_names = {
        record.toUnicode()
        for record in name.names
        if record.nameID == 6 and record.isUnicode()
    }
    if postscript_names != {spec["postscript"]}:
        raise ValueError(f"unexpected PostScript names: {postscript_names}")
    if "CFF " in font:
        cff = font["CFF "].cff
        if cff.fontNames != [spec["postscript"]]:
            raise ValueError(f"unexpected CFF Name INDEX: {cff.fontNames}")
        top = cff.topDictIndex[0]
        if top.FamilyName != spec["family"] or top.FullName != f"{spec['family']} {spec['subfamily']}":
            raise ValueError("CFF family/full name aliases did not persist")


def transform(spec: dict, output_path: Path, source_manifest_sha: str, source_sha: str) -> dict:
    source, _, _ = source_info(spec)
    original = TTFont(source, recalcTimestamp=False, recalcBBoxes=False)
    if original["name"].getBestFamilyName() != spec["source_family"]:
        raise ValueError(f"unexpected upstream family in {source}")
    before = {
        "outlines": outline_fingerprint(original),
        "metrics": metrics_fingerprint(original),
        "cmap": cmap_fingerprint(original),
    }
    set_family_names(original, spec)
    removed_tables = []
    if "DSIG" in original:
        # The source Noto OTF carries a zero-signature DSIG table. Even a real
        # signature would no longer authenticate a name-modified font.
        del original["DSIG"]
        removed_tables.append("DSIG")
    output_path.parent.mkdir(parents=True, exist_ok=True)
    original.save(output_path, reorderTables=True)

    derived = TTFont(output_path, recalcTimestamp=False, recalcBBoxes=False)
    verify_alias(derived, spec)
    if "DSIG" in derived:
        raise ValueError(f"stale DSIG table remains in derived font: {output_path}")
    after = {
        "outlines": outline_fingerprint(derived),
        "metrics": metrics_fingerprint(derived),
        "cmap": cmap_fingerprint(derived),
    }
    if before != after:
        changed = [name for name in before if before[name] != after[name]]
        raise ValueError(f"derived font changed non-name data in {output_path}: {changed}")
    raw = output_path.read_bytes()
    return {
        "source": str(source.relative_to(ROOT)),
        "source_manifest": str(spec["source_manifest"].relative_to(ROOT)),
        "source_manifest_sha256": source_manifest_sha,
        "source_sha256": source_sha,
        "upstream_family": spec["source_family"],
        "upstream_release": spec["upstream"],
        "derived_family": spec["family"],
        "subfamily": spec["subfamily"],
        "postscript_name": spec["postscript"],
        "path": spec["output"],
        "removed_tables": removed_tables,
        "bytes": len(raw),
        "sha256": sha256(raw),
        "preservation_fingerprints": before,
    }


def write_derived(output_dir: Path) -> dict:
    if fonttools_version != EXPECTED_FONTTOOLS:
        raise ValueError(
            f"fontTools {EXPECTED_FONTTOOLS} is required, found {fonttools_version}"
        )

    output_dir.mkdir(parents=True, exist_ok=True)
    # These copies were emitted by the first alias-manifest revision, but their
    # internal filenames referenced absent OFL.txt/AUTHORS.txt files. The
    # generated PigADE notice now provides the correct routed legal references.
    for retired_notice in ("JetBrainsMono-NOTICE.txt", "NotoSansCJK-NOTICE.txt"):
        path = output_dir / retired_notice
        if path.exists():
            path.unlink()
    derived_files = [transform(spec, output_dir / spec["output"], *source_info(spec)[1:]) for spec in FONTS_TO_DERIVE]

    legal_files = []
    seen_legal: set[str] = set()
    for spec in FONTS_TO_DERIVE:
        source_manifest = spec["source_manifest"]
        for source_name, output_name in spec["license_files"]:
            if output_name in seen_legal:
                continue
            source_path = source_manifest.parent / source_name
            target = output_dir / output_name
            shutil.copyfile(source_path, target)
            raw = target.read_bytes()
            legal_files.append(
                {
                    "source": str(source_path.relative_to(ROOT)),
                    "path": output_name,
                    "bytes": len(raw),
                    "sha256": sha256(raw),
                }
            )
            seen_legal.add(output_name)

    source_notices = []
    for source_manifest in sorted({spec["source_manifest"] for spec in FONTS_TO_DERIVE}):
        source_path = source_manifest.parent / "NOTICE.txt"
        raw = source_path.read_bytes()
        source_notices.append(
            {
                "source": str(source_path.relative_to(ROOT)),
                "bytes": len(raw),
                "sha256": sha256(raw),
            }
        )

    notice_text = """PigADE terminal font assets — derived redistribution notice

These files are derived from the upstream font files listed in
derived-font-manifest.json. The upstream source files, licenses, and notices
remain unchanged in their versioned source directories.

The derived font files change OpenType family, subfamily, full, unique, WWS,
PostScript, and (for CFF) name strings to PigADE-only aliases. The Noto Sans
Mono CJK JP OTF also had its empty, unsigned DSIG table removed. Glyph outlines,
metrics, and cmap are fingerprinted as unchanged in the manifest.

JetBrains Mono assets are redistributed under LICENSE-JetBrainsMono-OFL.txt;
their upstream authors are listed in JetBrainsMono-AUTHORS.txt. The Noto Sans
Mono CJK JP asset is redistributed under LICENSE-NotoSansCJK-OFL.txt. Its
upstream copyright and package attribution are recorded in the Noto source
NOTICE.txt identified by source_notices in the manifest.
"""
    notice_path = output_dir / "PigADE-Terminal-NOTICE.txt"
    notice_path.write_text(notice_text)
    notice_raw = notice_path.read_bytes()
    legal_files.append(
        {
            "source": "generated by scripts/prepare-terminal-fonts.py",
            "path": notice_path.name,
            "bytes": len(notice_raw),
            "sha256": sha256(notice_raw),
        }
    )

    manifest = {
        "schema": 1,
        "profile_id": PROFILE_ID,
        "status": "derived family aliases; stale DSIG removed where present; glyph outlines, metrics, and cmap verified unchanged",
        "generator": {
            "path": "scripts/prepare-terminal-fonts.py",
            "fontTools_version": fonttools_version,
            "command": "python3 scripts/prepare-terminal-fonts.py --output DIR",
            "reproducibility": "generate into two empty directories and compare file SHA-256 values; --verify checks a fresh generation against this directory",
        },
        "policy": {
            "upstream_assets_untouched": True,
            "glyphs_metrics_and_cmap_modified": False,
            "stale_dsig_tables_removed": True,
            "derived_changes": [
                "OpenType identity names changed to PigADE-only aliases",
                "empty or stale DSIG tables removed when present",
            ],
            "family_aliases": sorted({spec["family"] for spec in FONTS_TO_DERIVE}),
            "reserved_upstream_family_names_used_by_derived_fonts": False,
            "primary_alias": "PigADE Terminal Mono v1",
            "jp_cjk_alias": "PigADE Terminal CJK JP v1",
        },
        "files": derived_files,
        "legal_files": legal_files,
        "source_notices": source_notices,
    }
    manifest_path = output_dir / "derived-font-manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    return manifest


def verify_existing(output_dir: Path) -> None:
    if not output_dir.is_dir():
        raise ValueError(f"derived output directory does not exist: {output_dir}")
    with tempfile.TemporaryDirectory(prefix="pigade-font-verify-") as temp:
        expected = write_derived(Path(temp))
        expected_names = {item["path"] for item in expected["files"]}
        expected_names.update(item["path"] for item in expected["legal_files"])
        expected_names.add("derived-font-manifest.json")
        actual_names = {path.name for path in output_dir.iterdir() if path.is_file()}
        if actual_names != expected_names:
            raise ValueError(
                f"derived directory file set differs: expected={sorted(expected_names)}, "
                f"actual={sorted(actual_names)}"
            )
        for name in sorted(expected_names):
            if (output_dir / name).read_bytes() != (Path(temp) / name).read_bytes():
                raise ValueError(f"derived output is stale or not reproducible: {name}")
    print(f"verified reproducible derived terminal fonts in {output_dir}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument(
        "--verify",
        action="store_true",
        help="regenerate in a temporary directory and compare every output byte",
    )
    args = parser.parse_args()
    output_dir = args.output.resolve()
    try:
        if args.verify:
            verify_existing(output_dir)
        else:
            manifest = write_derived(output_dir)
            print(f"generated {len(manifest['files'])} derived fonts in {output_dir}")
            for item in manifest["files"]:
                print(f"{item['path']}: {item['bytes']} bytes sha256={item['sha256']}")
        return 0
    except (OSError, ValueError, KeyError, StopIteration) as error:
        print(f"prepare-terminal-fonts: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

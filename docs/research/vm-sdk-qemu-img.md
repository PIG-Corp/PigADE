# Pinned QEMU image utility for corpus VM runs

The corpus-backed reboot harness needs `qemu-img` to create and inspect its
private 64 GiB secondary qcow2 disk. Arch distributes it as the separate
`qemu-img` split package. The official Arch package page identifies version
11.1.2-1 for x86_64/Extra, its image-tool role, package license declarations,
packager, and signed package date: [Arch qemu-img package](https://archlinux.org/packages/extra/x86_64/qemu-img/).
The [Arch qemu-img manual](https://man.archlinux.org/man/qemu-img.1.en) documents
the offline image create/info commands used by the harness.

The pinned archive is
[`qemu-img-11.1.2-1-x86_64.pkg.tar.zst`](https://geo.mirror.pkgbuild.com/extra/os/x86_64/qemu-img-11.1.2-1-x86_64.pkg.tar.zst)
with detached signature
[`qemu-img-11.1.2-1-x86_64.pkg.tar.zst.sig`](https://geo.mirror.pkgbuild.com/extra/os/x86_64/qemu-img-11.1.2-1-x86_64.pkg.tar.zst.sig).
Its SHA-256 is
`666dfe6cda552f09091c1cc03fd1143293bd25f47eee4745053efa170fc32e5c`.
The signature verifies against the local Arch keyring after private dearmoring;
the recorded signer is David Runge, primary fingerprint
`991F6E3F0765CF6295888586139B09DA5BF0D338` (package-signing subkey
`62CC73F884E52957B2FDD8839B7A287D9A2EC608`). The verification output is
retained in the SDK manifest. Arch's `.PKGINFO` license declarations are
preserved there along with the exact dependency metadata; this does not replace
retaining upstream license and notice files with the extracted package.

`scripts/prepare-vm-sdk.py` now pins and verifies this package using the same
bounded download, hash, signature, safe extraction, and dependency-closure
checks as the other SDK packages. It never installs packages or executes their
installer hooks. The prepared SDK was written to a new private root, leaving
the prior retained SDK tree and manifest unchanged:

- SDK manifest: `/tmp/pigade-vm-sdk-qemu-img-20261017/evidence/manifest.json`
- Extracted content-addressed root: `root/qemu-77e330878a56f786`
- Package count: 14; downloaded archives and signatures: 16,372,721 bytes
- `qemu-img --version`: `qemu-img version 11.1.2`
- `qemu-img` dependency closure: no unresolved libraries
- QEMU system emulator and mtools versions: 11.1.2 and 4.0.49

The SDK remains host-ABI dependent as recorded by the manifest's `ldd_checks`;
this is a private test toolchain, not a portable binary bundle. The SDK step
does not create a disk image or start a VM. Pass the new manifest explicitly to
`scripts/vm-reboot.py` with `--sdk-manifest` when preparing corpus-mode inputs.

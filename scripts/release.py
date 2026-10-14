"""Version checks and portable release archives; uses the Python standard library."""

import argparse
import gzip
import hashlib
import os
from pathlib import Path
import subprocess
import struct
import tarfile
import tomllib


PROJECT = Path(__file__).resolve().parents[1]
TARGETS = ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl")


def version(project=PROJECT):
    return tomllib.loads((project / "Cargo.toml").read_text())["package"]["version"]


def validate_ref(value, ref, name):
    if ref.startswith("refs/tags/") and name != "v" + value:
        raise ValueError(f"release tag must match Cargo.toml: expected v{value}")


def verify_static_elf(binary, target):
    data = binary.read_bytes()
    machines = {"x86_64-unknown-linux-musl": 62, "aarch64-unknown-linux-musl": 183}
    if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01":
        raise ValueError("release binary must be a little-endian 64-bit ELF")
    kind, machine = struct.unpack_from("<HH", data, 16)
    if kind not in (2, 3) or machine != machines.get(target):
        raise ValueError("ELF architecture does not match the release target")
    offset = struct.unpack_from("<Q", data, 32)[0]
    size, count = struct.unpack_from("<HH", data, 54)
    if size < 56 or not count or offset + size * count > len(data):
        raise ValueError("invalid ELF program-header table")
    for index in range(count):
        fields = struct.unpack_from("<IIQQQQQQ", data, offset + size * index)
        segment, start, length = fields[0], fields[2], fields[5]
        if segment == 3:
            raise ValueError("release binary has a dynamic interpreter")
        if segment == 2:
            if length % 16 or start + length > len(data):
                raise ValueError("invalid ELF dynamic segment")
            for entry in range(start, start + length, 16):
                tag = struct.unpack_from("<q", data, entry)[0]
                if tag == 0:
                    break
                if tag == 1:
                    raise ValueError("release binary has a shared-library dependency")


def package(project, binary, target, destination, epoch):
    if target not in TARGETS:
        raise ValueError("unsupported release target")
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination / f"sj-v{version(project)}-{target}.tar.gz"
    files = {"sj": binary, "README.md": project / "README.md", "README.zh-CN.md": project / "README.zh-CN.md",
             "docs/assets/terminal-preview.svg": project / "docs/assets/terminal-preview.svg"}
    with archive.open("wb") as stream:
        with gzip.GzipFile(filename="", mode="wb", fileobj=stream, mtime=0) as compressed:
            with tarfile.open(mode="w", fileobj=compressed) as bundle:
                for name, source in sorted(files.items()):
                    info = bundle.gettarinfo(str(source), arcname=name)
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = epoch
                    info.mode = 0o755 if name == "sj" else 0o644
                    with source.open("rb") as content:
                        bundle.addfile(info, content)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n")
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("version")
    commands.add_parser("metadata")
    verify = commands.add_parser("verify")
    verify.add_argument("--binary", type=Path, required=True)
    verify.add_argument("--target", choices=TARGETS, required=True)
    archive = commands.add_parser("package")
    archive.add_argument("--binary", type=Path, required=True)
    archive.add_argument("--target", choices=TARGETS, required=True)
    archive.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "version":
        print(version())
    elif args.command == "metadata":
        value = version()
        validate_ref(value, os.environ.get("GITHUB_REF", ""), os.environ.get("GITHUB_REF_NAME", ""))
        print(f"version={value}")
    elif args.command == "verify":
        verify_static_elf(args.binary, args.target)
        print(f"Static ELF verified for {args.target}")
    else:
        epoch = int(subprocess.check_output(["git", "log", "-1", "--format=%ct"], cwd=PROJECT))
        print(package(PROJECT, args.binary, args.target, args.destination, epoch).name)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error

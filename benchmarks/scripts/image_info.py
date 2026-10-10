#!/usr/bin/env python3
"""Record which emulator images a benchmark run measured.

The startup and memory benchmarks run each emulator from its `:latest` tag,
which moves. This writes down what the tag pointed at on the day: the
registry digest, the image ID, the version the image declares in its
`org.opencontainers.image.version` label (when it has one) and its size on
disk.

Usage:
    python3 image_info.py floci=floci/floci:latest ministack=ministackorg/ministack:latest

Prints a JSON object keyed by the names given, with null for any image that
is not present locally.
"""

import json
import subprocess
import sys

VERSION_LABEL = "org.opencontainers.image.version"


def repository(ref):
    """The repository part of an image reference: floci/floci:latest -> floci/floci."""
    name = ref.split("@", 1)[0]
    head, sep, tail = name.rpartition(":")
    # A colon followed by a slash belongs to a registry port, not a tag.
    if sep and "/" not in tail:
        return head
    return name


def describe_image(ref, entry):
    """Summarise one `docker image inspect` entry."""
    labels = (entry.get("Config") or {}).get("Labels") or {}

    # RepoDigests holds one "repo@sha256:..." per repository the image was
    # pulled under. Prefer the one for the repository asked about.
    repo = repository(ref)
    digests = entry.get("RepoDigests") or []
    digest = None
    for candidate in digests:
        name, _, sha = candidate.partition("@")
        if name == repo:
            digest = sha
            break
    if digest is None and digests:
        digest = digests[0].partition("@")[2] or None

    return {
        "ref": ref,
        "id": entry.get("Id"),
        "digest": digest,
        "version": labels.get(VERSION_LABEL) or None,
        "bytes": entry.get("Size"),
    }


def docker_inspect(ref):
    """Return the inspect entry for a local image, or None if it is absent."""
    try:
        result = subprocess.run(
            ["docker", "image", "inspect", ref],
            capture_output=True, text=True, check=False,
        )
    except OSError:
        return None
    if result.returncode != 0:
        return None
    try:
        entries = json.loads(result.stdout)
    except json.JSONDecodeError:
        return None
    return entries[0] if entries else None


def collect(refs, inspect=docker_inspect):
    """Describe each (key, ref) pair, in order, with None for absent images."""
    images = {}
    for key, ref in refs:
        entry = inspect(ref)
        images[key] = describe_image(ref, entry) if entry else None
    return images


def parse_refs(args):
    """Turn ["key=ref", ...] into [(key, ref), ...]."""
    refs = []
    for arg in args:
        key, sep, ref = arg.partition("=")
        if not sep or not key or not ref:
            raise ValueError("expected key=image, got {!r}".format(arg))
        refs.append((key, ref))
    return refs


def main(argv):
    try:
        refs = parse_refs(argv)
    except ValueError as error:
        print("image_info.py: {}".format(error), file=sys.stderr)
        return 2
    json.dump(collect(refs), sys.stdout, indent=2)
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

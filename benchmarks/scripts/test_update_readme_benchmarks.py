#!/usr/bin/env python3
"""Tests for the README updater's container-emulator markers: startup, idle
memory, image size and the record of which images were measured.

Run with: python3 -m unittest discover -s benchmarks/scripts -p 'test_*.py'
"""

import copy
import os
import re
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from update_readme_benchmarks import (
    build_value_mapping,
    classify_key,
    fmt_image_versions,
    replace_markers,
)

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))

MIB = 1024 * 1024
GIB = 1024 * MIB


def startup(target, mean_ms, stddev_ms=10.0):
    return {"target": target, "mode": "cold", "samples": [mean_ms],
            "mean_ms": mean_ms, "stddev_ms": stddev_ms}


def image(version, digest_hex, size):
    return {"ref": "x:latest", "id": "sha256:" + digest_hex, "version": version,
            "digest": "sha256:" + digest_hex, "bytes": size}


# A refresh in which every target was measured.
RUN_SUMMARY = {
    "schema_version": 1,
    "git": {"sha": "0292396" + "0" * 33, "short_sha": "0292396"},
    "system_info": {
        "dynoxide_binary_bytes": 7 * MIB,
        "dynamodb_local_image_bytes": 473 * MIB,
        "localstack_image_bytes": int(1.2 * GIB),
        "floci_image_bytes": 324 * MIB,
        "ministack_image_bytes": 259 * MIB,
        "images": {
            "dynamodb_local": image(None, "ff89bd48ff32" + "0" * 52, 473 * MIB),
            "localstack": image("4.9.2", "1" * 64, int(1.2 * GIB)),
            "floci": image("2.2.0", "2" * 64, 324 * MIB),
            "ministack": image("1.5.25", "3" * 64, 259 * MIB),
        },
    },
    "results": {
        "startup": {
            "results": [
                startup("dynoxide_embedded", 0.2, 0.1),
                startup("dynoxide_http", 1.6, 0.5),
                startup("dynamodb_local", 2831.0, 477.0),
                startup("localstack", 13473.0, 2138.0),
                startup("floci", 146.0, 12.0),
                startup("ministack", 3200.0, 40.0),
            ]
        }
    },
}

MEMORY_ROWS = [
    {"mode": "memory", "step": "db_created", "rss_bytes": str(int(5.3 * MIB))},
    {"mode": "docker_idle", "step": "dynamodb_local", "rss_bytes": str(int(204.8 * MIB))},
    {"mode": "docker_idle", "step": "localstack", "rss_bytes": str(388 * MIB)},
    {"mode": "docker_idle", "step": "floci", "rss_bytes": str(22 * MIB)},
    {"mode": "docker_idle", "step": "ministack", "rss_bytes": str(int(50.4 * MIB))},
]


def without_target(key):
    """The same refresh with one target missing from every result file."""
    summary = copy.deepcopy(RUN_SUMMARY)
    summary["results"]["startup"]["results"] = [
        r for r in summary["results"]["startup"]["results"] if r["target"] != key]
    summary["system_info"].pop("{}_image_bytes".format(key), None)
    summary["system_info"]["images"][key] = None
    rows = [r for r in MEMORY_ROWS if r["step"] != key]
    return summary, rows


class ContainerTargetMarkersTest(unittest.TestCase):
    def setUp(self):
        self.values = build_value_mapping(RUN_SUMMARY, MEMORY_ROWS)

    def test_floci_and_ministack_fill_the_main_readme_table(self):
        self.assertEqual(self.values["ci_startup_floci"], "~146ms")
        self.assertEqual(self.values["ci_startup_ministack"], "~3,200ms")
        self.assertEqual(self.values["ci_memory_floci_idle"], "~22 MB")
        self.assertEqual(self.values["ci_memory_ministack_idle"], "~50 MB")
        self.assertEqual(self.values["ci_image_floci"], "324 MB")
        self.assertEqual(self.values["ci_image_ministack"], "259 MB")

    def test_floci_and_ministack_fill_the_benchmarks_readme_tables(self):
        self.assertEqual(self.values["ci_startup_floci_mean"], "146ms")
        self.assertEqual(self.values["ci_startup_floci_stddev"], "±12ms")
        self.assertEqual(self.values["ci_startup_ministack_mean"], "3,200ms")
        self.assertEqual(self.values["ci_mem_floci_idle"], "22.0 MB")
        self.assertEqual(self.values["ci_mem_ministack_idle"], "50.4 MB")

    def test_ratio_says_faster_when_the_target_beats_dynamodb_local(self):
        # 2,831 / 146 = 19.4. Not bold: bold is kept for Dynoxide's own rows.
        self.assertEqual(self.values["ci_startup_floci_ratio"], "~19x faster")

    def test_ratio_says_slower_when_the_target_trails_dynamodb_local(self):
        self.assertEqual(self.values["ci_startup_ministack_ratio"], "1.1x slower")

    def test_localstack_markers_are_unchanged(self):
        self.assertEqual(self.values["ci_startup_localstack"], "~13,473ms")
        self.assertEqual(self.values["ci_startup_localstack_mean"], "13,473ms")
        self.assertEqual(self.values["ci_startup_localstack_stddev"], "±2,138ms")
        self.assertEqual(self.values["ci_startup_localstack_ratio"], "4.8x slower")
        self.assertEqual(self.values["ci_memory_localstack_idle"], "~388 MB")
        self.assertEqual(self.values["ci_mem_localstack_idle"], "388.0 MB")
        self.assertEqual(self.values["ci_image_localstack"], "1.2 GB")

    def test_dynamodb_local_markers_are_unchanged(self):
        self.assertEqual(self.values["ci_startup_ddb_local"], "~2,831ms")
        self.assertEqual(self.values["ci_memory_ddb_local_idle"], "~205 MB")
        self.assertEqual(self.values["ci_image_ddb_local"], "473 MB")

    def test_image_over_a_gibibyte_is_shown_in_gb(self):
        summary = copy.deepcopy(RUN_SUMMARY)
        summary["system_info"]["ministack_image_bytes"] = int(1.5 * GIB)
        values = build_value_mapping(summary, MEMORY_ROWS)
        self.assertEqual(values["ci_image_ministack"], "1.5 GB")

    def test_a_target_with_no_results_produces_no_values(self):
        # The README then keeps whatever the cell held, rather than a number
        # the run never produced.
        for key in ("floci", "ministack"):
            summary, rows = without_target(key)
            values = build_value_mapping(summary, rows)
            leaked = sorted(k for k in values if key in k and k != "ci_image_versions")
            self.assertEqual(leaked, [], key)

    def test_ratio_needs_dynamodb_local(self):
        summary, rows = without_target("dynamodb_local")
        values = build_value_mapping(summary, rows)
        self.assertEqual(values["ci_startup_floci"], "~146ms")
        self.assertNotIn("ci_startup_floci_ratio", values)


class ImageVersionsTest(unittest.TestCase):
    def test_lists_each_measured_image(self):
        text = fmt_image_versions(RUN_SUMMARY["system_info"])
        self.assertEqual(
            text,
            "DynamoDB Local `ff89bd48ff32`, LocalStack 4.9.2, Floci 2.2.0, MiniStack 1.5.25",
        )

    def test_skips_images_the_run_did_not_have(self):
        summary, _ = without_target("localstack")
        text = fmt_image_versions(summary["system_info"])
        self.assertNotIn("LocalStack", text)
        self.assertIn("Floci 2.2.0", text)

    def test_falls_back_to_the_image_id_without_a_digest(self):
        info = copy.deepcopy(RUN_SUMMARY["system_info"])
        info["images"]["dynamodb_local"]["digest"] = None
        info["images"]["dynamodb_local"]["id"] = "sha256:abcdef123456" + "0" * 52
        self.assertIn("DynamoDB Local `abcdef123456`", fmt_image_versions(info))

    def test_absent_without_any_image_record(self):
        self.assertIsNone(fmt_image_versions({}))
        self.assertIsNone(fmt_image_versions({"images": {"floci": None}}))

    def test_is_in_the_mapping_and_not_sanity_checked(self):
        values = build_value_mapping(RUN_SUMMARY, MEMORY_ROWS)
        self.assertIn("Floci 2.2.0", values["ci_image_versions"])
        self.assertEqual(classify_key("ci_image_versions"), "skip")


class PendingPlaceholderTest(unittest.TestCase):
    def test_pending_cell_is_replaced_on_the_first_refresh(self):
        content = ("| Cold startup | <!-- bench:ci_startup_floci -->pending<!-- /bench --> |\n"
                   "| Idle | <!-- bench:ci_memory_ministack_idle -->pending<!-- /bench --> |\n")
        values = build_value_mapping(RUN_SUMMARY, MEMORY_ROWS)
        new, stats = replace_markers(content, values, "README.md")
        self.assertIn("-->~146ms<!--", new)
        self.assertIn("-->~50 MB<!--", new)
        self.assertEqual(stats["updated"], 2)
        self.assertEqual(stats["skipped"], 0)

    def test_pending_cell_stays_when_the_target_was_not_measured(self):
        content = "<!-- bench:ci_startup_floci -->pending<!-- /bench -->"
        summary, rows = without_target("floci")
        new, stats = replace_markers(content, build_value_mapping(summary, rows), "README.md")
        self.assertEqual(new, content)
        self.assertEqual(stats["missing"], 1)


class ReadmeMarkersTest(unittest.TestCase):
    """Every Floci and MiniStack marker in the real READMEs must have a value
    to receive, or a refresh would leave it on its placeholder for good."""

    NEW_MARKER = re.compile(r"<!-- bench:(\w*(?:floci|ministack)\w*|ci_image_versions) -->")

    def test_new_markers_in_both_readmes_are_mapped(self):
        values = build_value_mapping(RUN_SUMMARY, MEMORY_ROWS)
        found = []
        for name in ("README.md", os.path.join("benchmarks", "README.md")):
            with open(os.path.join(REPO_ROOT, name), encoding="utf-8") as handle:
                keys = self.NEW_MARKER.findall(handle.read())
            found.extend(keys)
            unmapped = sorted(set(k for k in keys if k not in values))
            self.assertEqual(unmapped, [], name)
        # Guard against the pattern matching nothing at all, which would pass
        # the loop above without checking a single marker.
        for expected in ("ci_startup_floci", "ci_startup_ministack", "ci_memory_floci_idle",
                         "ci_image_ministack", "ci_startup_floci_ratio", "ci_mem_ministack_idle",
                         "ci_image_versions"):
            self.assertIn(expected, found)


if __name__ == "__main__":
    unittest.main()

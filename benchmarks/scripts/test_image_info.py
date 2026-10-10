#!/usr/bin/env python3
"""Tests for image_info: what a refresh records about each emulator image.

Run with: python3 -m unittest discover -s benchmarks/scripts -p 'test_*.py'
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from image_info import collect, describe_image, parse_refs


# Trimmed from `docker image inspect floci/floci:latest` on the classic image
# store, where Id is the config digest and RepoDigests names the manifest the
# tag pointed at when it was pulled.
FLOCI = {
    "Id": "sha256:e97cd0c1dc2a8d2f6c1c5f0e2b1f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f",
    "RepoTags": ["floci/floci:latest"],
    "RepoDigests": [
        "floci/floci@sha256:18b0f4ca311f130f5c3beb6d63f13fda3b818b6b28f987cefd210019a987a818"
    ],
    "Size": 324000000,
    "Config": {
        "Labels": {
            "org.opencontainers.image.version": "2.2.0",
            "org.opencontainers.image.revision": "17c99047b92b5729b356f3584513cd6ed598b8a9",
        }
    },
}

# DynamoDB Local carries no version label at all, only the SDK it was built on.
DYNAMODB_LOCAL = {
    "Id": "sha256:ff89bd48ff32cd8d9be5fee8873b65b8854dc408f1afe881be6eb00247bc0dab",
    "RepoDigests": [
        "amazon/dynamodb-local@sha256:ff89bd48ff32cd8d9be5fee8873b65b8854dc408f1afe881be6eb00247bc0dab"
    ],
    "Size": 811167790,
    "Config": {"Labels": {"aws.java.sdk.version": "3.3.1"}},
}


class DescribeImageTest(unittest.TestCase):
    def test_reads_version_digest_id_and_size(self):
        info = describe_image("floci/floci:latest", FLOCI)
        self.assertEqual(info["ref"], "floci/floci:latest")
        self.assertEqual(info["version"], "2.2.0")
        self.assertEqual(
            info["digest"],
            "sha256:18b0f4ca311f130f5c3beb6d63f13fda3b818b6b28f987cefd210019a987a818",
        )
        self.assertEqual(info["id"], FLOCI["Id"])
        self.assertEqual(info["bytes"], 324000000)

    def test_version_is_none_without_the_oci_label(self):
        info = describe_image("amazon/dynamodb-local:latest", DYNAMODB_LOCAL)
        self.assertIsNone(info["version"])
        # The digest is still recorded, so the run says what it measured.
        self.assertTrue(info["digest"].startswith("sha256:ff89bd48ff32"))

    def test_null_labels_do_not_break_the_lookup(self):
        # Docker reports "Labels": null for an image built with none.
        entry = dict(DYNAMODB_LOCAL, Config={"Labels": None})
        self.assertIsNone(describe_image("x:latest", entry)["version"])
        entry = dict(DYNAMODB_LOCAL, Config=None)
        self.assertIsNone(describe_image("x:latest", entry)["version"])

    def test_locally_built_image_has_no_digest(self):
        # An image that was built rather than pulled has no RepoDigests.
        entry = dict(FLOCI, RepoDigests=[])
        info = describe_image("floci/floci:latest", entry)
        self.assertIsNone(info["digest"])
        self.assertEqual(info["version"], "2.2.0")

    def test_digest_comes_from_the_matching_repository(self):
        # An image pulled under two names lists a digest for each.
        entry = dict(
            FLOCI,
            RepoDigests=[
                "hectorvent/floci@sha256:" + "a" * 64,
                "floci/floci@sha256:" + "b" * 64,
            ],
        )
        info = describe_image("floci/floci:latest", entry)
        self.assertEqual(info["digest"], "sha256:" + "b" * 64)


class CollectTest(unittest.TestCase):
    def test_absent_image_is_recorded_as_none(self):
        found = {"floci/floci:latest": FLOCI}
        images = collect(
            [("floci", "floci/floci:latest"), ("localstack", "localstack/localstack:latest")],
            inspect=found.get,
        )
        self.assertEqual(images["floci"]["version"], "2.2.0")
        self.assertIn("localstack", images)
        self.assertIsNone(images["localstack"])

    def test_keys_follow_the_requested_order(self):
        found = {"a:latest": FLOCI, "b:latest": DYNAMODB_LOCAL}
        images = collect([("b", "b:latest"), ("a", "a:latest")], inspect=found.get)
        self.assertEqual(list(images), ["b", "a"])


class ParseRefsTest(unittest.TestCase):
    def test_splits_key_and_ref_on_the_first_equals(self):
        self.assertEqual(
            parse_refs(["floci=floci/floci:latest", "ddb=amazon/dynamodb-local:latest"]),
            [("floci", "floci/floci:latest"), ("ddb", "amazon/dynamodb-local:latest")],
        )

    def test_rejects_an_argument_without_a_key(self):
        with self.assertRaises(ValueError):
            parse_refs(["floci/floci:latest"])


if __name__ == "__main__":
    unittest.main()

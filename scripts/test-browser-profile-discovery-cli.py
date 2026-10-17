#!/usr/bin/env python3
"""Test the shipped CLI formatter using synthetic API replies; no server or browser."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

SOURCE = Path(__file__).resolve().parents[1] / "amux"


def formatter():
    source = SOURCE.read_text()
    start = source.index('      RESP="$resp" ME="$me" ALL="${1:-}" python3 - ')
    return source[start:].split("\n", 1)[1].split("\nPYEOF", 1)[0]


class DiscoveryCliTests(unittest.TestCase):
    def render(self, reply, all_rows=""):
        env = dict(os.environ, RESP=json.dumps(reply), ME="synthetic-worker", ALL=all_rows)
        return subprocess.run([sys.executable, "-c", formatter()], env=env,
                              capture_output=True, text=True, check=False)

    def test_permission_failure_never_prints_zero_discovered(self):
        for flag in ("", "--all"):
            with self.subTest(flag=flag):
                result = self.render({"profiles": [], "discovery": {
                    "measured": False, "n_considered": 0,
                    "why_unmeasured": "permission_denied: /synthetic/Local State",
                    "owner_step": "Grant the amux server Full Disk Access."},
                    "saved_discovery": {"measured": True, "n_considered": 0,
                                        "empty_reason": "no_saved_profiles_configured"}}, flag)
                self.assertIn("permission_denied: /synthetic/Local State", result.stdout)
                self.assertIn("Owner step: Grant the amux server Full Disk Access.", result.stdout)
                self.assertNotIn("0 profile(s) discovered", result.stdout)
                self.assertIn("Profile discovery incomplete", result.stdout)
                self.assertEqual(result.returncode, 1)

    def test_missing_parse_and_registry_failures_are_visible(self):
        for key in ("discovery", "saved_discovery"):
            for reason in ("not_found", "parse_failure", "permission_denied"):
                with self.subTest(key=key, reason=reason):
                    reply = {"profiles": [], "discovery": {"measured": True},
                             "saved_discovery": {"measured": True}}
                    reply[key] = {"measured": False, "why_unmeasured": reason + ": /synthetic"}
                    result = self.render(reply)
                    self.assertIn(reason, result.stdout)
                    self.assertEqual(result.returncode, 1)

    def test_saved_and_chrome_inventories_are_distinct(self):
        result = self.render({"profiles": [], "chrome_profiles": ["Default", "Profile 1"],
                              "discovery": {"measured": True, "n_considered": 2},
                              "saved_discovery": {"measured": True, "n_considered": 0,
                                                  "empty_reason": "no_saved_profiles_configured"}})
        self.assertEqual(result.returncode, 0)
        self.assertIn("no_saved_profiles_configured", result.stdout)
        self.assertIn("0 saved Amux profile(s) discovered", result.stdout)
        self.assertIn("2 Chrome profile(s) discovered separately", result.stdout)

    def test_scope_denied_rows_remain_discoverable(self):
        result = self.render({"profiles": [{"name": "synthetic", "access": {
            "allowed_for_you": False}}], "discovery": {"measured": True},
            "saved_discovery": {"measured": True}})
        self.assertEqual(result.returncode, 0)
        self.assertIn("x synthetic", result.stdout)
        self.assertIn("1 saved Amux profile(s) discovered", result.stdout)
        self.assertIn("0 shown and allowed", result.stdout)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=SOURCE, help="CLI source to verify")
    args = parser.parse_args()
    SOURCE = args.source
    unittest.main(argv=[sys.argv[0]])

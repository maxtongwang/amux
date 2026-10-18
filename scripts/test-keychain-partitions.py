#!/usr/bin/env python3
"""Exercise keychain dump parsing without invoking macOS keychain tools."""

import contextlib
import importlib.util
import io
import pathlib


ROOT = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("keychain_partitions", ROOT / "keychain-partitions.py")
parser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(parser)
dump = (ROOT / "fixtures/keychain-partitions.dump").read_text()

assert parser.partition_for_label("unrelated CI signing key", dump) == "apple-tool:,apple:"
assert parser.partition_for_label("amux local code signing", dump) == "apple-tool:,apple:,codesign:"

stderr = io.StringIO()
with contextlib.redirect_stderr(stderr):
    try:
        parser.partition_for_label("unrelated CI signing key", dump + dump)
    except ValueError:
        pass
    else:
        raise AssertionError("duplicate partition ACL did not fail")
assert "matching dump block:" in stderr.getvalue()
assert "raw ACL:" in stderr.getvalue()

without_unrelated_acl = dump.replace("authorizations (1): partition_id", "authorizations (1): sign", 1)
stderr = io.StringIO()
with contextlib.redirect_stderr(stderr):
    try:
        parser.partition_for_label("unrelated CI signing key", without_unrelated_acl)
    except ValueError:
        pass
    else:
        raise AssertionError("partition ACL leaked from the next item")
assert "found 0" in stderr.getvalue()

print("PASS: two labeled keys, hex XML and plain ACLs, duplicate and missing ACL diagnostics")

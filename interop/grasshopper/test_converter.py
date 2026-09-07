#!/usr/bin/env python3
"""Verify genuine GH binary conversion using a McNeel-authored fixture."""
import argparse
import hashlib
from pathlib import Path
import subprocess
import tempfile
import urllib.request
import xml.etree.ElementTree as ET

FIXTURE = "https://media.githubusercontent.com/media/mcneel/rhinocodetests/rhino-9.x/_temp/tests_rhinogh/code.gh"
SHA256 = "434d048ec7a69928f7ff5433751d6b6f392199336159929b6ea19d6051d760b5"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--helper", required=True)
    parser.add_argument("--ghx", type=Path, help="Also test a Mantis-produced GHX file")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="mantis-gh-test-") as temp:
        folder = Path(temp)
        original = folder / "mcneel.gh"
        original.write_bytes(urllib.request.urlopen(FIXTURE, timeout=45).read())
        assert hashlib.sha256(original.read_bytes()).hexdigest() == SHA256
        assert not original.read_bytes().startswith(b"<")
        decoded = subprocess.check_output([args.helper, "decode", str(original)], timeout=45)
        root = ET.fromstring(decoded)
        assert root.tag == "Archive"
        assert root.find(".//chunk[@name='DefinitionObjects']") is not None
        for index, source in enumerate([decoded] + ([args.ghx.read_bytes()] if args.ghx else [])):
            binary = folder / f"roundtrip-{index}.gh"
            subprocess.run([args.helper, "encode", str(binary)], input=source, check=True, timeout=45)
            assert not binary.read_bytes().startswith(b"<")
            back = subprocess.check_output([args.helper, "decode", str(binary)], timeout=45)
            # These identities and data must survive actual binary conversion.
            before = ET.fromstring(source)
            after = ET.fromstring(back)
            for name in ["GUID", "InstanceGuid", "Source", "Value", "ObjectCount"]:
                values = lambda xml: sorted((i.get("index", ""), (i.text or "").strip()) for i in xml.findall(f".//item[@name='{name}']"))
                assert values(before) == values(after), name
            again = subprocess.run([args.helper, "encode", str(binary)], input=source, capture_output=True)
            assert again.returncode != 0, "Existing exports must not be overwritten"
            assert subprocess.check_output([args.helper, "decode", str(binary)], timeout=45) == back
            assert not list(folder.glob(".mantis-temp-*.gh")), "Atomic export left a temporary file"
        bad = folder / "broken.gh"
        bad.write_bytes(b"not a Grasshopper binary archive")
        assert subprocess.run([args.helper, "decode", str(bad)], capture_output=True).returncode != 0
        destination = folder / "invalid-export.gh"
        assert subprocess.run([args.helper, "encode", str(destination)], input=b"<not-an-archive>", capture_output=True).returncode != 0
        assert not destination.exists()
        assert not list(folder.glob(".mantis-temp-*.gh"))
    print("PASS: real McNeel GH decode, binary encode/decode, identity/wire/value preservation, no overwrite, malformed input")


if __name__ == "__main__":
    main()

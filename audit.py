#!/usr/bin/env python3
"""Check every crate in Cargo.lock against the OSV vulnerability database
(which includes the RustSec advisories). Run with `make audit`.

Prints each advisory with its summary and the first fixed version, if any.
"Unmaintained" and "unsound" notices are informational. Exits 1 if any
advisory not in ACCEPTED is found.
"""

import json
import sys
import tomllib
import urllib.request

OSV = "https://api.osv.dev/v1"

# Advisories looked at and accepted, with why. Remove an entry once it no
# longer shows up.
ACCEPTED = {
    "RUSTSEC-2026-0196": "cgmath unmaintained; used inside smithay",
    "RUSTSEC-2026-0197": "cgmath swap_columns; neither we nor smithay call it with equal indices",
    "RUSTSEC-2026-0192": "ttf-parser unmaintained; used by fontdue for our one bundled font",
    "RUSTSEC-2026-0370": "zvariant merged into zbus 6, which drops the blocking API we use",
    "RUSTSEC-2026-0371": "zbus_names merged into zbus 6 (as above)",
    "RUSTSEC-2026-0372": "zvariant_derive merged into zbus 6 (as above)",
    "RUSTSEC-2026-0459": "about bundled SQLite; we link the system's (3.54+ has the fix)",
    "RUSTSEC-2026-0486": "rustix xattr calls, which nothing we build uses; 1.1.6 fixes it",
}


def post(url, body):
    req = urllib.request.Request(
        url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"}
    )
    return json.load(urllib.request.urlopen(req, timeout=60))


def get(url):
    return json.load(urllib.request.urlopen(url, timeout=60))


def main():
    with open("Cargo.lock", "rb") as f:
        lock = tomllib.load(f)
    pkgs = [p for p in lock["package"] if p.get("source", "").startswith("registry+")]
    queries = [
        {"package": {"name": p["name"], "ecosystem": "crates.io"}, "version": p["version"]}
        for p in pkgs
    ]
    results = post(f"{OSV}/querybatch", {"queries": queries})["results"]
    found = accepted = 0
    for p, r in zip(pkgs, results):
        for v in r.get("vulns", []):
            if v["id"] in ACCEPTED:
                accepted += 1
                print(f"{p['name']} {p['version']}: {v['id']} accepted: {ACCEPTED[v['id']]}")
                continue
            found += 1
            vuln = get(f"{OSV}/vulns/{v['id']}")
            fixed = [
                e["fixed"]
                for a in vuln.get("affected", [])
                for rng in a.get("ranges", [])
                for e in rng.get("events", [])
                if "fixed" in e
            ]
            print(f"{p['name']} {p['version']}: {v['id']} {vuln.get('summary', '')}")
            print(f"    fixed in: {fixed[0] if fixed else 'no fix'}")
    print(f"{len(pkgs)} crates checked, {found} new advisories, {accepted} accepted")
    sys.exit(1 if found else 0)


if __name__ == "__main__":
    main()

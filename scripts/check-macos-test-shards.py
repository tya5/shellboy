#!/usr/bin/env python3
"""Verify the fixed macOS Cargo target shards cover the default test inventory."""
import argparse
import json
import subprocess


SHARDS = {
    "1": ["lib:shellboy", "bin:shellboy"],
    "2": ["test:integration", "test:termination_signals", "test:tool_group_crash_recovery"],
}


def test_inventory():
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        check=True, capture_output=True, text=True,
    )
    package = json.loads(result.stdout)["packages"]
    root = next(item for item in package if item["name"] == "shellboy")
    return {
        f"{target['kind'][0]}:{target['name']}"
        for target in root["targets"]
        if target["test"] and not target.get("required-features")
    }


def cargo_args(target):
    kind, name = target.split(":", 1)
    return {"lib": ["--lib"], "bin": ["--bin", name], "test": ["--test", name]}[kind]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shard", choices=SHARDS)
    args = parser.parse_args()

    inventory = test_inventory()
    selected = [target for targets in SHARDS.values() for target in targets]
    if len(selected) != len(set(selected)):
        raise SystemExit("macOS test shard target is duplicated")
    missing = sorted(inventory - set(selected))
    extra = sorted(set(selected) - inventory)
    if missing or extra:
        raise SystemExit(f"macOS test shard inventory mismatch: missing={missing}, extra={extra}")

    if args.shard:
        print(" ".join(arg for target in SHARDS[args.shard] for arg in cargo_args(target)))
    else:
        print(f"macOS test shards cover all {len(inventory)} default test targets exactly once")


if __name__ == "__main__":
    main()

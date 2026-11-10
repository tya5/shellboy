#!/usr/bin/env python3
"""Validate actual encoded window against actual full exported schema, not DTO stand-ins."""
import copy
import json
import sys
from pathlib import Path
import jsonschema

root = Path(sys.argv[1])
schema = json.loads((root / "schema.json").read_text())
actual = json.loads((root / "actual-window.json").read_text())
# Keep the real export root so its #/definitions references remain resolvable.
selected = {"$schema": schema["$schema"], "definitions": copy.deepcopy(schema["definitions"]), "allOf": [schema["operation_results"]["history.window"]]}
validator = jsonschema.validators.validator_for(selected)(selected)
validator.validate(actual)
mutated = copy.deepcopy(actual)
mutated["items"][0]["items"][0]["kind"] = "invented_kind"
assert list(validator.iter_errors(mutated)), "schema accepted unknown item kind"
mutated = copy.deepcopy(actual)
del mutated["items"][0]["items"][0]["text_complete"]
assert list(validator.iter_errors(mutated)), "schema accepted missing text extent"
print("actual window valid; unknown kind and missing text_complete rejected")

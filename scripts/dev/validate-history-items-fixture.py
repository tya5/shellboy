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
for name in ("legacy-window.json", "response-budget-window.json", "unavailable-window.json", "rewindow-window.json", "public-route-window.json"):
    value = json.loads((root / name).read_text())
    validator.validate(value)
budget = json.loads((root / "response-budget-window.json").read_text())
mutated_pair = copy.deepcopy(budget)
del mutated_pair["items"][128]["items_unavailable_reason"]
assert list(validator.iter_errors(mutated_pair)), "schema accepted null items without its unavailable reason"
mutated = copy.deepcopy(actual)
mutated["items"][0]["items"][0]["kind"] = "invented_kind"
assert list(validator.iter_errors(mutated)), "schema accepted unknown item kind"
mutated = copy.deepcopy(actual)
del mutated["items"][0]["items"][0]["text_complete"]
assert list(validator.iter_errors(mutated)), "schema accepted missing text extent"
mutated = copy.deepcopy(actual)
mutated["items"][0]["items_unavailable_reason"] = "response_budget"
assert list(validator.iter_errors(mutated)), "schema accepted array items plus node reason"
mutated = copy.deepcopy(budget)
mutated["items"][128]["items_unavailable_reason"] = "source_budget"
assert list(validator.iter_errors(mutated)), "schema accepted source_budget as null-node reason"
mutated = copy.deepcopy(actual)
del mutated["items"][0]["items"][0]["item_identity"]
assert list(validator.iter_errors(mutated)), "schema accepted missing stable identity"
print("actual producer windows valid (typed/legacy/budget/unavailable); six invalid mutations rejected")

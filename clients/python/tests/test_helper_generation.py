"""Contract growth must fail generation until every helper exposes the new surface."""
import copy
import importlib.util
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("sdk_generator", ROOT / "clients/python/scripts/generate.py")
GENERATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GENERATOR)
CONTRACT = json.loads((ROOT / "crates/api/contract/http.json").read_text())
METHODS = {entry["operationId"]: entry for entry in CONTRACT["operations"]}
SCHEMA = json.loads((ROOT / "crates/api/contract/api.schema.json").read_text())["definitions"]


@pytest.mark.parametrize("problem", ["missing", "extra", "duplicate", "name_collision"])
def test_helper_inventory_rejects_incomplete_or_ambiguous_coverage(problem):
    inventory = GENERATOR.helper_inventory()
    if problem == "missing":
        inventory["all"].pop()
    elif problem == "extra":
        inventory["all"].append(["missing/operation", "missingOperation"])
    elif problem == "duplicate":
        inventory["all"].append(list(inventory["all"][0]))
    else:
        inventory["all"][1][1] = inventory["all"][0][1]
    with pytest.raises(ValueError):
        GENERATOR.validate_python_helpers(inventory, METHODS, SCHEMA)


def test_handwritten_helpers_must_expose_new_wire_fields():
    schema = copy.deepcopy(SCHEMA)
    schema["RunStartParams"]["properties"]["futureOption"] = {"type": "string"}
    with pytest.raises(ValueError, match="futureOption"):
        GENERATOR.validate_python_helpers(GENERATOR.helper_inventory(), METHODS, schema)


def test_generated_helpers_cannot_shadow_handwritten_utilities():
    inventory = GENERATOR.helper_inventory()
    operation = inventory["generated"][0][0]
    inventory["generated"][0][1] = "withActor"
    for entry in inventory["all"]:
        if entry[0] == operation:
            entry[1] = "withActor"
    with pytest.raises(ValueError, match="collides"):
        GENERATOR.validate_python_helpers(inventory, METHODS, SCHEMA)


@pytest.mark.parametrize("fields", [{"headers": {"type": "string"}}, {"self": {"type": "string"}}, {"someId": {"type": "string"}, "some_id": {"type": "string"}}, {"from": {"type": "string"}}])
def test_parameter_names_cannot_shadow_transport_arguments_or_each_other(fields):
    entry = {"method": "example/read", "params": {"schema": {"$ref": "api.schema.json#/definitions/ExampleParams"}}, "result": {"type": "ExampleResponse", "schema": {"$ref": "api.schema.json#/definitions/ExampleResponse"}}}
    with pytest.raises(ValueError, match="collides"):
        GENERATOR.render_helper(entry, "read_example", [], {"ExampleParams": {"properties": fields}}, {})

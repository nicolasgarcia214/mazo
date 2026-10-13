"""Reference loader for skima's strict YAML differential test (SPEC §10.5).

PolicyLoader and construct_boolean below are copied verbatim from
dotfiles skills/install.sh:106-132; `frontmatter` repeats install.sh:141-145.
Do not edit them: tests/strict_yaml_diff.rs holds src/strict_yaml.rs to them.

Reads a JSON list of {"mode": "load" | "frontmatter", "text": str} on stdin and
prints a JSON list with one canonical typed tree, or {"error": true}, per case.
"""

import datetime
import io
import json
import math
import re
import struct
import sys

import yaml


class PolicyLoader(yaml.SafeLoader):
    yaml_implicit_resolvers = {
        key: [(tag, pattern) for tag, pattern in values if tag != "tag:yaml.org,2002:bool"]
        for key, values in yaml.SafeLoader.yaml_implicit_resolvers.items()
    }

    def construct_mapping(self, node, deep=False):
        keys = set()
        for key_node, _ in node.value:
            # Pi does not enable YAML merge keys. Require explicit policy fields.
            if key_node.tag == "tag:yaml.org,2002:merge":
                raise ValueError("YAML merge keys are not supported in invocation policy")
            key = self.construct_object(key_node, deep=deep)
            if key in keys:
                raise ValueError(f"duplicate YAML key: {key}")
            keys.add(key)
        return super().construct_mapping(node, deep=deep)


def construct_boolean(loader, node):
    if node.value not in ("true", "false"):
        raise ValueError("invocation booleans must be literal true or false")
    return node.value == "true"


PolicyLoader.add_implicit_resolver("tag:yaml.org,2002:bool", re.compile(r"^(true|false)$"), list("tf"))
PolicyLoader.add_constructor("tag:yaml.org,2002:bool", construct_boolean)


def frontmatter(text):
    # install.sh reads SKILL.md with read_text(encoding="utf-8-sig"):
    # BOM dropped, universal newlines.
    text = io.TextIOWrapper(io.BytesIO(text.encode("utf-8")), encoding="utf-8-sig").read()
    lines = text.splitlines()
    if not lines or lines[0] != "---":
        raise ValueError("SKILL.md must start with YAML frontmatter")
    end = lines.index("---", 1)
    return "\n".join(lines[1:end])


def canon(value):
    if value is None:
        return {"null": None}
    if isinstance(value, bool):
        return {"bool": value}
    if isinstance(value, int):
        return {"int": str(value)}
    if isinstance(value, float):
        return {"float": "nan" if math.isnan(value) else struct.pack(">d", value).hex()}
    if isinstance(value, str):
        if any(0xD800 <= ord(c) <= 0xDFFF for c in value):
            return {"unrepresentable": ascii(value)}  # a lone surrogate from a \u escape
        return {"str": value}
    if isinstance(value, bytes):
        return {"binary": value.hex()}
    if isinstance(value, datetime.datetime):
        return {"datetime": value.isoformat()}
    if isinstance(value, datetime.date):
        return {"date": value.isoformat()}
    if isinstance(value, list):
        return {"seq": [canon(item) for item in value]}
    if isinstance(value, tuple):
        return {"tuple": [canon(item) for item in value]}
    if isinstance(value, dict):
        return {"map": [[canon(k), canon(v)] for k, v in value.items()]}
    if isinstance(value, set):
        return {"set": [canon(item) for item in value]}
    raise TypeError(f"no canonical form for {type(value).__name__}")


def run(case):
    text = case["text"]
    if case["mode"] == "frontmatter":
        text = frontmatter(text)
    return canon(yaml.load(text, Loader=PolicyLoader))


def main():
    sys.set_int_max_str_digits(0)
    results = []
    for case in json.load(sys.stdin):
        try:
            results.append(run(case))
        except Exception:
            results.append({"error": True})
    json.dump(results, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()

# -*- coding: utf-8 -*-
"""Check the submission rules for conversation.json before it is committed."""

import json
import sys
from pathlib import Path

LIMIT = 128 * 1024


def no_duplicates(pairs):
    seen = {}
    for key, value in pairs:
        if key in seen:
            raise ValueError("duplicate key: %s" % key)
        seen[key] = value
    return seen


def reject_constant(name):
    raise ValueError("non-standard number: %s" % name)


def main(path):
    raw = Path(path).read_bytes()
    problems = []
    if len(raw) > LIMIT:
        problems.append("size %d exceeds %d bytes" % (len(raw), LIMIT))
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        problems.append("not UTF-8: %s" % error)
        text = None
    if text is not None:
        if text.startswith("\ufeff"):
            problems.append("file starts with a BOM")
        try:
            doc = json.loads(
                text,
                object_pairs_hook=no_duplicates,
                parse_constant=reject_constant,
            )
        except ValueError as error:
            problems.append("invalid JSON: %s" % error)
            doc = None
        if doc is not None and not isinstance(doc, dict):
            problems.append("top level is %s, not an object" % type(doc).__name__)
        if isinstance(doc, dict):
            if doc.get("meta", {}).get("synthetic") is not False:
                problems.append("meta.synthetic should be false for real records")
            for name in ("user_prompts", "experiments", "evaluation_instances"):
                if not doc.get(name):
                    problems.append("missing or empty: %s" % name)
            for index, instance in enumerate(doc.get("evaluation_instances", [])):
                if "level" in instance:
                    problems.append(
                        "instance %d: drop the self-assigned level, the reviewer sets the band"
                        % index
                    )
                evidence = instance.get("result_evidence")
                if evidence is None:
                    problems.append("instance %d: missing result_evidence" % index)
                elif isinstance(evidence, dict) and not evidence.get("quote"):
                    problems.append("instance %d: result_evidence without a quote" % index)
            for name in ("student_prompts", "user_prompts", "experiments", "retrospective"):
                if not doc.get(name):
                    problems.append("missing or empty: %s" % name)
    for problem in problems:
        print("FAIL " + problem)
    print("%s: %d bytes, %d problems" % (path, len(raw), len(problems)))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "agent/conversation.json"))

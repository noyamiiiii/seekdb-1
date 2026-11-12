# -*- coding: utf-8 -*-
"""Validate that every evaluation-instance quote matches its JSON pointer."""

import json
import sys
from pathlib import Path


def resolve(document, pointer):
    if pointer == "":
        return document
    if not pointer.startswith("/"):
        raise ValueError("pointer must start with /: %r" % pointer)
    node = document
    for raw in pointer[1:].split("/"):
        token = raw.replace("~1", "/").replace("~0", "~")
        if isinstance(node, list):
            node = node[int(token)]
        else:
            node = node[token]
    return node


def main(path):
    document = json.loads(Path(path).read_text(encoding="utf-8"))
    failures = 0
    for index, instance in enumerate(document.get("evaluation_instances", [])):
        for side in ("student_evidence", "result_evidence"):
            evidence = instance.get(side) or {}
            pointer = evidence.get("pointer")
            if pointer is None:
                continue
            quote = evidence.get("quote")
            try:
                text = resolve(document, pointer)
            except (KeyError, IndexError, ValueError, TypeError) as error:
                print("[%s/%s] %s: BAD POINTER (%s)" % (instance.get("id"), side, pointer, error))
                failures += 1
                continue
            if not isinstance(text, str):
                print("[%s/%s] %s: NOT A STRING" % (instance.get("id"), side, pointer))
                failures += 1
                continue
            if not quote:
                print("[%s/%s] %s: EMPTY QUOTE" % (instance.get("id"), side, pointer))
                failures += 1
                continue
            if len(quote) > 240:
                print("[%s/%s] quote longer than 240 chars (%d)" % (instance.get("id"), side, len(quote)))
                failures += 1
            if quote not in text:
                print("[%s/%s] %s: QUOTE NOT FOUND" % (instance.get("id"), side, pointer))
                print("    wanted: %r" % quote)
                print("    actual: %r" % text[:400])
                failures += 1
    print("instances: %d, failures: %d" % (len(document.get("evaluation_instances", [])), failures))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "agent/conversation.json"))

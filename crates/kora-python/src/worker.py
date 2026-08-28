"""The Kora Python worker.

Runs as a child process, reads one JSON request per line, writes one JSON
response per line. Deliberately small: it imports modules and calls functions,
and it cannot call back into Kora.

Values cross as JSON. That is the whole boundary -- there are no live object
handles, which is what keeps Kora's threading, durability, and data labels
intact (see DECISIONS.md).
"""

import importlib
import json
import sys
import traceback

_MODULES = {}


def _load(name):
    if name not in _MODULES:
        _MODULES[name] = importlib.import_module(name)
    return _MODULES[name]


def _encodable(value):
    """Convert a result into something JSON can carry.

    Anything without a JSON form is described rather than dropped, so the
    caller sees what happened instead of a silent null.
    """
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, (list, tuple, set, frozenset)):
        return [_encodable(v) for v in value]
    if isinstance(value, dict):
        return {str(k): _encodable(v) for k, v in value.items()}
    # numpy scalars, Decimal, datetime, dataclasses, and anything else with a
    # sensible textual form.
    for attr in ("tolist", "item", "isoformat"):
        method = getattr(value, attr, None)
        if callable(method):
            try:
                return _encodable(method())
            except Exception:
                pass
    if hasattr(value, "__dict__"):
        try:
            return {str(k): _encodable(v) for k, v in vars(value).items()}
        except Exception:
            pass
    return f"<{type(value).__name__}>"


def _handle(request):
    module_name = request.get("module")
    func_name = request.get("func")
    args = request.get("args", [])
    kwargs = request.get("kwargs", {})

    module = _load(module_name)
    target = getattr(module, func_name, None)
    if target is None:
        available = [n for n in dir(module) if not n.startswith("_")]
        raise AttributeError(
            f"`{module_name}` has no `{func_name}`. It provides: "
            + ", ".join(sorted(available)[:20])
        )
    if not callable(target):
        # A module attribute that is a value, not a function: return it.
        if args or kwargs:
            raise TypeError(f"`{module_name}.{func_name}` is not callable")
        return _encodable(target)
    return _encodable(target(*args, **kwargs))


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            continue

        response = {"id": request.get("id")}
        try:
            response["ok"] = True
            response["result"] = _handle(request)
        except Exception as e:
            response["ok"] = False
            # The exception type matters as much as the message when working
            # out what went wrong on the other side of a process boundary.
            response["error"] = f"{type(e).__name__}: {e}"
            response["traceback"] = traceback.format_exc(limit=3)

        sys.stdout.write(json.dumps(response) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()

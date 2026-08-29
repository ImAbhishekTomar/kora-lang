"""An MCP server that behaves badly on purpose.

Modes come from argv[1]:
  wedge  -- accept `tools/call` and never answer it
  die    -- exit part-way through a `tools/call`
  flaky  -- exit before the handshake on the first two starts, then serve
            normally, so a client that gives up too early never connects
  late   -- answer the first `tools/call` long after the client gave up,
            and hold the second answer until that stale one has been sent,
            so the client is guaranteed to see them in that order

`argv[2]`, when present, is a file the server appends one line to per
`tools/call` it receives, so a test can prove a call was not repeated.
"""

import json
import sys
import threading
import time

mode = sys.argv[1]
tally = sys.argv[2] if len(sys.argv) > 2 else None
calls = 0

if mode == "flaky":
    # The tally doubles as the attempt counter here: a start that is not the
    # third one dies before answering anything.
    with open(tally, "a") as f:
        f.write("start\n")
    with open(tally) as f:
        if len(f.readlines()) < 3:
            sys.exit(1)
# Ordering, not timing, is what the late-reply test needs: the stale answer
# must reach the client before the answer it is waiting for. A sleep alone
# makes that a race with whatever else the machine is running.
stale_sent = threading.Event()
writing = threading.Lock()


def send(obj):
    with writing:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


def reply(msg_id, text):
    send({"jsonrpc": "2.0", "id": msg_id,
          "result": {"content": [{"type": "text", "text": text}]}})


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if "id" not in msg:
        continue

    if method == "initialize":
        send({"jsonrpc": "2.0", "id": msg["id"],
              "result": {"protocolVersion": "2024-11-05"}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [
            {"name": "act", "description": "has a side effect",
             "inputSchema": {"type": "object", "properties": {}}}
        ]}})
    elif method == "tools/call":
        calls += 1
        if tally:
            with open(tally, "a") as f:
                f.write("call\n")
        if mode == "wedge":
            time.sleep(3600)
        elif mode == "die":
            sys.exit(1)
        elif mode == "late":
            if calls == 1:
                # Answered on a thread, so the server keeps serving while the
                # client gives up on this one.
                def answer_late(msg_id):
                    # Half again the client's timeout in wedged_server_test.rs;
                    # the two are chosen together.
                    time.sleep(6)
                    reply(msg_id, "late answer to the first call")
                    stale_sent.set()

                threading.Thread(
                    target=answer_late, args=(msg["id"],), daemon=True
                ).start()
            else:
                stale_sent.wait(30)
                reply(msg["id"], "answer to the second call")
    else:
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {}})

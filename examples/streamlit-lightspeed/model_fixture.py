"""A loopback Chat Completions fixture for the Streamlit walkthrough."""

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from time import time
from uuid import uuid4

MODEL = "gpt-4o-mini"
NOTE = "This file lives in the Lightspeed demo VFS workspace.\n"


def reply(request: dict) -> tuple[dict, str]:
    messages = request.get("messages", [])
    last_user = next((i for i in reversed(range(len(messages))) if messages[i]["role"] == "user"), None)
    turn = messages[last_user:] if last_user is not None else []
    text = turn[0].get("content", "") if turn else ""
    if not isinstance(text, str):
        text = json.dumps(text)
    results = [item.get("content") for item in turn if item["role"] == "tool"]

    if results:
        evidence = json.dumps(results)
        parts = []
        if NOTE.strip() in evidence:
            parts.append("Read /workspace/notes.txt from VFS.")
        if "/envd/workspace" in evidence:
            parts.append("Ran pwd on the local daemon and found its workspace.")
        return {"role": "assistant", "content": " ".join(parts) or "See the tool outcomes in the run map."}, "stop"

    tools = {item["function"]["name"] for item in request.get("tools", []) if item["type"] == "function"}
    prompt = text.lower()
    calls = []
    if "vfs_read_file" in tools and any(word in prompt for word in ("notes.txt", "/workspace", "vfs")):
        calls.append(("vfs_read_file", {"path": "/workspace/notes.txt"}))
    if "exec_command" in tools and any(word in prompt for word in ("pwd", "daemon", "environment")):
        calls.append(("exec_command", {"cmd": "pwd"}))
    if not calls:
        return {"role": "assistant", "content": f"Local demo model received: {text[:300]}"}, "stop"
    return {"role": "assistant", "content": None, "tool_calls": [
        {"id": uuid4().hex, "type": "function", "function": {"name": name, "arguments": json.dumps(params)}}
        for name, params in calls
    ]}, "tool_calls"


class ModelHandler(BaseHTTPRequestHandler):
    def log_message(self, *_args: object) -> None:
        pass

    def send_json(self, data: dict) -> None:
        body = json.dumps(data).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        if self.path == "/health":
            self.send_json({"instance": self.server.instance})
        elif self.path == "/v1/models":
            self.send_json({
                "object": "list", "data": [{"id": MODEL, "object": "model", "created": 1, "owned_by": "local-demo"}],
            })
        else:
            self.send_error(404)

    def do_POST(self) -> None:
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        message, finish = reply(request)
        self.send_json({
            "id": uuid4().hex, "object": "chat.completion", "created": int(time()), "model": MODEL,
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15},
        })


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port_file", type=Path)
    parser.add_argument("port", type=int)
    parser.add_argument("instance")
    args = parser.parse_args()
    with ThreadingHTTPServer(("127.0.0.1", args.port), ModelHandler) as server:
        server.instance = args.instance
        args.port_file.write_text(str(server.server_port))
        server.serve_forever()


if __name__ == "__main__":
    main()

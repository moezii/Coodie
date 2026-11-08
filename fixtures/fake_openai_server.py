"""Minimal fake OpenAI-compatible server for verifying Codie's semantic find path.

It parses the numbered candidate list out of the ranking prompt and selects the
candidate whose name contains a token from the query, then replies in the
OpenAI chat/completions shape. This stands in for a real model so the full CLI
pipeline (prompt build -> HTTP -> JSON parse -> routing -> render) can be exercised.
"""
import json
import re
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer


def rank(prompt_user: str) -> dict:
    query_line = next((l for l in prompt_user.splitlines() if l.startswith("Query:")), "")
    query = query_line[len("Query:"):].strip().lower()
    # crude concept expansion so "auth" lands on verify_token/login/issue_token
    concept = {"auth", "authentication", "login", "token", "verify", "credential", "signin"}
    want = set(re.findall(r"[a-z0-9]+", query)) | (concept if "auth" in query else set())

    matches = []
    for line in prompt_user.splitlines():
        m = re.match(r"\s*(\d+)\.\s+name=(\S+)", line)
        if not m:
            continue
        index, name = int(m.group(1)), m.group(2).lower()
        tokens = set(re.findall(r"[a-z0-9]+", name))
        if tokens & want:
            matches.append({"index": index, "score": 0.92})

    confidence = "high" if len(matches) == 1 else ("medium" if matches else "low")
    suggestions = [] if matches else ["try: login, token, or session"]
    return {"confidence": confidence, "matches": matches, "suggestions": suggestions}


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        user = ""
        for message in body.get("messages", []):
            if message.get("role") == "user":
                user = message.get("content", "")

        if body.get("stream"):
            # `explain` streams: emit a short, bare explanation as SSE deltas.
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            text = (
                "This function is the retrieved snippet; it runs the logic shown "
                "and matched your query. (No fixes suggested.)"
            )
            for word in text.split(" "):
                chunk = {"choices": [{"delta": {"content": word + " "}}]}
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode("utf-8"))
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
            return

        # `find` ranking: non-streaming JSON in the chat/completions shape.
        content = json.dumps(rank(user))
        payload = json.dumps({"choices": [{"message": {"role": "assistant", "content": content}}]})
        encoded = payload.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, *_args):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8723
    HTTPServer(("127.0.0.1", port), Handler).serve_forever()

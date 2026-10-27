"""Serve a mixer page on another port; pass every other request on to the
running BananaSplit (127.0.0.1:8790). Used by measure.sh.

    python3 -I serve_page.py PORT PAGE.html
"""
import http.client
import http.server
import sys

UPSTREAM = ("127.0.0.1", 8790)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def do_GET(self):
        if self.path == "/":
            with open(PAGE, "rb") as f:
                body = f.read()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(body)
            return
        c = http.client.HTTPConnection(*UPSTREAM, timeout=60)
        headers = {k: v for k, v in self.headers.items() if k.lower() != "host"}
        headers["Host"] = "%s:%d" % UPSTREAM  # BananaSplit only answers its own host name
        c.request("GET", self.path, headers=headers)
        r = c.getresponse()
        self.send_response(r.status)
        for k, v in r.getheaders():
            if k.lower() not in ("transfer-encoding", "connection", "content-length"):
                self.send_header(k, v)
        self.end_headers()
        try:
            while True:  # /api/events is a stream
                b = r.read1(65536)
                if not b:
                    break
                self.wfile.write(b)
                self.wfile.flush()
        except OSError:
            pass

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    PAGE = sys.argv[2]
    http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()

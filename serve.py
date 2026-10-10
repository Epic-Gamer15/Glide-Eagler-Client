#!/usr/bin/env python3
"""Glide launcher: serves the web panel at http://localhost:8080 and opens it."""
import http.server, socketserver, webbrowser, os, functools, threading

PORT = int(os.environ.get("PORT", "8080"))
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "Html")
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=ROOT)
socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", PORT), handler) as httpd:
    url = f"http://localhost:{PORT}/"
    print(f"Glide panel running at {url}  (Ctrl+C to stop)")
    threading.Timer(0.8, lambda: webbrowser.open(url)).start()
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass

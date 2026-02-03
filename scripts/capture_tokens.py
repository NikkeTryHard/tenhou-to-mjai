#!/usr/bin/env python3
"""
mitmproxy addon to capture Majsoul OAuth tokens from URL redirect.

Usage:
    mitmdump -s capture_tokens.py

Tokens are saved to tokens.txt in format: uid,token,server
"""
from urllib.parse import urlparse, parse_qs
import os

OUTPUT_FILE = os.environ.get("TOKEN_FILE", "tokens.txt")

class MajsoulTokenLogger:
    def __init__(self):
        self.captured = set()

    def request(self, flow):
        host = flow.request.pretty_host

        # Match EN and JP game domains
        if "mahjongsoul.game.yo-star.com" in host:
            server = "en"
        elif "game.mahjongsoul.com" in host:
            server = "jp"
        else:
            return

        queries = parse_qs(urlparse(flow.request.url).query)

        if "uid" in queries and "token" in queries:
            uid = queries["uid"][0]
            token = queries["token"][0]
            key = f"{uid},{token}"

            if key not in self.captured:
                self.captured.add(key)

                with open(OUTPUT_FILE, "a") as f:
                    f.write(f"{uid},{token},{server}\n")

                print(f"[CAPTURED] uid={uid}, server={server}")
                print(f"  token={token[:20]}...")

addons = [MajsoulTokenLogger()]

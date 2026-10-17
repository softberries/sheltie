#!/usr/bin/env python3
"""Register a GitHub App for Sheltie from deploy/github-app-manifest.json.

Uses GitHub's App manifest flow: opens a page that posts the manifest to GitHub, you confirm
the App name in the browser, and GitHub redirects back here with a one-hour code that is
exchanged for the App id, private key and webhook secret.

The private key is written to sheltie-github-app.pem (git-ignored, mode 600) and the three
SHELTIE_GITHUB_* settings are appended to .env if they are not set there yet. Secrets are never
printed.

Usage:
  scripts/register-github-app.py --url https://sheltie.example.com [--org ORG] [--name NAME]
      A production App: webhooks go to <url>/webhooks/github. Also writes a random
      SHELTIE_POSTGRES_PASSWORD and SHELTIE_PUBLIC_HOSTS to .env (for compose.yaml).
  scripts/register-github-app.py [--org ORG] [--name NAME] [--webhook-url URL]
      A development App. Without --webhook-url a new smee.io channel forwards webhooks locally.
"""

import argparse
import html
import http.server
import json
import os
import secrets
import sys
import urllib.parse
import urllib.request
import webbrowser
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "deploy" / "github-app-manifest.json"
KEY_FILE = ROOT / "sheltie-github-app.pem"
ENV_FILE = ROOT / ".env"
PORT = 8765


def new_smee_channel() -> str:
    request = urllib.request.Request("https://smee.io/new", method="HEAD")
    with urllib.request.urlopen(request) as response:  # follows the redirect to the channel
        return response.geturl()


def exchange(code: str) -> dict:
    request = urllib.request.Request(
        f"https://api.github.com/app-manifests/{urllib.parse.quote(code)}/conversions",
        method="POST",
        headers={"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"},
    )
    with urllib.request.urlopen(request) as response:
        return json.load(response)


def save(app: dict, extra: dict) -> None:
    KEY_FILE.write_text(app["pem"])
    os.chmod(KEY_FILE, 0o600)
    existing = ENV_FILE.read_text() if ENV_FILE.exists() else ""
    settings = {
        "SHELTIE_GITHUB_APP_ID": str(app["id"]),
        "SHELTIE_GITHUB_PRIVATE_KEY_PATH": f"./{KEY_FILE.name}",
        "SHELTIE_GITHUB_WEBHOOK_SECRET": app["webhook_secret"],
        **extra,
    }
    missing = {k: v for k, v in settings.items() if f"\n{k}=" not in f"\n{existing}"}
    if missing:
        with ENV_FILE.open("a") as env:
            if existing and not existing.endswith("\n"):
                env.write("\n")
            env.writelines(f"{k}={v}\n" for k, v in missing.items())
    skipped = sorted(set(settings) - set(missing))
    print(f"App created: {app['html_url']} (id {app['id']})")
    print(f"Private key written to {KEY_FILE.name}")
    if missing:
        print(f"Added to .env: {', '.join(missing)}")
    if skipped:
        print(f"Already set in .env, left unchanged: {', '.join(skipped)}. Update them by hand.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--org", help="register under this organization instead of your account")
    parser.add_argument("--name", help="App name (must be unique on GitHub)")
    parser.add_argument("--url", help="public https URL of your Sheltie server (production)")
    parser.add_argument("--webhook-url", help="development: public URL that forwards to /webhooks/github")
    args = parser.parse_args()
    if args.url and args.webhook_url:
        sys.exit("give --url (production) or --webhook-url (development), not both")
    base_url = None
    if args.url:
        parsed = urllib.parse.urlparse(args.url)
        if parsed.scheme != "https" or not parsed.hostname or parsed.path not in ("", "/"):
            sys.exit("--url must be an https URL of the server, e.g. https://sheltie.example.com")
        base_url = f"https://{parsed.netloc}"

    if KEY_FILE.exists():
        sys.exit(f"{KEY_FILE.name} already exists; move it away to register a new App.")

    manifest = json.loads(MANIFEST.read_text())
    if base_url:
        host = urllib.parse.urlparse(base_url).hostname or ""
        # App names are unique on GitHub and at most 34 characters.
        manifest["name"] = f"Sheltie {host}"[:34]
        manifest["description"] = "Parallel merge engine for agent-generated pull requests."
    if args.name:
        manifest["name"] = args.name
    if base_url:
        webhook_url = f"{base_url}/webhooks/github"
    else:
        webhook_url = args.webhook_url or new_smee_channel()
    manifest["hook_attributes"]["url"] = webhook_url
    manifest["redirect_url"] = f"http://127.0.0.1:{PORT}/callback"

    state = secrets.token_urlsafe(16)
    base = f"https://github.com/organizations/{args.org}" if args.org else "https://github.com"
    action = f"{base}/settings/apps/new?state={state}"
    form = f"""<!doctype html><meta charset="utf-8"><title>Register Sheltie App</title>
<form id="f" method="post" action="{html.escape(action)}">
<input type="hidden" name="manifest" value="{html.escape(json.dumps(manifest))}">
<button>Continue to GitHub</button></form>
<script>document.getElementById("f").submit()</script>"""
    result: dict = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            url = urllib.parse.urlparse(self.path)
            query = urllib.parse.parse_qs(url.query)
            if url.path == "/":
                self._send(200, form)
            elif url.path == "/callback" and query.get("state") == [state] and "code" in query:
                result["app"] = exchange(query["code"][0])
                self._send(200, "<p>Sheltie App registered. You can close this tab.</p>")
            else:
                self._send(400, "<p>Unexpected request.</p>")

        def _send(self, status: int, body: str) -> None:
            self.send_response(status)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(body.encode())

        def log_message(self, *_: object) -> None:  # keep codes out of the terminal
            pass

    server = http.server.HTTPServer(("127.0.0.1", PORT), Handler)
    print(f"Webhook URL: {webhook_url}")
    print(f"Opening http://127.0.0.1:{PORT}/ — confirm the App on GitHub.")
    webbrowser.open(f"http://127.0.0.1:{PORT}/")
    while "app" not in result:
        server.handle_request()
    if base_url:
        extra = {
            "SHELTIE_POSTGRES_PASSWORD": secrets.token_urlsafe(24),
            "SHELTIE_PUBLIC_HOSTS": urllib.parse.urlparse(base_url).netloc,
            # compose.yaml runs the server as this user so it can read the mode-600 key.
            "SHELTIE_UID": str(os.getuid()),
        }
        save(result["app"], extra)
        print("\nNext:")
        print("  docker compose up -d --build")
        print(f"  curl {base_url}/healthz")
        print("Then install the App on your repositories from the App's page, and add the")
        print("workflows (docs/self-hosting.md).")
    else:
        save(result["app"], {})
        print("\nNext: forward webhooks to the local server (keep it running):")
        print(f"  npx smee-client --url {webhook_url} --target http://127.0.0.1:8080/webhooks/github")
        print("Then install the App on a test repository from the App's page.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

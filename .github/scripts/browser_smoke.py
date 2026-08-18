#!/usr/bin/env python3
"""Exercise the served WASM application through Chrome's WebDriver API.

This intentionally uses only the Python standard library. GitHub's Ubuntu
runner image already provides a matched Google Chrome and ChromeDriver pair,
so the smoke test does not need to download an additional browser or npm
dependency.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.request
from typing import Any


class WebDriverError(RuntimeError):
    """Raised when ChromeDriver rejects a WebDriver command."""


def webdriver_request(
    base_url: str,
    method: str,
    path: str,
    payload: dict[str, Any] | None = None,
) -> Any:
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/{path.lstrip('/')}",
        data=data,
        method=method,
        headers={"Content-Type": "application/json; charset=utf-8"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            document = json.load(response)
    except urllib.error.HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        raise WebDriverError(
            f"ChromeDriver returned HTTP {error.code} for {method} {path}: {detail}"
        ) from error

    value = document.get("value")
    if isinstance(value, dict) and value.get("error"):
        raise WebDriverError(
            f"ChromeDriver rejected {method} {path}: "
            f"{value.get('error')}: {value.get('message', '')}"
        )
    return value


def browser_logs(base_url: str, session_id: str) -> list[dict[str, Any]]:
    """Read browser console logs across current and older ChromeDriver routes."""

    for suffix in ("se/log", "log"):
        try:
            value = webdriver_request(
                base_url,
                "POST",
                f"session/{session_id}/{suffix}",
                {"type": "browser"},
            )
            return value if isinstance(value, list) else []
        except WebDriverError:
            continue
    return []


def print_browser_logs(logs: list[dict[str, Any]]) -> None:
    for entry in logs:
        level = entry.get("level", "UNKNOWN")
        source = entry.get("source", "browser")
        message = entry.get("message", "")
        print(f"browser[{level}][{source}] {message}", file=sys.stderr)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("url", help="Application URL to load")
    parser.add_argument(
        "--webdriver",
        default="http://127.0.0.1:9515",
        help="ChromeDriver base URL",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=30.0,
        help="Seconds to wait for the WASM application to remove its loader",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    session_id: str | None = None
    logs: list[dict[str, Any]] = []
    state: dict[str, Any] = {}

    try:
        session = webdriver_request(
            args.webdriver,
            "POST",
            "session",
            {
                "capabilities": {
                    "alwaysMatch": {
                        "browserName": "chrome",
                        "goog:chromeOptions": {
                            "args": [
                                "--headless=new",
                                "--no-sandbox",
                                "--disable-dev-shm-usage",
                                "--enable-unsafe-swiftshader",
                                "--use-gl=angle",
                                "--use-angle=swiftshader-webgl",
                                "--window-size=1400,900",
                            ]
                        },
                        "goog:loggingPrefs": {"browser": "ALL"},
                    }
                }
            },
        )
        if not isinstance(session, dict) or not isinstance(
            session.get("sessionId"), str
        ):
            raise WebDriverError(f"unexpected new-session response: {session!r}")
        session_id = session["sessionId"]

        webdriver_request(
            args.webdriver,
            "POST",
            f"session/{session_id}/timeouts",
            {"pageLoad": 30_000, "script": 10_000, "implicit": 0},
        )
        webdriver_request(
            args.webdriver,
            "POST",
            f"session/{session_id}/url",
            {"url": args.url},
        )

        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            value = webdriver_request(
                args.webdriver,
                "POST",
                f"session/{session_id}/execute/sync",
                {
                    "script": """
                        const loading = document.getElementById("loading");
                        const canvas = document.getElementById("mantis_canvas");
                        return {
                            readyState: document.readyState,
                            wasmStarted: typeof window.wasmBindings !== "undefined",
                            loadingText: loading === null ? null : loading.textContent,
                            canvasWidth: canvas === null ? 0 : canvas.width,
                            canvasHeight: canvas === null ? 0 : canvas.height,
                        };
                    """,
                    "args": [],
                },
            )
            state = value if isinstance(value, dict) else {}
            if state.get("wasmStarted") and state.get("loadingText") is None:
                break
            loading_text = state.get("loadingText")
            if isinstance(loading_text, str) and "failed to start" in loading_text:
                break
            time.sleep(0.25)

        logs = browser_logs(args.webdriver, session_id)
        print_browser_logs(logs)

        if not state.get("wasmStarted"):
            raise WebDriverError(
                "WASM bootstrap did not run; check script CSP/nonces and module loading "
                f"(last browser state: {state!r})"
            )
        if state.get("loadingText") is not None:
            raise WebDriverError(
                "application did not remove its loading indicator "
                f"(last browser state: {state!r})"
            )
        if not state.get("canvasWidth") or not state.get("canvasHeight"):
            raise WebDriverError(
                f"application canvas was not initialized (last browser state: {state!r})"
            )

        severe_script_logs = [
            entry
            for entry in logs
            if entry.get("level") == "SEVERE"
            and entry.get("source") in {"javascript", "security"}
        ]
        if severe_script_logs:
            raise WebDriverError(
                f"browser reported {len(severe_script_logs)} severe script/security error(s)"
            )

        print(f"browser smoke passed: {json.dumps(state, sort_keys=True)}")
        return 0
    finally:
        if session_id is not None:
            try:
                webdriver_request(
                    args.webdriver, "DELETE", f"session/{session_id}"
                )
            except WebDriverError as error:
                print(f"warning: failed to close browser session: {error}", file=sys.stderr)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (WebDriverError, TimeoutError, urllib.error.URLError) as error:
        print(f"browser smoke failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error

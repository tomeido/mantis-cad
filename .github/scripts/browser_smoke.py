#!/usr/bin/env python3
"""Exercise WASM startup, pending reload recovery, and save failure handling.

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
from collections.abc import Callable
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


def execute_script(
    base_url: str, session_id: str, script: str, *, asynchronous: bool = False
) -> Any:
    return webdriver_request(
        base_url,
        "POST",
        f"session/{session_id}/execute/{'async' if asynchronous else 'sync'}",
        {"script": script, "args": []},
    )


def wait_for_app(base_url: str, session_id: str, timeout: float) -> dict[str, Any]:
    state: dict[str, Any] = {}
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = execute_script(
            base_url,
            session_id,
            """
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
        )
        state = value if isinstance(value, dict) else {}
        if state.get("wasmStarted") and state.get("loadingText") is None:
            break
        loading_text = state.get("loadingText")
        if isinstance(loading_text, str) and "failed to start" in loading_text:
            break
        time.sleep(0.25)

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
    return state


def reload_app(base_url: str, session_id: str, timeout: float) -> None:
    """Require a new browser document as well as a running WASM app."""

    # An already running app also satisfies wait_for_app(). Keep a protected,
    # non-enumerable marker on this document so a skipped refresh cannot pass.
    execute_script(
        base_url, session_id,
        """
            Object.defineProperty(window, Symbol.for("mantis-cad.browser-smoke.reload"), {
                value: true,
            });
        """,
    )
    webdriver_request(base_url, "POST", f"session/{session_id}/refresh", {})
    wait_for_app(base_url, session_id, timeout)
    if execute_script(
        base_url, session_id,
        'return !(Symbol.for("mantis-cad.browser-smoke.reload") in window);',
    ) is not True:
        raise WebDriverError("reload did not replace the browser document")


def saved_workspace(base_url: str, session_id: str) -> dict[str, Any] | None:
    """Return only active workspace fields from the fresh browser profile."""

    value = execute_script(
        base_url,
        session_id,
        """
            const done = arguments[arguments.length - 1];
            const request = indexedDB.open("mantis-cad", 1);
            // The application owns database creation. Do not create a catalog
            // on its behalf if startup persistence has not run.
            request.onupgradeneeded = () => request.transaction.abort();
            request.onerror = () => done({error: "cannot open app IndexedDB"});
            request.onsuccess = () => {
                const database = request.result;
                let snapshot = null;
                let error = null;
                let transaction;
                try {
                    transaction = database.transaction("state", "readonly");
                    const read = transaction.objectStore("state").get("catalog");
                    read.onsuccess = () => {
                        if (read.result === undefined) return;
                        try {
                            const catalog = JSON.parse(read.result);
                            const workspace = catalog.workspaces.find(
                                item => item.id === catalog.active_id
                            );
                            if (!workspace || !Array.isArray(workspace.pending)
                                || !Array.isArray(workspace.recovery_ops)
                                || !Array.isArray(workspace.chain.blocks)
                                || workspace.chain.blocks.length === 0) {
                                error = "invalid active workspace in app IndexedDB";
                                return;
                            }
                            const blocks = workspace.chain.blocks;
                            // Never return the raw catalog: settings include the
                            // browser-generated signing identity.
                            snapshot = {
                                activeId: catalog.active_id,
                                head: blocks[blocks.length - 1].hash,
                                blocks: blocks.length,
                                pending: workspace.pending,
                                recovery: workspace.recovery_ops,
                            };
                        } catch (_) {
                            error = "cannot decode active workspace in app IndexedDB";
                        }
                    };
                } catch (_) {
                    database.close();
                    done({error: "cannot read app IndexedDB catalog"});
                    return;
                }
                transaction.oncomplete = () => {
                    database.close();
                    done(error === null ? snapshot : {error});
                };
                transaction.onabort = () => {
                    database.close();
                    done({error: "app IndexedDB read transaction aborted"});
                };
            };
        """,
        asynchronous=True,
    )
    if value is None:
        return None
    if not isinstance(value, dict):
        raise WebDriverError("unexpected active workspace response from browser")
    if "error" in value:
        raise WebDriverError(str(value["error"]))
    return value


def wait_for_workspace(
    base_url: str,
    session_id: str,
    timeout: float,
    description: str,
    predicate: Callable[[dict[str, Any]], bool],
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    state = None
    while time.monotonic() < deadline:
        state = saved_workspace(base_url, session_id)
        if state is not None and predicate(state):
            return state
        time.sleep(0.1)
    counts = None if state is None else {
        "blocks": state["blocks"],
        "pending": len(state["pending"]),
        "recovery": len(state["recovery"]),
    }
    raise WebDriverError(
        f"timed out waiting for {description} (last saved counts: {counts!r})"
    )


def run_command(base_url: str, session_id: str, command: str, timeout: float) -> None:
    """Use the same focused command palette and keyboard events as a user."""

    execute_script(
        base_url, session_id, 'document.getElementById("mantis_canvas").focus();'
    )

    def send(actions: list[dict[str, Any]]) -> None:
        webdriver_request(
            base_url, "POST", f"session/{session_id}/actions",
            {"actions": [{"type": "key", "id": "commands", "actions": actions}]},
        )

    def shortcut(key: str) -> list[dict[str, Any]]:
        return [
            {"type": "keyDown", "value": "\ue009"},  # Control
            {"type": "keyDown", "value": key},
            {"type": "keyUp", "value": key},
            {"type": "keyUp", "value": "\ue009"},
            {"type": "pause", "duration": 150},
        ]

    # Bootstrap can finish while IndexedDB is still loading. Retry the
    # idempotent palette shortcut until egui focuses its DOM text input.
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        send(shortcut("k"))
        if execute_script(base_url, session_id, """
            const input = document.activeElement;
            return input !== null && input.tagName === "INPUT" && input.type === "text";
        """):
            break
    else:
        raise WebDriverError(f"timed out opening command palette for {command}")

    actions = shortcut("a")
    for character in command:
        actions.extend([
            {"type": "keyDown", "value": character},
            {"type": "keyUp", "value": character},
        ])
    actions.extend([
        {"type": "pause", "duration": 150},
        {"type": "keyDown", "value": "\ue007"},  # Enter
        {"type": "keyUp", "value": "\ue007"},
    ])
    send(actions)

    # Successful commands close the palette and eframe focuses the canvas.
    # An invalid command can also blur the text agent for one frame before
    # commands.rs requests focus again, so require a stable completion signal.
    deadline = time.monotonic() + timeout
    focused_since: float | None = None
    while time.monotonic() < deadline:
        if execute_script(base_url, session_id, """
            const canvas = document.getElementById("mantis_canvas");
            return canvas !== null && document.activeElement === canvas;
        """):
            now = time.monotonic()
            if focused_since is None:
                focused_since = now
            elif now - focused_since >= 0.25:
                return
        else:
            focused_since = None
        time.sleep(0.1)
    raise WebDriverError(f"timed out waiting for command to complete: {command}")


def contains_shape(ops: list[dict[str, Any]], component: str, radius: int) -> bool:
    shape_ids = {
        op["id"] for op in ops
        if op.get("op") == "AddNode" and op.get("type_name") == component
    }
    radius_ids = {
        op["id"] for op in ops
        if op.get("op") == "SetParam" and op.get("key") == "value"
        and op.get("value") == {"Number": radius}
    }
    return any(
        op.get("op") == "Connect" and op["from"][0] in radius_ids
        and op["to"][0] in shape_ids and op["to"][1] == 1
        for op in ops
    )


def run_history_shortcut(base_url: str, session_id: str, *, redo: bool = False) -> None:
    """Send the application's Undo/Redo shortcut to the focused canvas."""

    execute_script(
        base_url, session_id, 'document.getElementById("mantis_canvas").focus();'
    )
    keys = ["\ue009", "\ue008", "z"] if redo else ["\ue009", "z"]
    actions = [
        {"type": "keyDown", "value": key} for key in keys
    ] + [
        {"type": "keyUp", "value": key} for key in reversed(keys)
    ]
    webdriver_request(
        base_url, "POST", f"session/{session_id}/actions",
        {"actions": [{"type": "key", "id": "history", "actions": actions}]},
    )


def exercise_pending_reload(
    base_url: str, session_id: str, timeout: float
) -> dict[str, Any]:
    initial = wait_for_workspace(
        base_url, session_id, timeout, "initial workspace autosave", lambda _: True
    )
    if initial["pending"] or initial["recovery"]:
        raise WebDriverError("fresh browser profile unexpectedly contains local work")

    run_command(base_url, session_id, "Circle 5", timeout)
    before = wait_for_workspace(
        base_url, session_id, timeout, "Circle 5 pending autosave",
        lambda state: contains_shape(state["pending"], "circle", 5),
    )
    circle_ids = {
        op["id"] for op in before["pending"]
        if op.get("op") == "AddNode" and op.get("type_name") == "circle"
    }
    if len(circle_ids) != 1:
        raise WebDriverError("Circle 5 did not create exactly one circle node")
    circle_id = next(iter(circle_ids))
    reload_app(base_url, session_id, timeout)

    # This command needs the restored selection and working graph. Creating
    # an independent shape alone would only prove pending-ledger preservation.
    run_command(base_url, session_id, "Move 1 2 3", timeout)
    moved = wait_for_workspace(
        base_url, session_id, timeout, "post-reload Move 1 2 3 pending autosave",
        lambda state: any(
            op.get("op") == "AddNode" and op.get("type_name") == "move"
            for op in state["pending"][len(before["pending"]) :]
        ),
    )
    if moved["pending"][: len(before["pending"])] != before["pending"]:
        raise WebDriverError("reload lost or changed previously saved pending operations")
    move_ops = moved["pending"][len(before["pending"]) :]
    move_ids = {
        op["id"] for op in move_ops
        if op.get("op") == "AddNode" and op.get("type_name") == "move"
    }
    if len(move_ids) != 1 or not any(
        op.get("op") == "Connect" and op["from"] == [circle_id, 0]
        and op["to"] == [next(iter(move_ids)), 0]
        for op in move_ops
    ):
        raise WebDriverError("post-reload Move did not reference the restored Circle 5")

    # Each shortcut must reach durable storage before the next one. Otherwise
    # a missing Undo followed by a no-op Redo could leave the saved Move intact.
    run_history_shortcut(base_url, session_id)
    undone = wait_for_workspace(
        base_url, session_id, timeout, "post-reload Undo pending autosave",
        lambda state: state["pending"] == before["pending"],
    )
    run_history_shortcut(base_url, session_id, redo=True)
    redone = wait_for_workspace(
        base_url, session_id, timeout, "post-reload Redo pending autosave",
        lambda state: state["pending"] == moved["pending"],
    )

    run_command(base_url, session_id, "Sphere 7", timeout)
    after = wait_for_workspace(
        base_url, session_id, timeout, "post-reload Sphere 7 pending autosave",
        lambda state: contains_shape(state["pending"], "sphere", 7),
    )
    if after["pending"][: len(moved["pending"])] != moved["pending"]:
        raise WebDriverError("post-reload Sphere 7 lost or changed the restored-geometry edit")
    appended = after["pending"][len(moved["pending"]) :]
    if not contains_shape(appended, "sphere", 7):
        raise WebDriverError("post-reload edit did not append its pending operations")
    for state in (before, moved, undone, redone, after):
        if any(state[key] != initial[key] for key in ("activeId", "head", "blocks")):
            raise WebDriverError("local autosave/reload changed the workspace or committed chain")
        if state["recovery"] != initial["recovery"]:
            raise WebDriverError("local autosave/reload unexpectedly created recovery operations")
    return {
        "pendingBeforeReload": len(before["pending"]),
        "pendingAfterMove": len(moved["pending"]),
        "pendingAfterUndo": len(undone["pending"]),
        "pendingAfterRedo": len(redone["pending"]),
        "pendingAfterReload": len(after["pending"]),
        "committedBlocks": after["blocks"],
    }


def install_save_failure(
    base_url: str, session_id: str, *, failure: str = "abort"
) -> None:
    """Fail one catalog save without inspecting its sensitive payload."""

    if failure not in {"abort", "quota", "request-error", "transaction-start"}:
        raise ValueError(f"unknown save failure mode: {failure}")
    installed = execute_script(
        base_url,
        session_id,
        f"const failure = {json.dumps(failure)};\n" + """
            if (window.__mantisSaveFailureProbe !== undefined) return false;
            const probe = window.__mantisSaveFailureProbe = {
                attempts: 0,
                requestSucceeded: false,
                abortRequested: false,
                aborted: false,
                quotaErrorThrown: false,
                requestError: null,
                transactionError: false,
                transactionAttempts: 0,
                transactionStartError: null,
            };
            const originalTransaction = IDBDatabase.prototype.transaction;
            IDBDatabase.prototype.transaction = function(storeNames, mode) {
                const catalogWrite = this.name === "mantis-cad" && mode === "readwrite"
                    && (typeof storeNames === "string" ? storeNames === "state"
                        : Array.from(storeNames).includes("state"));
                if (catalogWrite) probe.transactionAttempts += 1;
                if (catalogWrite && failure === "transaction-start"
                    && probe.transactionAttempts === 1) {
                    // A closed app connection produces a genuine browser
                    // InvalidStateError before any write request can exist.
                    // Snapshot reads use separate readonly connections.
                    this.close();
                    try {
                        return Reflect.apply(originalTransaction, this, arguments);
                    } catch (error) {
                        probe.transactionStartError = error?.name ?? null;
                        throw error;
                    }
                }
                return Reflect.apply(originalTransaction, this, arguments);
            };
            const originalPut = IDBObjectStore.prototype.put;
            const originalAdd = IDBObjectStore.prototype.add;
            IDBObjectStore.prototype.put = function(value, key) {
                const transaction = this.transaction;
                const catalogWrite = transaction.db.name === "mantis-cad"
                    && this.name === "state" && key === "catalog";
                if (catalogWrite) probe.attempts += 1;
                if (catalogWrite && probe.attempts === 1) {
                    transaction.addEventListener("abort", () => {
                        probe.aborted = true;
                    }, {once: true});
                }
                if (catalogWrite && probe.attempts === 1 && failure === "quota") {
                    // Exercise the synchronous put() error path without
                    // exhausting browser quota or examining the catalog.
                    // The application must abort this transaction itself.
                    probe.quotaErrorThrown = true;
                    throw new DOMException("Injected catalog write failure", "QuotaExceededError");
                }
                if (catalogWrite && probe.attempts === 1 && failure === "request-error") {
                    // The durable catalog already owns this key. add() fails
                    // asynchronously, and its unhandled error aborts the
                    // transaction without an explicit abort from this hook.
                    transaction.addEventListener("error", () => {
                        probe.transactionError = true;
                    }, {once: true});
                    const request = Reflect.apply(originalAdd, this, arguments);
                    request.addEventListener("error", () => {
                        probe.requestError = request.error?.name ?? null;
                    }, {once: true});
                    return request;
                }
                // Forward the value opaquely: the catalog contains an identity.
                const request = Reflect.apply(originalPut, this, arguments);
                if (catalogWrite && probe.attempts === 1) {
                    request.addEventListener("success", () => {
                        probe.requestSucceeded = true;
                        transaction.abort();
                        probe.abortRequested = true;
                    }, {once: true});
                }
                // Subsequent writes proceed normally, making an unintended
                // application retry observable rather than blocking it again.
                return request;
            };
            return true;
        """,
    )
    if installed is not True:
        raise WebDriverError("save failure probe was already installed")


def save_failure_state(base_url: str, session_id: str) -> dict[str, Any]:
    """Read only counters and flags from the failure probe."""

    state = execute_script(
        base_url,
        session_id,
        """
            const probe = window.__mantisSaveFailureProbe;
            return probe === undefined ? null : {
                attempts: probe.attempts,
                requestSucceeded: probe.requestSucceeded,
                abortRequested: probe.abortRequested,
                aborted: probe.aborted,
                quotaErrorThrown: probe.quotaErrorThrown,
                requestError: probe.requestError,
                transactionError: probe.transactionError,
                transactionAttempts: probe.transactionAttempts,
                transactionStartError: probe.transactionStartError,
            };
        """,
    )
    if not isinstance(state, dict):
        raise WebDriverError("save failure probe is missing")
    return state


def exercise_save_failure(
    base_url: str, session_id: str, timeout: float, *, failure: str = "abort"
) -> dict[str, Any]:
    # Distinct commands keep each scenario from accepting an earlier
    # successful save as evidence that its own post-reload edit persisted.
    failed_radius, disabled_radius, recovered_radius = {
        "abort": (11, 13, 17),
        "quota": (19, 23, 29),
        "request-error": (31, 37, 41),
        "transaction-start": (43, 47, 53),
    }[failure]
    before = wait_for_workspace(
        base_url, session_id, timeout, "durable workspace before save failure",
        lambda _: True,
    )
    if any(
        contains_shape(before["pending"], component, radius)
        for component, radius in (
            ("circle", failed_radius), ("circle", disabled_radius),
            ("sphere", recovered_radius),
        )
    ):
        raise WebDriverError("save failure scenario commands already exist in durable work")
    install_save_failure(base_url, session_id, failure=failure)
    run_command(base_url, session_id, f"Circle {failed_radius}", timeout)
    deadline = time.monotonic() + timeout
    probe: dict[str, Any] = {}
    while time.monotonic() < deadline:
        probe = save_failure_state(base_url, session_id)
        if probe["aborted"] or probe["transactionStartError"] is not None:
            break
        time.sleep(0.1)
    if failure == "transaction-start":
        if not (
            probe.get("transactionStartError") == "InvalidStateError"
            and probe.get("transactionAttempts") == 1 and probe.get("attempts") == 0
        ) or any(probe.get(key) for key in (
            "requestSucceeded", "abortRequested", "aborted", "quotaErrorThrown",
            "requestError", "transactionError",
        )):
            raise WebDriverError(
                f"catalog transaction did not fail before its first write: {probe!r}"
            )
    elif failure == "request-error":
        if not (
            probe.get("requestError") == "ConstraintError"
            and probe.get("transactionError") and probe.get("aborted")
        ) or any(probe.get(key) for key in (
            "requestSucceeded", "abortRequested", "quotaErrorThrown"
        )):
            raise WebDriverError(
                f"catalog request error did not propagate and abort its transaction: {probe!r}"
            )
    elif failure == "quota":
        if not (probe.get("quotaErrorThrown") and probe.get("aborted")) or any(
            probe.get(key) for key in ("requestSucceeded", "abortRequested")
        ):
            raise WebDriverError(
                f"catalog write did not abort after a synchronous quota exception: {probe!r}"
            )
    elif not all(probe.get(key) for key in (
        "requestSucceeded", "abortRequested", "aborted"
    )):
        raise WebDriverError(
            f"catalog write did not reach an asynchronous transaction abort: {probe!r}"
        )
    if saved_workspace(base_url, session_id) != before:
        raise WebDriverError("failed catalog write changed the durable workspace")

    run_command(base_url, session_id, f"Circle {disabled_radius}", timeout)
    # app.rs AUTOSAVE_DEBOUNCE is 0.5 s. Observe well beyond it so the
    # second edit proves that the application disabled subsequent saves.
    deadline = time.monotonic() + 2.0
    expected_writes = 0 if failure == "transaction-start" else 1
    while time.monotonic() < deadline:
        probe = save_failure_state(base_url, session_id)
        if probe["attempts"] != expected_writes or probe["transactionAttempts"] != 1:
            raise WebDriverError("app retried catalog persistence after a save failure")
        time.sleep(0.1)
    if saved_workspace(base_url, session_id) != before:
        raise WebDriverError("editing after a save failure changed the durable workspace")

    # A page reload removes the hook and resets the application's storage
    # failure state. A new UI edit must save alongside only the durable work.
    reload_app(base_url, session_id, timeout)
    if execute_script(
        base_url, session_id,
        "return window.__mantisSaveFailureProbe === undefined;",
    ) is not True:
        raise WebDriverError("reload did not remove the save failure probe")
    run_command(base_url, session_id, f"Sphere {recovered_radius}", timeout)
    after = wait_for_workspace(
        base_url, session_id, timeout, f"post-failure reload Sphere {recovered_radius} autosave",
        lambda state: contains_shape(state["pending"], "sphere", recovered_radius),
    )
    if after["pending"][: len(before["pending"])] != before["pending"]:
        raise WebDriverError("save failure/reload lost previously durable pending operations")
    appended = after["pending"][len(before["pending"]) :]
    if not contains_shape(appended, "sphere", recovered_radius):
        raise WebDriverError("post-failure reload edit did not append pending operations")
    if any(
        contains_shape(after["pending"], "circle", radius)
        for radius in (failed_radius, disabled_radius)
    ):
        raise WebDriverError("reload unexpectedly restored edits made while saving failed")
    if any(after[key] != before[key] for key in ("activeId", "head", "blocks", "recovery")):
        raise WebDriverError("save failure/reload changed workspace, chain, or recovery state")
    return {
        **probe,
        "pendingBeforeFailure": len(before["pending"]),
        "pendingAfterReload": len(after["pending"]),
        "committedBlocks": after["blocks"],
    }


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
        help="Seconds to wait for each WASM startup, command completion, or workspace autosave",
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

        state = wait_for_app(args.webdriver, session_id, args.timeout)
        logs = browser_logs(args.webdriver, session_id)
        state["reloadRecovery"] = exercise_pending_reload(
            args.webdriver, session_id, args.timeout
        )
        state["saveFailure"] = exercise_save_failure(
            args.webdriver, session_id, args.timeout
        )
        state["synchronousSaveFailure"] = exercise_save_failure(
            args.webdriver, session_id, args.timeout, failure="quota"
        )
        state["requestSaveFailure"] = exercise_save_failure(
            args.webdriver, session_id, args.timeout, failure="request-error"
        )
        state["transactionStartSaveFailure"] = exercise_save_failure(
            args.webdriver, session_id, args.timeout, failure="transaction-start"
        )
        logs.extend(browser_logs(args.webdriver, session_id))
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
                logs.extend(browser_logs(args.webdriver, session_id))
            except (TimeoutError, urllib.error.URLError) as error:
                print(f"warning: failed to read browser logs: {error}", file=sys.stderr)
            print_browser_logs(logs)
            try:
                webdriver_request(
                    args.webdriver, "DELETE", f"session/{session_id}"
                )
            except (WebDriverError, TimeoutError, urllib.error.URLError) as error:
                print(f"warning: failed to close browser session: {error}", file=sys.stderr)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (WebDriverError, TimeoutError, urllib.error.URLError) as error:
        print(f"browser smoke failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error

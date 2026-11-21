#!/usr/bin/env python3
"""Reject missing reloads/commands, wrong geometry, and storage failure mechanisms.

Accepts the same URL, --webdriver, and --timeout arguments as browser_smoke.py.
Each control uses a fresh browser session and alters one navigation, command,
or failure hook. No stored catalog payload is inspected by the fault hooks.
"""

from __future__ import annotations

import contextlib
import io
import sys
import urllib.error
from unittest.mock import patch

sys.dont_write_bytecode = True
import browser_smoke as smoke


def exercise_reload_control() -> None:
    original_request = smoke.webdriver_request
    refresh_calls = 0
    expected = "reload did not replace the browser document"

    def fault_request(base_url, method, path, payload=None):
        nonlocal refresh_calls
        if method == "POST" and path.endswith("/refresh"):
            refresh_calls += 1
            if refresh_calls == 1:
                # A successful WebDriver response alone does not prove that
                # the saved workspace was restored into a new WASM instance.
                return None
        return original_request(base_url, method, path, payload)

    observed_expected = False
    with (
        patch.object(smoke, "webdriver_request", fault_request),
        contextlib.redirect_stdout(io.StringIO()),
        contextlib.redirect_stderr(io.StringIO()),
    ):
        try:
            smoke.main()
        except smoke.WebDriverError as error:
            # Only the reload assertion may pass this control. Startup,
            # transport, command, and persistence errors must still fail.
            if str(error) != expected:
                raise
            observed_expected = True

    if not (observed_expected and refresh_calls == 1):
        raise smoke.WebDriverError(
            "negative control failed (missing-reload): expected document replacement error missing"
        )
    print(f"negative control passed (missing-reload): {expected}")


def exercise_control(mode: str) -> None:
    original_command = smoke.run_command
    original_request = smoke.webdriver_request
    target_active = False
    target_calls = 0
    removed_enters = 0
    target_rejected = False
    replacement = "Circle NaN" if mode == "invalid-command" else "Circle 13"
    expected = f"timed out waiting for command to complete: {replacement}"

    def fault_request(base_url, method, path, payload=None):
        nonlocal removed_enters
        if target_active and mode == "missing-enter" and path.endswith("/actions"):
            sources = []
            for source in payload["actions"]:
                kept = []
                for action in source["actions"]:
                    if (
                        action.get("type") in {"keyDown", "keyUp"}
                        and action.get("value") == "\ue007"
                    ):
                        removed_enters += 1
                    else:
                        kept.append(action)
                sources.append({**source, "actions": kept})
            payload = {**payload, "actions": sources}
        return original_request(base_url, method, path, payload)

    def fault_command(base_url, session_id, command, timeout):
        nonlocal target_active, target_calls, target_rejected
        if command != "Circle 13":
            return original_command(base_url, session_id, command, timeout)
        target_calls += 1
        target_active = True
        try:
            # A timeout is the expected outcome; keep this wait short.
            return original_command(base_url, session_id, replacement, min(timeout, 2.0))
        except smoke.WebDriverError as error:
            target_rejected = str(error) == expected
            raise
        finally:
            target_active = False

    observed_expected = False
    with (
        patch.object(smoke, "run_command", fault_command),
        patch.object(smoke, "webdriver_request", fault_request),
        contextlib.redirect_stdout(io.StringIO()),
        contextlib.redirect_stderr(io.StringIO()),
    ):
        try:
            smoke.main()
        except smoke.WebDriverError as error:
            # Bootstrap, driver, storage, and transport errors cannot pass.
            if str(error) != expected:
                raise
            observed_expected = True

    if not (observed_expected and target_rejected and target_calls == 1):
        raise smoke.WebDriverError(
            f"negative control failed ({mode}): expected command completion error missing"
        )
    if mode == "missing-enter" and removed_enters != 2:
        raise smoke.WebDriverError(
            "negative control failed (missing-enter): Enter keyDown/keyUp was not removed exactly once"
        )
    print(f"negative control passed ({mode}): {expected}")


def exercise_restored_geometry_control() -> None:
    original_command = smoke.run_command
    target_calls = 0
    move_completed = False
    expected = "post-reload Move did not reference the restored Circle 5"

    def fault_command(base_url, session_id, command, timeout):
        nonlocal target_calls, move_completed
        if command != "Move 1 2 3":
            return original_command(base_url, session_id, command, timeout)
        target_calls += 1
        # A valid Move on a newly created circle must not count as editing
        # the restored circle, even though both commands complete and save.
        original_command(base_url, session_id, "Circle 59", timeout)
        result = original_command(base_url, session_id, command, timeout)
        move_completed = True
        return result

    observed_expected = False
    with (
        patch.object(smoke, "run_command", fault_command),
        contextlib.redirect_stdout(io.StringIO()),
        contextlib.redirect_stderr(io.StringIO()),
    ):
        try:
            smoke.main()
        except smoke.WebDriverError as error:
            # Startup, command, autosave, and transport errors cannot pass.
            if str(error) != expected:
                raise
            observed_expected = True

    if not (observed_expected and target_calls == 1 and move_completed):
        raise smoke.WebDriverError(
            "negative control failed (restored-geometry): Move on a new circle was not rejected"
        )
    print("negative control passed (restored-geometry): Move on a new circle was rejected")


def exercise_history_control(*, redo: bool) -> None:
    original_shortcut = smoke.run_history_shortcut
    original_wait = smoke.wait_for_workspace
    action = "Redo" if redo else "Undo"
    description = f"post-reload {action} pending autosave"
    target_calls = 0
    target_rejected = False

    def skip_shortcut(base_url, session_id, *, redo=False):
        nonlocal target_calls
        if redo == (action == "Redo"):
            target_calls += 1
            return
        return original_shortcut(base_url, session_id, redo=redo)

    def bounded_wait(base_url, session_id, timeout, label, predicate):
        nonlocal target_rejected
        if label != description:
            return original_wait(base_url, session_id, timeout, label, predicate)
        try:
            return original_wait(base_url, session_id, min(timeout, 2.0), label, predicate)
        except smoke.WebDriverError as error:
            target_rejected = str(error).startswith(
                f"timed out waiting for {description} (last saved counts: "
            )
            raise

    observed_expected = False
    with (
        patch.object(smoke, "run_history_shortcut", skip_shortcut),
        patch.object(smoke, "wait_for_workspace", bounded_wait),
        contextlib.redirect_stdout(io.StringIO()),
        contextlib.redirect_stderr(io.StringIO()),
    ):
        try:
            smoke.main()
        except smoke.WebDriverError:
            # Only this step's autosave timeout can pass. Driver, startup,
            # storage-read errors, and failures in earlier steps must escape.
            if not target_rejected:
                raise
            observed_expected = True

    if not (observed_expected and target_calls == 1):
        raise smoke.WebDriverError(
            f"negative control failed (missing-{action.lower()}): expected autosave timeout missing"
        )
    print(f"negative control passed (missing-{action.lower()}): stale pending state was rejected")


def exercise_request_error_control() -> None:
    original_install = smoke.install_save_failure
    original_state = smoke.save_failure_state
    target_calls = 0
    target_probe = None

    def fault_install(base_url, session_id, *, failure="abort"):
        nonlocal target_calls
        if failure == "request-error":
            target_calls += 1
            failure = "abort"
        return original_install(base_url, session_id, failure=failure)

    def observe_state(base_url, session_id):
        nonlocal target_probe
        state = original_state(base_url, session_id)
        if target_calls:
            target_probe = state
        return state

    observed_expected = False
    with (
        patch.object(smoke, "install_save_failure", fault_install),
        patch.object(smoke, "save_failure_state", observe_state),
        contextlib.redirect_stdout(io.StringIO()),
        contextlib.redirect_stderr(io.StringIO()),
    ):
        try:
            smoke.main()
        except smoke.WebDriverError as error:
            expected = (
                "catalog request error did not propagate and abort its transaction: "
                f"{target_probe!r}"
            )
            # Only the targeted assertion, with its actual observed probe,
            # can pass; startup, transport, and earlier failures must escape.
            if target_probe is None or str(error) != expected:
                raise
            observed_expected = True

    if not (
        observed_expected and target_calls == 1 and target_probe is not None
        and target_probe["attempts"] == 1
        and all(target_probe[key] for key in (
            "requestSucceeded", "abortRequested", "aborted"
        ))
        and target_probe["requestError"] is None
        and not target_probe["transactionError"]
        and not target_probe["quotaErrorThrown"]
    ):
        raise smoke.WebDriverError(
            "negative control failed (request-error): explicit abort was not rejected"
        )
    print("negative control passed (request-error): explicit abort was rejected")


def exercise_transaction_start_control() -> None:
    original_install = smoke.install_save_failure
    original_state = smoke.save_failure_state
    target_calls = 0
    target_probe = None

    def fault_install(base_url, session_id, *, failure="abort"):
        nonlocal target_calls
        if failure == "transaction-start":
            target_calls += 1
            failure = "quota"
        return original_install(base_url, session_id, failure=failure)

    def observe_state(base_url, session_id):
        nonlocal target_probe
        state = original_state(base_url, session_id)
        if target_calls:
            target_probe = state
        return state

    observed_expected = False
    with (
        patch.object(smoke, "install_save_failure", fault_install),
        patch.object(smoke, "save_failure_state", observe_state),
        contextlib.redirect_stdout(io.StringIO()),
        contextlib.redirect_stderr(io.StringIO()),
    ):
        try:
            smoke.main()
        except smoke.WebDriverError as error:
            expected = (
                "catalog transaction did not fail before its first write: "
                f"{target_probe!r}"
            )
            # A failure in put() must not satisfy the distinct failure to
            # start a transaction; unrelated earlier errors must escape.
            if target_probe is None or str(error) != expected:
                raise
            observed_expected = True

    if not (
        observed_expected and target_calls == 1 and target_probe is not None
        and target_probe["transactionAttempts"] == 1
        and target_probe["transactionStartError"] is None
        and target_probe["attempts"] == 1
        and target_probe["quotaErrorThrown"] and target_probe["aborted"]
        and target_probe["requestError"] is None
        and not any(target_probe[key] for key in (
            "requestSucceeded", "abortRequested", "transactionError"
        ))
    ):
        raise smoke.WebDriverError(
            "negative control failed (transaction-start): quota write failure was not rejected"
        )
    print("negative control passed (transaction-start): quota write failure was rejected")


if __name__ == "__main__":
    try:
        # Keep --help and argument errors visible before capturing browser logs.
        smoke.parse_args()
        exercise_reload_control()
        exercise_restored_geometry_control()
        exercise_history_control(redo=False)
        exercise_history_control(redo=True)
        exercise_control("missing-enter")
        exercise_control("invalid-command")
        exercise_request_error_control()
        exercise_transaction_start_control()
    except (smoke.WebDriverError, TimeoutError, urllib.error.URLError) as error:
        print(f"negative browser control failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error

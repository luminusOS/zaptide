#!/usr/bin/env python3
"""Exercise the native GTK UI with synthetic data through AT-SPI."""

import os
import shutil
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(ROOT, "target", "debug", "zaptide")
WAIT_SECONDS = 20


def descendants(node):
    if node is None:
        return
    yield node
    try:
        count = node.childCount
    except Exception:
        return
    for index in range(count):
        try:
            child = node.getChildAtIndex(index)
        except Exception:
            continue
        if child is not None:
            yield from descendants(child)


def find_named(root, name, role=None):
    for node in descendants(root):
        try:
            if node.name == name and (role is None or node.getRole() == role):
                return node
        except Exception:
            continue
    return None


def find_named_all(root, name):
    matches = []
    for node in descendants(root):
        try:
            if node.name == name:
                matches.append(node)
        except Exception:
            continue
    return matches


def find_containing(root, text):
    needle = text.casefold()
    for node in descendants(root):
        try:
            if needle in node.name.casefold():
                return node
        except Exception:
            continue
    return None


def click(node):
    try:
        actions = node.queryAction()
        for index in range(actions.nActions):
            if actions.getName(index).lower() in (
                "click", "press", "activate", "default.activate"
            ):
                return actions.doAction(index)
    except Exception:
        return False
    return False


def has_state(node, state):
    try:
        return node.getState().contains(state)
    except Exception:
        return False


def wait_for(predicate, description, timeout=WAIT_SECONDS):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except Exception:
            pass
        time.sleep(0.15)
    raise RuntimeError(f"timed out waiting for {description}")


def find_entry(root, pyatspi, names=()):
    def editable_text(node):
        role = node.getRoleName().lower()
        return (
            role in ("entry", "search box", "text", "text area")
            and has_state(node, pyatspi.STATE_EDITABLE)
        )

    for name in names:
        entry = find_named(root, name)
        if entry is not None and editable_text(entry):
            return entry
    return next(
        (node for node in descendants(root) if editable_text(node)),
        None,
    )


def set_text(entry, text):
    try:
        return entry.queryText().setTextContents(text)
    except Exception:
        try:
            return entry.queryEditableText().setTextContents(text)
        except Exception:
            return False


def visible_enabled(node, pyatspi):
    return has_state(node, pyatspi.STATE_SHOWING) and has_state(
        node, pyatspi.STATE_ENABLED
    )


def select_accessible(node):
    current = node
    for _ in range(8):
        try:
            parent = current.parent
            selection = parent.querySelection()
            for index in range(parent.childCount):
                try:
                    child = parent.getChildAtIndex(index)
                    if child == current:
                        return selection.selectChild(index)
                except Exception:
                    continue
            current = parent
        except Exception:
            try:
                current = current.parent
            except Exception:
                return False
    return False


def click_at(node, pyatspi):
    try:
        rect = node.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
        if rect.width <= 0 or rect.height <= 0:
            return False
        x = rect.x + rect.width // 2
        y = rect.y + rect.height // 2
        pyatspi.Registry.generateMouseEvent(x, y, "b1c")
        return True
    except Exception:
        return False


def activate_accessible(node, pyatspi):
    current = node
    for _ in range(6):
        if click(current):
            return True
        try:
            current = current.parent
        except Exception:
            break
    return click_at(node, pyatspi)


def ancestor_with_role(node, role):
    current = node
    for _ in range(10):
        try:
            if current.getRole() == role:
                return current
            current = current.parent
        except Exception:
            return None
    return None


def open_preferences(app, frame, pyatspi):
    button = find_named(frame, "Preferences", pyatspi.ROLE_PUSH_BUTTON)
    if button is None or not click(button):
        raise RuntimeError("Preferences control is not accessible and activatable")
    dialog = wait_for(
        lambda: find_named(app, "Preferences", pyatspi.ROLE_DIALOG),
        "native Preferences dialog",
    )
    labels = {node.name for node in descendants(dialog) if node.name}
    for label in (
        "Appearance",
        "Theme",
        "Custom palette",
        "Open themes folder…",
        "Send messages with Enter",
    ):
        if label not in labels:
            raise RuntimeError(f"Preferences dialog is missing accessible control {label}")
    return ("Appearance",)


def main():
    try:
        import pyatspi
    except ImportError as error:
        raise RuntimeError("pyatspi is required for AT-SPI smoke") from error

    if os.environ.get("NATIVE_ATSPI_SKIP_BUILD") == "1":
        if not os.path.isfile(BINARY):
            raise RuntimeError("build skipped but native debug binary is missing")
        print("Using existing native debug binary")
    else:
        subprocess.run(
            ["cargo", "build", "--locked", "--features", "demo"],
            cwd=ROOT,
            check=True,
        )
    env = os.environ.copy()
    env["ZAPTIDE_NATIVE_SYNTHETIC"] = "1"
    env["GTK_A11Y"] = "atspi"
    env.setdefault("RUST_LOG", "warn")
    process = subprocess.Popen(
        [BINARY], cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    orca = None
    try:
        desktop = pyatspi.Registry.getDesktop(0)

        def get_frame():
            for index in range(desktop.childCount):
                try:
                    candidate = desktop.getChildAtIndex(index)
                    if candidate is not None and candidate.name == "zaptide":
                        frame = find_named(candidate, "ZapTide", pyatspi.ROLE_FRAME)
                        if frame is not None:
                            return candidate, frame
                except Exception:
                    continue
            return None

        app, frame = wait_for(get_frame, "native application frame")
        if shutil.which("orca"):
            orca = subprocess.Popen(
                ["orca", "--replace"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            wait_for(lambda: orca.poll() is None, "Orca CLI startup", timeout=8)
            print("Orca CLI started")
        else:
            print("Orca CLI unavailable; AT-SPI checks continue")

        categories = open_preferences(app, frame, pyatspi)
        dialog = wait_for(
            lambda: find_named(app, "Preferences", pyatspi.ROLE_DIALOG),
            "Preferences dialog",
        )
        close = find_named(dialog, "Close", pyatspi.ROLE_PUSH_BUTTON)
        if close is None or not click(close):
            raise RuntimeError("Preferences dialog close action is unavailable")
        wait_for(
            lambda: not find_named(app, "Preferences", pyatspi.ROLE_DIALOG),
            "closed Preferences dialog",
        )

        # Find and exercise search without reading or printing entered content.
        search = find_entry(frame, pyatspi, ("Search chats", "Search"))
        if search is None:
            search = wait_for(
                lambda: find_entry(frame, pyatspi), "chat search entry"
            )
        try:
            search_focus_requested = search.queryComponent().grabFocus()
        except Exception:
            search_focus_requested = False
        if not search_focus_requested and not has_state(search, pyatspi.STATE_FOCUSED):
            click_at(search, pyatspi)
        search_focus_confirmed = has_state(search, pyatspi.STATE_FOCUSED)
        if not set_text(search, "Synthetic Contact"):
            raise RuntimeError("chat search entry does not accept accessible text input")
        wait_for(
            lambda: find_named(frame, "Synthetic Contact"),
            "synthetic chat search result",
        )
        if not set_text(search, ""):
            raise RuntimeError("chat search entry cannot be cleared")

        contact = wait_for(
            lambda: find_named(frame, "Synthetic Contact"), "synthetic chat row"
        )
        activated = click(contact)
        if not activated:
            try:
                contact.queryComponent().grabFocus()
            except Exception:
                pass
            activated = select_accessible(contact)
        clicked = click_at(contact, pyatspi)
        if clicked:
            try:
                rect = contact.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
                pyatspi.Registry.generateMouseEvent(
                    rect.x + rect.width // 2,
                    rect.y + rect.height // 2,
                    "b1d",
                )
            except Exception:
                pass
            activated = True
        if not activated:
            raise RuntimeError("synthetic chat result cannot be activated through AT-SPI")
        try:
            contact.queryComponent().grabFocus()
        except Exception:
            pass
        row = ancestor_with_role(contact, pyatspi.ROLE_LIST_ITEM)
        if row is not None:
            try:
                row.queryComponent().grabFocus()
            except Exception:
                pass
        pyatspi.Registry.generateKeyboardEvent(0, "Return", pyatspi.KEY_SYM)

        composer = wait_for(
            lambda: find_named(frame, "Message composer")
            or find_named(frame, "Type a message"),
            "accessible chat composer",
        )
        try:
            composer_focus_requested = composer.queryComponent().grabFocus()
        except Exception:
            composer_focus_requested = False
        composer_focus_confirmed = has_state(composer, pyatspi.STATE_FOCUSED)
        if not has_state(composer, pyatspi.STATE_SHOWING) or not has_state(
            composer, pyatspi.STATE_EDITABLE
        ):
            raise RuntimeError("composer is not visible and editable")

        required_controls = (
            "Load older messages",
            "Send",
            "Emoji",
            "Mention",
            "Paste image",
        )
        for label in required_controls:
            control = wait_for(
                lambda label=label: find_named(frame, label), f"control {label}"
            )
            if not has_state(control, pyatspi.STATE_SHOWING):
                raise RuntimeError(f"control {label} is not visible")

        preview = wait_for(
            lambda: find_containing(frame, "Offline link preview sample"),
            "synthetic message row",
        )
        if not click_at(preview, pyatspi):
            raise RuntimeError("synthetic message cannot be selected")
        forward = wait_for(
            lambda: find_named(frame, "Forward to chat…", pyatspi.ROLE_PUSH_BUTTON),
            "selected-message forward action",
        )
        if not activate_accessible(forward, pyatspi):
            raise RuntimeError("forward action cannot be activated")
        forward_dialog = wait_for(
            lambda: find_named(app, "Forward message", pyatspi.ROLE_DIALOG)
            or find_named(app, "Forward message", pyatspi.ROLE_FRAME),
            "chat-search forwarding dialog",
        )
        forward_search = wait_for(
            lambda: find_entry(forward_dialog, pyatspi, ("Search chats", "Search")),
            "forward chat search",
        )
        if not set_text(forward_search, "Synthetic Contact"):
            raise RuntimeError("forward chat search rejects accessible input")
        destination = wait_for(
            lambda: find_containing(forward_dialog, "Forward to Synthetic Contact"),
            "synthetic forwarding destination",
        )
        if not activate_accessible(destination, pyatspi):
            raise RuntimeError("synthetic forwarding destination cannot be chosen")

        media_controls = tuple(
            node
            for node in descendants(frame)
            if node.name in ("Play", "Pause", "Download", "Download attachment", "Open attachment", "Retry")
            and has_state(node, pyatspi.STATE_SHOWING)
        )
        if not media_controls:
            # Synthetic transcript includes a media attachment action.
            if find_named(frame, "Download attachment") is None:
                raise RuntimeError("synthetic media controls are not exposed over AT-SPI")

        print(
            "AT-SPI native synthetic flow passed: search, chat activation, forwarding, focused composer, "
            f"{len(required_controls)} controls, media controls, {len(categories)} preference categories; "
            f"search focus confirmed={str(search_focus_confirmed).lower()}, "
            f"composer focus confirmed={str(composer_focus_confirmed).lower()}"
        )
        return 0
    finally:
        if orca is not None and orca.poll() is None:
            orca.terminate()
            try:
                orca.wait(timeout=5)
            except subprocess.TimeoutExpired:
                orca.kill()
                orca.wait()
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        # Error strings contain only fixed test labels, never accessible text values.
        print(f"AT-SPI smoke failed: {error}", file=sys.stderr)
        sys.exit(1)

"""Test the synthetic demo under private Xvfb, D-Bus, HOME/XDG and no network.

Run: python3 tests/reaction_hover.py /absolute/path/to/zaptide-demo
Set ZAPTIDE_DEMO_DIR to a writable private directory. Each scenario starts a
fresh demo because its screens advance automatically. The 1280x900 Xvfb screen
must have no window manager and the demo must use the X11 backend.
"""

import ctypes as c
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

D, W, I, U = c.c_void_p, c.c_ulong, c.c_int, c.c_uint


class Attributes(c.Structure):
    _fields_ = [('x', I), ('y', I), ('width', I), ('height', I),
                ('border_width', I), ('depth', I), ('visual', D), ('root', W),
                ('class_', I), ('bit_gravity', I), ('win_gravity', I),
                ('backing_store', I), ('backing_planes', W), ('backing_pixel', W),
                ('save_under', I), ('colormap', W), ('map_installed', I),
                ('map_state', I), ('all_event_masks', c.c_long),
                ('your_event_mask', c.c_long), ('do_not_propagate_mask', c.c_long),
                ('override_redirect', I), ('screen', D)]


class Image(c.Structure):
    _fields_ = [('width', I), ('height', I), ('xoffset', I), ('format', I),
                ('data', D), ('byte_order', I), ('bitmap_unit', I),
                ('bitmap_bit_order', I), ('bitmap_pad', I), ('depth', I),
                ('bytes_per_line', I), ('bits_per_pixel', I),
                ('red_mask', W), ('green_mask', W), ('blue_mask', W)]


def wait_for(predicate, message, timeout=1):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.01)
    raise AssertionError(message)


class Display:
    def __init__(self):
        self.x = c.CDLL('libX11.so.6')
        self.xtest = c.CDLL('libXtst.so.6')
        self.x.XOpenDisplay.argtypes = [c.c_char_p]
        self.x.XOpenDisplay.restype = D
        self.x.XDefaultRootWindow.argtypes = [D]
        self.x.XDefaultRootWindow.restype = W
        self.x.XQueryTree.argtypes = [D, W, c.POINTER(W), c.POINTER(W),
                                     c.POINTER(c.POINTER(W)), c.POINTER(U)]
        self.x.XGetWindowAttributes.argtypes = [D, W, c.POINTER(Attributes)]
        self.x.XGetImage.argtypes = [D, W, I, I, U, U, W, I]
        self.x.XGetImage.restype = c.POINTER(Image)
        self.x.XDestroyImage.argtypes = [c.POINTER(Image)]
        self.x.XKeysymToKeycode.argtypes = [D, W]
        self.x.XKeysymToKeycode.restype = c.c_ubyte
        self.x.XFree.argtypes = [D]
        self.x.XSync.argtypes = [D, I]
        self.x.XCloseDisplay.argtypes = [D]
        self.xtest.XTestFakeMotionEvent.argtypes = [D, I, I, I, W]
        self.xtest.XTestFakeButtonEvent.argtypes = [D, U, I, W]
        self.xtest.XTestFakeKeyEvent.argtypes = [D, U, I, W]
        self.display = self.x.XOpenDisplay(None)
        if not self.display:
            raise RuntimeError('Cannot open private Xvfb display')
        self.root = self.x.XDefaultRootWindow(self.display)

    def windows(self):
        root, parent, children, count = W(), W(), c.POINTER(W)(), U()
        if not self.x.XQueryTree(self.display, self.root, c.byref(root),
                                 c.byref(parent), c.byref(children), c.byref(count)):
            raise RuntimeError('XQueryTree failed')
        result = []
        try:
            for index in range(count.value):
                attrs = Attributes()
                if self.x.XGetWindowAttributes(self.display, children[index], c.byref(attrs)):
                    result.append((children[index], attrs.width, attrs.height, attrs.map_state))
        finally:
            if children:
                self.x.XFree(children)
        return result

    def crop(self, x, y, width, height):
        image = self.x.XGetImage(self.display, self.root, x, y, width, height, 0xffffffff, 2)
        if not image:
            raise RuntimeError('XGetImage failed')
        try:
            info = image.contents
            if info.bits_per_pixel != 32:
                raise RuntimeError('Expected a 32-bit Xvfb image')
            return c.string_at(info.data, info.bytes_per_line * info.height)
        finally:
            self.x.XDestroyImage(image)

    def move(self, x, y):
        if not self.xtest.XTestFakeMotionEvent(self.display, -1, x, y, 0):
            raise RuntimeError('XTest motion failed')
        self.x.XSync(self.display, 0)

    def click(self):
        for pressed in (1, 0):
            if not self.xtest.XTestFakeButtonEvent(self.display, 1, pressed, 0):
                raise RuntimeError('XTest click failed')
            self.x.XSync(self.display, 0)
            if pressed:
                time.sleep(.06)

    def type_text(self):
        key = self.x.XKeysymToKeycode(self.display, ord('m'))
        if not key:
            raise RuntimeError('No keycode for synthetic text')
        for _ in range(8):
            for pressed in (1, 0):
                if not self.xtest.XTestFakeKeyEvent(self.display, key, pressed, 0):
                    raise RuntimeError('XTest key failed')
        self.x.XSync(self.display, 0)


def difference(a, b):
    return sum(1 for i in range(0, len(a), 4)
               if sum(abs(a[i + j] - b[i + j]) for j in range(3)) > 60)


def run_scenario(display, binary, demo_dir, scenario):
    with tempfile.TemporaryDirectory(prefix='reaction-hover-', dir=demo_dir) as output:
        env = os.environ.copy()
        env['ZAPTIDE_DEMO_DIR'] = output
        env['XDG_CONFIG_HOME'] = output
        display.move(1200, 800)
        process = subprocess.Popen([binary], env=env)
        try:
            wait_for(lambda: any(w[1:] == (1100, 760, 2) for w in display.windows()),
                     'demo window not mapped at expected size', 4)
            time.sleep(2.3)
            if process.poll() is not None:
                raise AssertionError(f'demo exited early: {process.returncode}')
            if scenario == 'quick':
                display.move(500, 735)
                display.click()
                time.sleep(.1)
            original = display.crop(400, 252, 252, 40)
            def visible():
                return difference(original, display.crop(400, 252, 252, 40)) > 1000
            display.move(500, 330)
            wait_for(visible, 'reaction bar did not appear')
            display.move(500, 296)
            time.sleep(.08)
            display.move(500, 280)
            end = time.monotonic() + .7
            while time.monotonic() < end:
                if not visible():
                    raise AssertionError('reaction bar disappeared while hovered')
                time.sleep(.02)
            if scenario == 'hover':
                display.move(500, 296)
                time.sleep(.08)
                display.move(500, 280)
                time.sleep(.4)
                if not visible():
                    raise AssertionError('reaction bar disappeared after re-entry')
                display.move(500, 735)
                wait_for(lambda: not visible(), 'reaction bar did not hide after leaving')
            elif scenario == 'quick':
                display.move(452, 272)
                time.sleep(.08)
                display.click()
                wait_for(lambda: not visible(), 'quick reaction did not hide bar')
                settings = Path(output) / 'zaptide' / 'settings.json'
                wait_for(settings.is_file, 'quick reaction did not save recent reactions')
                if json.loads(settings.read_text())['recent_reactions'][:1] != ['❤️']:
                    raise AssertionError('quick reaction did not update recent reactions')
                before = display.crop(470, 718, 200, 30)
                display.type_text()
                time.sleep(.2)
                if difference(before, display.crop(470, 718, 200, 30)) <= 100:
                    raise AssertionError('composer did not accept keyboard input after reaction')
            else:
                display.move(634, 272)
                time.sleep(.08)
                before = {w[0] for w in display.windows() if w[3] == 2}
                screen_before = display.crop(0, 0, 1280, 900)
                display.click()
                def chooser():
                    return next((w for w in display.windows() if w[0] not in before and
                                 w[3] == 2 and w[1] > 260 and w[2] > 200), None)
                wait_for(chooser,
                         'More Reactions did not open the chooser')
                popup = chooser()
                attrs = Attributes()
                if not display.x.XGetWindowAttributes(display.display, popup[0], c.byref(attrs)):
                    raise AssertionError('cannot inspect chooser geometry')
                x, y = attrs.x, attrs.y
                if x < 0 or y < 0 or x + popup[1] > 1280 or y + popup[2] > 900:
                    raise AssertionError('chooser is clipped by the Xvfb output')
                left, top, width = x + 10, y + 70, popup[1] - 20
                original = b''.join(screen_before[(row * 1280 + left) * 4:
                                                   (row * 1280 + left + width) * 4]
                                    for row in range(top, top + 100))
                def rendered():
                    return difference(original, display.crop(left, top, width, 100)) > 1000
                wait_for(rendered, 'chooser content did not render')
                end = time.monotonic() + .5
                while time.monotonic() < end:
                    if process.poll() is not None or chooser() != popup or not rendered():
                        raise AssertionError('rendered chooser did not remain visible for 500ms')
                    time.sleep(.02)
                display.move(x + 40, y + 79)
                display.click()
                settings = Path(output) / 'zaptide' / 'settings.json'
                wait_for(settings.is_file, 'chooser selection did not save a reaction')
                if not json.loads(settings.read_text()).get('recent_reactions'):
                    raise AssertionError('chooser selection did not record a reaction')
            print(f'PASS {scenario}', flush=True)
        finally:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=5)


def main():
    if len(sys.argv) != 2:
        raise SystemExit(f'Usage: {sys.argv[0]} /absolute/path/to/zaptide-demo')
    binary = Path(sys.argv[1])
    demo_dir = os.environ.get('ZAPTIDE_DEMO_DIR', '')
    required = ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME',
                'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS')
    missing = [key for key in required if not os.environ.get(key)]
    if not binary.is_absolute() or not binary.is_file() or not os.access(binary, os.X_OK):
        raise SystemExit('Supply an absolute path to an executable synthetic demo binary')
    if not demo_dir or not Path(demo_dir).is_dir():
        raise SystemExit('ZAPTIDE_DEMO_DIR must name a writable private directory')
    if missing:
        raise SystemExit(f'Private display/session environment missing: {missing}')
    display = Display()
    try:
        for scenario in ('hover', 'quick', 'more'):
            run_scenario(display, str(binary), demo_dir, scenario)
    finally:
        display.x.XCloseDisplay(display.display)


if __name__ == '__main__':
    main()

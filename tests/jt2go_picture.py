"""Render a JT file in Siemens JT2Go and measure what the viewer actually draws.

This is the external oracle for the JT this workspace writes: a file can parse
here, pass ``jtinfo``, and still show a strict consumer an empty picture. That
is exactly the class of bug this script catches — a rewritten file that reports
a root node and a healthy triangle count in its key/value metadata but draws
nothing at all.

JT2Go is driven without a single synthetic input event: the file is handed on
the command line, the viewer fits it on its own, and the picture is taken with
``PrintWindow`` (``PW_RENDERFULLCONTENT``), which also works when the window is
minimised, covered, or on another monitor. Each file gets its own process, so a
viewer that wedges on one file cannot poison the next.

Run it directly::

    .\\.venv\\Scripts\\python.exe tests\\jt2go_picture.py data\\ExampleFiles\\example_block_jt10.3.jt
    .\\.venv\\Scripts\\python.exe tests\\jt2go_picture.py target\\out.jt --out tests\\_jt2go_out
    .\\.venv\\Scripts\\python.exe tests\\jt2go_picture.py data\\ExampleFiles\\*.jt --min-ink 5000

or through pytest, which skips when JT2Go is not installed::

    .\\.venv\\Scripts\\python.exe -m pytest tests\\test_jt2go.py -q

Exit code: 0 when every file drew geometry, 1 when one of them did not, 2 when
the viewer itself could not be used.
"""

from __future__ import annotations

import argparse
import ctypes
import glob
import os
import subprocess
import sys
import time
from ctypes import wintypes
from pathlib import Path

# --------------------------------------------------------------------------- #
# configuration
# --------------------------------------------------------------------------- #

#: where the viewer binary lives when nothing is configured. A Teamcenter-style
#: user install puts it under the profile rather than under ``Program Files``,
#: and the binary is called ``VisView_NG.exe``, not the ``visview.exe`` of the
#: older releases.
DEFAULT_EXE_GLOBS = [
    r"C:\Users\*\JT2Go\Products\JT2Go\VisView*.exe",
    r"C:\Program Files\Siemens\**\VisView*.exe",
    r"C:\Program Files (x86)\Siemens\**\VisView*.exe",
    r"C:\Tools\**\VisView*.exe",
]

#: how long the frame window takes to appear after the process starts
STARTUP_TIMEOUT = 60.0

#: how long a model may take to reach the viewport (assemblies need minutes)
LOAD_TIMEOUT = 120.0

#: two grabs a second apart must agree this closely before the picture is used
STABLE_RATIO = 0.02

#: a pixel is geometry when it differs from the viewport backdrop by more than this
INK_EPS = 24

#: how much of the viewport has to be geometry for a file to count as drawn
DEFAULT_MIN_INK = 2000

#: the window class of the drawing surface inside a JT2Go document
VIEW_CLASS = "DMUtils"

WIN = sys.platform == "win32"


class Jt2GoError(RuntimeError):
    """JT2Go could not be found, started, or captured."""


# --------------------------------------------------------------------------- #
# win32
# --------------------------------------------------------------------------- #

if WIN:  # pragma: no cover - exercised only on Windows
    user32 = ctypes.windll.user32
    gdi32 = ctypes.windll.gdi32

    #: the callback object has to outlive the enumeration, hence a module global
    _ENUM_PROC = ctypes.WINFUNCTYPE(ctypes.c_bool, wintypes.HWND, wintypes.LPARAM)

    SW_MAXIMIZE = 3
    PW_RENDERFULLCONTENT = 2

    class _BITMAPINFOHEADER(ctypes.Structure):
        _fields_ = [
            ("biSize", wintypes.DWORD),
            ("biWidth", ctypes.c_long),
            ("biHeight", ctypes.c_long),
            ("biPlanes", wintypes.WORD),
            ("biBitCount", wintypes.WORD),
            ("biCompression", wintypes.DWORD),
            ("biSizeImage", wintypes.DWORD),
            ("biXPelsPerMeter", ctypes.c_long),
            ("biYPelsPerMeter", ctypes.c_long),
            ("biClrUsed", wintypes.DWORD),
            ("biClrImportant", wintypes.DWORD),
        ]

    def set_dpi_aware() -> None:
        """Ask for per-monitor DPI so window rects are physical pixels."""

        try:  # Windows 10 1604 and newer
            user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
        except AttributeError:  # pragma: no cover - older Windows
            user32.SetProcessDPIAware()

    def window_title(hwnd: int) -> str:
        length = user32.GetWindowTextLengthW(hwnd)
        buf = ctypes.create_unicode_buffer(length + 1)
        user32.GetWindowTextW(hwnd, buf, length + 1)
        return buf.value

    def window_class(hwnd: int) -> str:
        buf = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, buf, 256)
        return buf.value

    def window_rect(hwnd: int) -> tuple[int, int, int, int]:
        rect = wintypes.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        return rect.left, rect.top, rect.right, rect.bottom

    def window_pid(hwnd: int) -> int:
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        return pid.value

    def top_windows() -> list[int]:
        """Every visible top-level window of the desktop."""

        found: list[int] = []

        def callback(hwnd: int, _: int) -> bool:
            if user32.IsWindowVisible(hwnd):
                found.append(hwnd)
            return True

        user32.EnumWindows(_ENUM_PROC(callback), 0)
        return found

    def child_windows(hwnd: int) -> list[int]:
        """Every descendant of ``hwnd`` — ``EnumChildWindows`` recurses by itself."""

        found: list[int] = []

        def callback(child: int, _: int) -> bool:
            found.append(child)
            return True

        user32.EnumChildWindows(hwnd, _ENUM_PROC(callback), 0)
        return found

    def print_window(hwnd: int) -> bytes | None:
        """Grab ``hwnd`` into a top-down RGBA buffer. ``None`` when the grab failed.

        ``PW_RENDERFULLCONTENT`` is what makes the OpenGL viewport show up: the
        plain flag returns the frame only, and a screen shot would miss the
        window entirely because JT2Go creates it off screen.
        """

        left, top, right, bottom = window_rect(hwnd)
        width, height = right - left, bottom - top
        if width <= 0 or height <= 0:
            return None

        source = user32.GetWindowDC(hwnd)
        if not source:
            return None
        bitmap = memory = previous = None
        try:
            bitmap = gdi32.CreateCompatibleBitmap(source, width, height)
            memory = gdi32.CreateCompatibleDC(source)
            previous = gdi32.SelectObject(memory, bitmap)
            if not bitmap or not memory or not user32.PrintWindow(hwnd, memory, PW_RENDERFULLCONTENT):
                return None

            info = _BITMAPINFOHEADER()
            info.biSize = ctypes.sizeof(_BITMAPINFOHEADER)
            info.biWidth = width
            info.biHeight = -height  # negative asks for a top-down DIB
            info.biPlanes = 1
            info.biBitCount = 32
            buffer = ctypes.create_string_buffer(_stride(width) * height)
            if gdi32.GetDIBits(memory, bitmap, 0, height, buffer, ctypes.byref(info), 0) == 0:
                return None
            return buffer.raw
        finally:
            if memory and previous:
                gdi32.SelectObject(memory, previous)
            if memory:
                gdi32.DeleteDC(memory)
            if bitmap:
                gdi32.DeleteObject(bitmap)
            user32.ReleaseDC(hwnd, source)
else:  # pragma: no cover - the viewer only exists on Windows

    def set_dpi_aware() -> None:
        raise Jt2GoError("JT2Go only runs on Windows")

    def top_windows() -> list[int]:
        raise Jt2GoError("JT2Go only runs on Windows")

    def child_windows(hwnd: int) -> list[int]:
        raise Jt2GoError("JT2Go only runs on Windows")

    def print_window(hwnd: int) -> bytes | None:
        raise Jt2GoError("JT2Go only runs on Windows")


# --------------------------------------------------------------------------- #
# finding the viewer
# --------------------------------------------------------------------------- #


def find_viewer(explicit: str | None = None) -> str:
    """Locate the viewer: ``--exe``, then ``JT2GO_EXE``, then the usual places."""

    if not WIN:
        raise Jt2GoError("JT2Go only runs on Windows")

    for candidate in (explicit, os.environ.get("JT2GO_EXE")):
        if candidate:
            path = Path(candidate)
            if path.is_file():
                return str(path.resolve())
            raise Jt2GoError(f"no such viewer: {candidate}")

    for pattern in DEFAULT_EXE_GLOBS:
        for hit in sorted(glob.glob(pattern, recursive=True)):
            if Path(hit).is_file():
                return hit

    shortcuts = _shortcut_targets()
    if shortcuts:
        raise Jt2GoError(
            "a JT2Go shortcut exists but its target is missing: "
            + "; ".join(f"{name} -> {target}" for name, target in sorted(shortcuts.items()))
            + " — pass --exe or set JT2GO_EXE"
        )
    raise Jt2GoError("no JT2Go (VisView*.exe) found; pass --exe or set JT2GO_EXE")


def _shortcut_targets() -> dict[str, str]:
    """Every JT2Go entry in the Start Menu, as ``{name: target path}``."""

    roots = [
        Path(os.environ.get("ProgramData", r"C:\ProgramData"))
        / "Microsoft"
        / "Windows"
        / "Start Menu"
        / "Programs",
        Path.home() / "AppData" / "Roaming" / "Microsoft" / "Windows" / "Start Menu" / "Programs",
    ]
    found: dict[str, str] = {}
    for root in roots:
        if not root.is_dir():
            continue
        for link in root.rglob("*.lnk"):
            if "jt2go" not in link.stem.lower():
                continue
            target = _read_shortcut(link)
            if target:
                found[link.stem] = target
    return found


def _read_shortcut(link: Path) -> str | None:
    """Resolve a ``.lnk`` — pywin32 is not installed, so ask PowerShell for it."""

    script = f"(New-Object -ComObject WScript.Shell).CreateShortcut('{link}').TargetPath"
    try:
        out = subprocess.run(
            ["powershell", "-NoProfile", "-Command", script],
            capture_output=True,
            text=True,
            timeout=30,
            check=True,
        )
    except (OSError, subprocess.SubprocessError):  # pragma: no cover - no PowerShell
        return None
    return out.stdout.strip() or None


# --------------------------------------------------------------------------- #
# measuring a picture
# --------------------------------------------------------------------------- #


def _stride(width: int) -> int:
    """The row length of a 32-bit DIB, padded to four bytes as ``GetDIBits`` writes it."""

    return ((width * 4 + 3) // 4) * 4


def _array(raw: bytes, width: int, height: int):
    """The grab as an ``(height, width, 3)`` uint8 array with the padding dropped."""

    import numpy as np

    view = np.frombuffer(raw, dtype=np.uint8).reshape(height, _stride(width))
    return view[:, : width * 4].reshape(height, width, 4)[:, :, :3]


def measure(raw: bytes, width: int, height: int) -> dict[str, object]:
    """How much of a grab is geometry rather than the viewport backdrop.

    The backdrop is read from the corners instead of assumed white: JT2Go draws
    a vertical gradient, and grabbing the GL child keeps the outliner, the
    ribbon and the status bar out of the count altogether.
    """

    import numpy as np

    pixels = _array(raw, width, height)
    corners = pixels[[0, 0, height - 1, height - 1], [0, width - 1, 0, width - 1]]
    back = np.median(corners, axis=0)
    delta = np.abs(pixels.astype(np.int16) - back.astype(np.int16)).max(axis=2)
    mask = delta > INK_EPS
    ink = int(mask.sum())
    box = None
    if ink:
        rows = np.flatnonzero(mask.any(axis=1))
        cols = np.flatnonzero(mask.any(axis=0))
        box = (int(cols[0]), int(rows[0]), int(cols[-1] - cols[0] + 1), int(rows[-1] - rows[0] + 1))
    return {
        "ink": ink,
        "fraction": ink / (width * height),
        "background": tuple(int(v) for v in back),
        "box": box,
    }


def similarity(a: bytes, b: bytes, width: int, height: int) -> float:
    """``1 - mean|delta| / 255`` between two grabs, the metric of ``test_visual.py``."""

    import numpy as np

    first = _array(a, width, height).astype(np.int16)
    second = _array(b, width, height).astype(np.int16)
    return float(1.0 - np.abs(first - second).max(axis=2).mean() / 255.0)


def save_png(path: Path, raw: bytes, width: int, height: int) -> None:
    """Write the grab out as an RGB PNG."""

    from PIL import Image

    path.parent.mkdir(parents=True, exist_ok=True)
    image = Image.frombytes("RGBA", (width, height), raw, "raw", "RGBA", _stride(width))
    image.convert("RGB").save(path)


def load_png(path: Path) -> tuple[bytes, int, int]:
    """Read a PNG back in the layout ``print_window`` returns (RGBA, top-down)."""

    from PIL import Image

    image = Image.open(path).convert("RGBA")
    width, height = image.size
    return image.tobytes(), width, height


# --------------------------------------------------------------------------- #
# driving the viewer
# --------------------------------------------------------------------------- #


def find_frame(pids: set[int] | None = None) -> int:
    """The JT2Go frame window, or ``0`` while it does not exist yet.

    The window class is an unstable MFC string that changes every run, so the
    title prefix is the only reliable handle — the UI language varies, the
    prefix does not.
    """

    for hwnd in top_windows():
        if not window_title(hwnd).startswith("JT2Go"):
            continue
        if pids is not None and window_pid(hwnd) not in pids:
            continue
        return hwnd
    return 0


def find_document(frame: int, stem: str) -> int:
    """The MDI document of ``stem``, or ``0`` while the file is not open yet.

    JT2Go names its document after the file, so the title turning up is the
    load-complete signal — far more reliable than a fixed sleep.
    """

    for child in child_windows(frame):
        title = window_title(child)
        if title == stem or title.startswith(f"{stem}."):
            return child
    return 0


def find_viewport(document: int) -> int:
    """The drawing surface inside a document."""

    for inner in child_windows(document):
        if window_class(inner) == VIEW_CLASS:
            return inner
    return document


def render(
    path: Path,
    exe: str,
    *,
    timeout: float = LOAD_TIMEOUT,
    maximize: bool = True,
    full_window: bool = False,
    stable: bool = True,
) -> tuple[bytes, int, int]:
    """Open one file in a fresh viewer and return a stable grab of its picture."""

    set_dpi_aware()
    try:
        process = subprocess.Popen([exe, str(path)])
    except OSError as exc:
        raise Jt2GoError(f"cannot start {exe}: {exc}") from exc

    try:
        deadline = time.monotonic() + STARTUP_TIMEOUT
        frame = 0
        while time.monotonic() < deadline:
            # a second invocation can be forwarded to an instance that is still
            # up, so fall back to any JT2Go frame once ours does not appear
            frame = find_frame({process.pid}) or find_frame()
            if frame:
                break
            time.sleep(0.5)
        if not frame:
            raise Jt2GoError(f"{exe} never showed a JT2Go window within {STARTUP_TIMEOUT:.0f} s")

        if maximize:
            # The frame is created off screen at negative coordinates, so a grab
            # before this returns an empty or useless rect.
            user32.ShowWindow(frame, SW_MAXIMIZE)
            time.sleep(1.0)

        deadline = time.monotonic() + timeout
        document = 0
        while time.monotonic() < deadline:
            document = find_document(frame, path.stem)
            if document:
                break
            time.sleep(0.5)
        if not document:
            raise Jt2GoError(f"JT2Go never loaded {path.name} (timeout after {timeout:.0f} s)")

        target = frame if full_window else find_viewport(document)
        best: tuple[bytes, int, int] | None = None
        for _ in range(6):
            left, top, right, bottom = window_rect(target)
            width, height = right - left, bottom - top
            raw = print_window(target)
            if raw is None or width <= 0 or height <= 0:
                time.sleep(1.0)
                continue
            if best is None:
                best = (raw, width, height)
                time.sleep(1.0)
                continue
            if not stable:
                return raw, width, height
            before = int(measure(best[0], best[1], best[2])["ink"])
            after = int(measure(raw, width, height)["ink"])
            span = max(before, after)
            if span == 0 or abs(before - after) / span < STABLE_RATIO:
                return raw, width, height
            best = (raw, width, height)
            time.sleep(1.0)
        if best is None:
            raise Jt2GoError(f"PrintWindow returned nothing for the picture of {path.name}")
        return best
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:  # pragma: no cover - stubborn viewer
                pass


# --------------------------------------------------------------------------- #
# the check
# --------------------------------------------------------------------------- #


def check(
    files: list[Path],
    *,
    exe: str | None = None,
    out_dir: Path | None = None,
    baseline_dir: Path | None = None,
    threshold: float = 0.99,
    min_ink: int = DEFAULT_MIN_INK,
    timeout: float = LOAD_TIMEOUT,
    full_window: bool = False,
    maximize: bool = True,
    verbose: bool = True,
) -> list[dict[str, object]]:
    """Render every file in JT2Go and report what each one drew.

    A file passes when its viewport holds at least ``min_ink`` pixels of
    geometry. With ``baseline_dir``, a previous picture named ``<stem>.png``
    there must also match to within ``threshold``.
    """

    for path in files:
        if not path.is_file():
            raise Jt2GoError(f"no such file: {path}")

    viewer = find_viewer(exe)
    results: list[dict[str, object]] = []
    for path in files:
        record: dict[str, object] = {"file": str(path)}
        try:
            raw, width, height = render(
                path,
                viewer,
                timeout=timeout,
                maximize=maximize,
                full_window=full_window,
            )
            stats = measure(raw, width, height)
            record.update(
                {
                    "width": width,
                    "height": height,
                    "ink": int(stats["ink"]),
                    "fraction": float(stats["fraction"]),
                    "box": stats["box"],
                    "ok": int(stats["ink"]) >= min_ink,
                }
            )
            if out_dir is not None:
                picture = out_dir / f"{path.stem}.png"
                save_png(picture, raw, width, height)
                record["picture"] = str(picture)
            if baseline_dir is not None:
                reference = baseline_dir / f"{path.stem}.png"
                if reference.is_file():
                    other, other_w, other_h = load_png(reference)
                    if (other_w, other_h) == (width, height):
                        score = similarity(raw, other, width, height)
                        record["similarity"] = score
                        record["ok"] = bool(record["ok"]) and score >= threshold
                    else:
                        record["similarity"] = None
                        record["note"] = f"baseline {reference.name} is {other_w}x{other_h}"
                else:
                    record["similarity"] = None
                    record["note"] = "no baseline yet"
        except Jt2GoError as exc:
            record.update({"ok": False, "error": str(exc), "ink": 0, "fraction": 0.0, "box": None})
        results.append(record)
        if verbose:
            print(_line(record))
    return results


def _line(record: dict[str, object]) -> str:
    name = Path(str(record["file"])).name
    verdict = "PASS" if record["ok"] else "FAIL"
    if record.get("error"):
        return f"{verdict}  {name}: {record['error']}"
    box = record["box"]
    extent = f"{box[2]}x{box[3]}" if box else "nothing"
    text = (
        f"{verdict}  {name}: {record['ink']} ink px of {record['width']}x{record['height']} "
        f"({100 * float(record['fraction']):.2f} %), geometry {extent}"
    )
    if record.get("similarity") is not None:
        text += f", similarity {float(record['similarity']):.5f}"
    if record.get("note"):
        text += f" [{record['note']}]"
    return text


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Render JT files in Siemens JT2Go and measure the picture.",
    )
    parser.add_argument("files", nargs="*", type=Path, help="the .jt files to render")
    parser.add_argument("--exe", help="path to VisView_NG.exe (default: discover it)")
    parser.add_argument("--out", type=Path, help="write a PNG of each picture here")
    parser.add_argument(
        "--baseline",
        type=Path,
        help="compare each picture against <dir>/<stem>.png",
    )
    parser.add_argument(
        "--write-baseline",
        type=Path,
        help="write the pictures to <dir> and compare them against it",
    )
    parser.add_argument("--threshold", type=float, default=0.99, help="similarity to accept (default 0.99)")
    parser.add_argument("--min-ink", type=int, default=DEFAULT_MIN_INK, help="ink pixels required")
    parser.add_argument("--timeout", type=float, default=LOAD_TIMEOUT, help="seconds to wait for a load")
    parser.add_argument(
        "--full-window",
        action="store_true",
        help="measure the whole window instead of only the 3D viewport",
    )
    parser.add_argument("--no-maximize", action="store_true", help="leave the window at its own size")
    parser.add_argument("--locate", action="store_true", help="print the viewer path and exit")
    args = parser.parse_args(argv)

    try:
        exe = find_viewer(args.exe)
    except Jt2GoError as exc:
        print(f"jt2go: {exc}", file=sys.stderr)
        return 2

    if args.locate:
        print(exe)
        return 0
    if not args.files:
        parser.error("no files given")

    out_dir = args.out or args.write_baseline
    baseline_dir = args.baseline or args.write_baseline
    try:
        results = check(
            args.files,
            exe=exe,
            out_dir=out_dir,
            baseline_dir=baseline_dir,
            threshold=args.threshold,
            min_ink=args.min_ink,
            timeout=args.timeout,
            full_window=args.full_window,
            maximize=not args.no_maximize,
        )
    except Jt2GoError as exc:
        print(f"jt2go: {exc}", file=sys.stderr)
        return 2

    failed = [r for r in results if not r["ok"]]
    print(f"{len(results) - len(failed)} of {len(results)} rendered")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

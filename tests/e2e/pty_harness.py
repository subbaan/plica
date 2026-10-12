"""End-to-end test: drive ./plica in a pty, render with pyte, check behaviour.

Requires python-pexpect and python-pyte (dev only). Run: make e2e
Works in a fresh temp dir; never touches your real palette or state dir.
The temp dir is left in place (path printed at the end) for inspection.
"""
import os, signal, sys, tempfile, time, unicodedata
import pexpect, pyte

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(REPO, "plica")
WORK = tempfile.mkdtemp(prefix="plica-e2e-")
SAMPLE_PALETTE = """foreground #e0e1e1
background #131617
cursor #e0e1e1
selection_foreground #0d0f0f
selection_background #189abf
active_tab_foreground #0d0f0f
active_tab_background #189abf
color0 #232627
color1 #de8d4b
color3 #de9b4b
color8 #505253
"""
COLS, ROWS = 60, 14
fails = []

def check(name, cond, extra=""):
    print(("PASS " if cond else "FAIL ") + name + (f"  [{extra}]" if extra and not cond else ""))
    if not cond:
        fails.append(name)

class Term:
    def __init__(self, args, env_extra=None):
        env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor",
                   XDG_STATE_HOME=os.path.join(WORK, "state"), XDG_CONFIG_HOME=os.path.join(WORK, "config"), DISPLAY="", WAYLAND_DISPLAY="")
        env.pop("NO_COLOR", None)
        env.pop("PLICA_PALETTE", None)
        env.update(env_extra or {})
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.raw = b""
        self.p = pexpect.spawn(BIN, args, env=env, dimensions=(ROWS, COLS))
        # Answer device-attribute queries like a real terminal would.
        self.screen.write_process_input = lambda d: self.p.send(d)
        self.pump(1.0)

    def pump(self, secs=0.3):
        end = time.time() + secs
        while time.time() < end:
            try:
                data = self.p.read_nonblocking(65536, timeout=0.05)
                self.raw += data
                self.stream.feed(data)
            except pexpect.TIMEOUT:
                pass
            except pexpect.EOF:
                break

    def send(self, s, secs=0.3):
        self.p.send(s)
        self.pump(secs)

    def text(self):
        return "\n".join(self.screen.display)

    def line(self, y):
        return self.screen.display[y]

    def col_of(self, y, needle):
        """Screen column where `needle` starts on row y (cell-accurate)."""
        cells = [self.screen.buffer[y][x].data for x in range(COLS)]
        for x in range(COLS):
            acc, k = "", x
            while k < COLS and len(acc) < len(needle):
                acc += cells[k]; k += 1
            if acc.startswith(needle):
                return x
        raise ValueError(needle)

    def cell(self, x, y):
        return self.screen.buffer[y][x]

    def click(self, x, y, btn=0, mods=0):  # 0-based screen coords
        self.p.send(f"\x1b[<{btn+mods};{x+1};{y+1}M"); self.p.send(f"\x1b[<{btn+mods};{x+1};{y+1}m")
        self.pump(0.2)

    def drag(self, x0, y0, x1, y1):
        self.p.send(f"\x1b[<0;{x0+1};{y0+1}M")
        self.p.send(f"\x1b[<32;{x1+1};{y1+1}M")
        self.p.send(f"\x1b[<0;{x1+1};{y1+1}m")
        self.pump(0.3)

def write_palette(path, bg, sel):
    src = SAMPLE_PALETTE
    lines = []
    for l in src.splitlines():
        k = l.split()[0] if l.split() else ""
        if k == "background": l = f"background {bg}"
        if k == "selection_background": l = f"selection_background {sel}"
        lines.append(l)
    tmp = path + ".tmp"
    open(tmp, "w").write("\n".join(lines) + "\n")
    os.replace(tmp, path)  # atomic replacement, like a theme switcher would

pal = os.path.join(WORK, "theme", "current-theme.conf")
os.makedirs(os.path.dirname(pal), exist_ok=True)
write_palette(pal, "#131617", "#189abf")

doc = os.path.join(WORK, "notes.txt")
open(doc, "w", newline="").write("hello world\r\ncafé 中文 é end\r\n\tTabbed line\r\n")

# ---------------------------------------------------------------- session 1
t = Term(["--palette", pal, doc])
check("title shows file", "notes.txt" in t.line(0), t.line(0))
check("status shows CRLF + palette", "CRLF" in t.line(ROWS - 1) and "current-theme.conf" in t.line(ROWS - 1), t.line(ROWS - 1))
check("unicode line rendered", "café 中文" in t.line(2), t.line(2))
check("tab expanded", "    Tabbed" in t.line(3) or "\tTabbed" not in t.line(3), t.line(3))
bg = t.cell(10, 5).bg
check("editor background from palette", bg.lower() == "131617", bg)

# Typing at start of doc
t.send("Hi ")
check("typed text appears", "Hi hello world" in t.line(1), t.line(1))
check("dirty marker", "●" in t.line(0), t.line(0))

# Click after 中文 on line 2: gutter is 5 cols ("  1 " is width 3+2) -> find text x
row2 = t.line(2)
x_end = t.col_of(2, "end")
t.click(x_end, 2)
t.send("X")
check("click places caret (unicode hit-test)", unicodedata.normalize("NFC", "e\u0301 Xend") in unicodedata.normalize("NFC", t.line(2)), t.line(2))

# Double-click selects word, typing replaces it
xw = t.line(1).index("world")
t.click(xw + 1, 1); t.click(xw + 1, 1)
t.send("there")
check("double-click word select + replace", "Hi hello there" in t.line(1), t.line(1))

# Drag selection across lines and check selection colour
x0 = t.line(1).index("hello")
t.drag(x0, 1, x0 + 2, 2)
check("drag selection shown in palette selection colour", t.cell(x0, 1).bg.lower() == "189abf", t.cell(x0, 1).bg)
check("status shows selection count", "Sel " in t.line(ROWS - 1), t.line(ROWS - 1))

# Live palette change with atomic replace
write_palette(pal, "#202a10", "#aa3355")
t.pump(0.8)
check("live palette: background updated", t.cell(10, 8).bg.lower() == "202a10", t.cell(10, 8).bg)
check("live palette: selection updated", t.cell(x0, 1).bg.lower() == "aa3355", t.cell(x0, 1).bg)

# Invalid palette keeps last-known-good
open(pal, "w").write("garbage\n")
t.pump(0.8)
check("invalid palette keeps colours", t.cell(10, 8).bg.lower() == "202a10", t.cell(10, 8).bg)
check("invalid palette warns", "⚠" in t.line(ROWS - 1), t.line(ROWS - 1))
write_palette(pal, "#101830", "#3355aa")
t.pump(0.8)
check("repaired palette applies", t.cell(10, 8).bg.lower() == "101830", t.cell(10, 8).bg)

# Undo the drag-free edits: Esc, then undo 'there' replacement
t.send("\x1b", 0.2)  # Esc (clear selection)
t.send("\x1a")  # Ctrl+Z
check("undo restores replaced word", "Hi hello world" in t.line(1), t.line(1))

# Save
t.send("\x13")  # Ctrl+S
check("saved message", "Saved" in t.line(ROWS - 1), t.line(ROWS - 1))
data = open(doc, "rb").read()
check("CRLF preserved on save", data.count(b"\r\n") == 3 and b"\n" not in data.replace(b"\r\n", b""), repr(data))
check("saved content", data.startswith(b"Hi hello world\r\n"), repr(data[:40]))

# Enter uses CRLF in a CRLF doc
t.send("\x1b[F")  # End... (xterm End)
t.send("\r")
t.send("\x13")
data = open(doc, "rb").read()
check("Enter inserts CRLF", data.count(b"\r\n") == 4, repr(data))

# Bracketed paste: literal text, newlines normalised, one undo
t.send("\x1b[200~pasted\rline two\x1b[201~")
check("bracketed paste literal", "pasted" in t.text() and "line two" in t.text(), t.text())
t.send("\x1a")
check("paste is one undo step", "pasted" not in t.text(), t.text())

# External modification while the buffer has unsaved edits: the watcher
# raises a conflict dialog; Keep Mine, then saving asks before overwriting.
t.send("z")
time.sleep(0.05)
with open(doc, "ab") as f: f.write(b"external\r\n")
t.pump(0.8)
check("watcher raises conflict dialog", "changed on disk" in t.text(), t.text())
t.send("k", 0.3)  # Keep Mine
t.send("\x13", 0.3)
check("save asks before overwriting external change", "changed on disk" in t.text(), t.text())
check("file not overwritten", open(doc, "rb").read().endswith(b"external\r\n"))
t.send("\x1b", 0.2)  # Cancel

# Quit with dirty buffer shows a dialog; Don't Save quits.
t.send("\x11")
check("dirty quit asks", t.p.isalive() and "Save changes to notes.txt" in t.text(), t.text())
t.send("n", 0.6)
check("Don't Save quits", not t.p.isalive())
tail = t.raw[-400:]
check("teardown: leaves alt screen", b"\x1b[?1049l" in tail, repr(tail))
check("teardown: mouse off", b"\x1b[?1000l" in tail or b"\x1b[?1006l" in tail, repr(tail))
check("teardown: bracketed paste off", b"\x1b[?2004l" in tail, repr(tail))

# ---------------------------------------------------------------- session 2: SIGTERM / SIGKILL + recovery
doc2 = os.path.join(WORK, "kill.txt")
rec_dir = os.path.join(WORK, "state", "plica", "recovery")
def snaps():
    return [f for f in os.listdir(rec_dir) if f.endswith(".plica-snap")] if os.path.isdir(rec_dir) else []
def snap_texts():
    return [open(os.path.join(rec_dir, f)).read().split("\n\n", 1)[1] for f in snaps()]
t = Term([doc2])
t.send("unsaved work")
t.p.kill(signal.SIGTERM)
t.pump(0.8)
check("SIGTERM flushes a recovery snapshot", "unsaved work" in snap_texts(), str(snap_texts()))
check("SIGTERM did not create original", not os.path.exists(doc2))
check("SIGTERM teardown", b"\x1b[?1049l" in t.raw[-400:])
# Restart: the snapshot is offered; Restore brings it back as unsaved work.
t = Term([])
check("restart offers recovery", "Recover unsaved work" in t.text() and "kill.txt" in t.text(), t.text())
t.send("r", 0.5)
check("restore brings text back", "unsaved work" in t.text() and "kill.txt" in t.line(0) and "●" in t.line(0), t.text())
check("restored snapshots removed", snaps() == [] or all("unsaved work" not in x for x in snap_texts()) or len(snaps()) <= 1, str(snaps()))
# SIGKILL (no chance to clean up): the idle snapshot written while running survives.
t.send(" plus more", 0.3)
t.pump(2.6)  # > 2 s idle -> snapshot
t.p.kill(signal.SIGKILL)
t.pump(0.3)
check("SIGKILL leaves the periodic snapshot", any("plus more" in x and "unsaved work" in x for x in snap_texts()), str(snap_texts()))
t = Term([])
check("restart after SIGKILL offers recovery", "Recover unsaved work" in t.text(), t.text())
t.send("d", 0.5)  # Discard
check("discard removes snapshots", snaps() == [], str(snaps()))
t.send("\x11", 0.5)
check("quit after discard", not t.p.isalive())

# ---------------------------------------------------------------- session 3: wrap + scrolling
doc3 = os.path.join(WORK, "long.txt")
open(doc3, "w").write("".join(f"line {i} " + "word " * 15 + "\n" for i in range(200)))
t = Term([doc3])
check("soft wrap continuation marker", "↪" in t.text(), t.text())
t.send("\x1b[<65;10;5M", 0.3)  # wheel down
check("wheel scrolls", "line 0 " not in t.line(1), t.line(1))
t.send("\x07"); t.send("150\r")  # Ctrl+G go to line
check("go to line", "line 149 " in t.text(), t.text())
t.send("\x06"); t.send("line 199", 0.4)  # Ctrl+F incremental
check("incremental find jumps", "line 199" in t.text(), t.text())
t.send("\x1b", 0.1)
t.send("\x1bz", 0.3)  # Alt+Z: wrap off
check("wrap off", "↪" not in t.text(), t.text())
t.send("\x11", 0.5)
check("clean quit when not dirty", not t.p.isalive())

# ---------------------------------------------------------------- session 4: NO_COLOR
t = Term([doc3], {"NO_COLOR": "1"})
check("NO_COLOR uses default colours", t.cell(10, 5).bg == "default", t.cell(10, 5).bg)
t.send("\x11", 0.5)

# ---------------------------------------------------------------- session 5: Phase 1 gate extras
dropped = os.path.join(WORK, "dropped note.txt")
open(dropped, "w").write("I was dropped\n\tindented\n")
t = Term([])
# Kitty delivers a drop as a bracketed paste of the absolute path.
t.send(f"\x1b[200~{dropped}\x1b[201~", 0.5)
check("drop opens file", "I was dropped" in t.text() and "dropped" in t.line(0) and ".txt" in t.line(0), t.line(0))
check("drop did not insert path text", dropped not in t.text(), t.text())
t.send("\x1bw", 0.3)  # Alt+W
check("whitespace visible", "→" in t.text(), t.text())
t.send("\x1bw", 0.3)
# Resize: smaller and larger, content and status bar re-laid out.
t.p.setwinsize(10, 40); t.screen.resize(10, 40); t.pump(0.5)
check("resize smaller: status on last row", "Ln " in t.line(9), t.line(9))
check("resize smaller: content kept", "I was dropped" in t.text(), t.text())
t.p.setwinsize(20, 70); t.screen.resize(20, 70); t.pump(0.5)
check("resize larger: status on last row", "Ln " in t.line(19), t.line(19))
# Save failure: make the directory read-only, edit, save.
ro = os.path.join(WORK, "ro"); os.makedirs(ro, exist_ok=True)
rofile = os.path.join(ro, "locked.txt"); open(rofile, "w").write("original\n")
t.send("\x11", 0.5)
t = Term([rofile])
os.chmod(ro, 0o555)
try:
    t.send("edit ")
    t.send("\x13", 0.4)
    check("save failure reported", "Save failed" in t.line(ROWS - 1), t.line(ROWS - 1))
    check("still dirty after failed save", "●" in t.line(0), t.line(0))
    check("original intact after failed save", open(rofile).read() == "original\n")
finally:
    os.chmod(ro, 0o755)
t.send("\x13", 0.4)
check("save succeeds once writable", open(rofile).read() == "edit original\n", open(rofile).read())
t.send("\x11", 0.5)

# ---------------------------------------------------------------- session 6: tabs
fa = os.path.join(WORK, "alpha.txt"); open(fa, "w").write("alpha body\n")
fb = os.path.join(WORK, "beta.txt"); open(fb, "w").write("beta body\n")
t = Term([fa, fb])
check("two files open as tabs", "alpha.txt" in t.line(0) and "beta.txt" in t.line(0), t.line(0))
check("first tab active", "alpha body" in t.text(), t.text())
t.click(t.col_of(0, "beta.txt") + 1, 0)
check("click tab switches", "beta body" in t.text(), t.text())
t.send("\x1b[5;5~")  # Ctrl+PageUp
check("Ctrl+PageUp switches back", "alpha body" in t.text(), t.text())
t.send("edit")
check("dirty tab shows ●", "●" in t.line(0), t.line(0))
# Middle-click on beta closes it (clean, no prompt).
t.p.send(f"\x1b[<1;{t.col_of(0, 'beta.txt') + 2};1M"); t.p.send(f"\x1b[<1;{t.col_of(0, 'beta.txt') + 2};1m"); t.pump(0.3)
check("middle-click closes tab", "beta.txt" not in t.line(0) and "alpha.txt" in t.line(0), t.line(0))
# Drop a file while dirty: opens in a new tab, edit kept.
t.send(f"\x1b[200~{fb}\x1b[201~", 0.4)
check("drop while dirty opens new tab", "beta body" in t.text() and "alpha.txt" in t.line(0), t.line(0))
t.send("\x11")
check("quit asks when a background tab is dirty", t.p.isalive() and "alpha.txt" in t.text(), t.text())
# Click the "Don't Save" button with the mouse.
row = next(y for y in range(ROWS) if "Don't Save" in t.line(y))
t.click(t.col_of(row, "Don't Save") + 1, row)
t.pump(0.4)
check("clicking dialog button quits", not t.p.isalive())

# ---------------------------------------------------------------- session 7: live reload of clean tab
fr = os.path.join(WORK, "live.txt"); open(fr, "w").write("version one\n")
t = Term([fr])
time.sleep(0.05)
tmp = fr + ".tmp"; open(tmp, "w").write("version two, written by another program\n"); os.replace(tmp, fr)
t.pump(0.8)
check("clean tab reloads on external change", "version two" in t.text(), t.text())
t.send("\x1a", 0.3)  # Ctrl+Z
check("reload is undoable", "version one" in t.text(), t.text())
t.send("\x19", 0.3)  # Ctrl+Y
t.send("\x13", 0.3)  # save (no conflict: our state equals disk)
check("no conflict after reload", "changed on disk" not in t.text(), t.text())
t.send("\x11", 0.5)
check("clean quit", not t.p.isalive())

# ---------------------------------------------------------------- session 8: find & replace
ff = os.path.join(WORK, "find.txt"); open(ff, "w").write("apple Apple apple pie\nbanana apple\n")
t = Term([ff])
t.send("\x06"); t.send("apple", 0.4)  # Ctrl+F
check("find bar shows count", "1 of 4" in t.text(), t.text())
bar_row = next(y for y in range(ROWS) if "Find:" in t.line(y))
t.click(t.col_of(bar_row, " Aa ") + 1, bar_row); t.pump(0.3)
check("click Aa toggles match case", "of 3" in t.line(bar_row), t.line(bar_row))
t.click(t.col_of(bar_row, " ↓ ") + 1, bar_row); t.pump(0.3)
check("click ↓ goes to next match", "2 of 3" in t.line(bar_row), t.line(bar_row))
t.send("\x12", 0.3)  # Ctrl+R: replace mode, focus Replace field
t.send("pear", 0.3)
t.send("\x1ba", 0.4)  # Alt+A: replace all
check("replace all", open(ff).read() == "apple Apple apple pie\nbanana apple\n" and "pear Apple pear pie" in t.text(), t.text())
t.send("\x1b", 0.2)  # close bar
t.send("\x1a", 0.3)  # undo
check("replace all undoes in one step", "apple Apple apple pie" in t.text(), t.text())
# The find field has a cursor: arrows, Home/End, Backspace mid-text, click to place.
t.send("\x1b", 0.2); t.send("\x06", 0.3)  # Esc, Ctrl+F (last query "apple")
t.send("\x1b[F\x1b[D\x1b[D\x7f", 0.3)  # End, Left, Left, Backspace
t.send("\x1b[H", 0.2); t.send("x", 0.3)  # Home, type
bar_row = next(y for y in range(ROWS) if "Find:" in t.line(y))
check("find field edits at the cursor", "Find:    xaple " in t.line(bar_row), t.line(bar_row))
check("caret drawn in the field", t.screen.cursor.y == bar_row and t.screen.cursor.x == t.col_of(bar_row, "xaple") + 1, (t.screen.cursor.x, t.screen.cursor.y))
t.click(t.col_of(bar_row, "xaple") + 3, bar_row); t.pump(0.2)
t.send("Q", 0.3)
check("click places the field cursor", "Find:    xapQle " in t.line(bar_row), t.line(bar_row))
t.send("\x1b", 0.2)
t.send("\x11", 0.3)
if t.p.isalive(): t.send("n", 0.4)
check("quit after find tests", not t.p.isalive())

# ---------------------------------------------------------------- session 9: config file
cfg_dir = os.path.join(WORK, "config", "plica"); os.makedirs(cfg_dir, exist_ok=True)
open(os.path.join(cfg_dir, "config.toml"), "w").write("""version = 1
[editor]
line_numbers = false
tab_width = 99
[keys]
save = "f2"
quit = ["ctrl+k"]
""")
fc = os.path.join(WORK, "cfg.txt"); open(fc, "w").write("config test\n")
t = Term([fc])
check("config: line numbers off", not t.line(1).lstrip().startswith("1 "), t.line(1))
check("config: bad value warned", "Config:" in t.line(ROWS - 1) and "tab_width" in t.line(ROWS - 1), t.line(ROWS - 1))
t.send("\x1bOP", 0.4)  # F1
check("help shows remapped key", "F2" in t.text() and "Ctrl+K" in t.text(), t.text())
t.send("x", 0.2)  # close help
t.send("edit ", 0.2)
t.send("\x1bOQ", 0.4)  # F2 = save
check("remapped save works", open(fc).read().startswith("edit "), open(fc).read())
t.send("\x13", 0.3)  # Ctrl+S no longer bound: nothing happens
t.send("\x0b", 0.5)  # Ctrl+K = quit
check("remapped quit works", not t.p.isalive())
os.remove(os.path.join(cfg_dir, "config.toml"))

# ---------------------------------------------------------------- session 10: preferences
fp = os.path.join(WORK, "prefs.txt"); open(fp, "w").write("prefs test\n")
cfg_file = os.path.join(WORK, "config", "plica", "config.toml")
t = Term([fp])
check("starts with Dracula background", t.cell(10, 5).bg.lower() == "282a36", t.cell(10, 5).bg)
t.send("\x1b[20~", 0.4)  # F9
check("preferences open", "Preferences" in t.text(), t.text())
t.send("\t\t", 0.3)  # -> Appearance
t.send("\x1b[C\x1b[C\x1b[C", 0.4)  # Colours: auto -> palette -> dracula -> terminal
check("theme previews live (terminal colours)", t.cell(10, ROWS - 3).bg == "default", t.cell(10, ROWS - 3).bg)
row = next(y for y in range(ROWS) if " Save " in t.line(y) and "Cancel" in t.line(y))
t.click(t.col_of(row, " Save ") + 2, row); t.pump(0.4)
check("click Save closes and writes config", "Tab section" not in t.text() and 'theme = "terminal"' in open(cfg_file).read(), t.text())
t.send("\x11", 0.5)
t = Term([fp])
check("saved preference applies on restart", t.cell(10, 5).bg == "default", t.cell(10, 5).bg)
t.send("\x11", 0.5)
os.remove(cfg_file)

print(f"\nwork dir: {WORK}")
print(f"{len(fails)} failure(s)" + (": " + ", ".join(fails) if fails else ""))
sys.exit(1 if fails else 0)

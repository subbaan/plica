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
    def __init__(self, args, env_extra=None, wrap=()):
        env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor",
                   XDG_STATE_HOME=os.path.join(WORK, "state"), XDG_CONFIG_HOME=os.path.join(WORK, "config"), DISPLAY="", WAYLAND_DISPLAY="")
        env.pop("NO_COLOR", None)
        env.pop("PLICA_PALETTE", None)
        env.update(env_extra or {})
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.ByteStream(self.screen)
        self.raw = b""
        cmd = [*wrap, BIN, *args]
        self.p = pexpect.spawn(cmd[0], cmd[1:], env=env, dimensions=(ROWS, COLS))
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
t.send("\x1b[D", 0.4)  # Colours: auto -> (wraps back to) terminal
check("theme previews live (terminal colours)", t.cell(10, ROWS - 3).bg == "default", t.cell(10, ROWS - 3).bg)
row = next(y for y in range(ROWS) if " Save " in t.line(y) and "Cancel" in t.line(y))
t.click(t.col_of(row, " Save ") + 2, row); t.pump(0.4)
check("click Save closes and writes config", "Tab section" not in t.text() and 'theme = "terminal"' in open(cfg_file).read(), t.text())
t.send("\x11", 0.5)
t = Term([fp])
check("saved preference applies on restart", t.cell(10, 5).bg == "default", t.cell(10, 5).bg)
t.send("\x11", 0.5)
os.remove(cfg_file)

# ---------------------------------------------------------------- session 11: highlights
hl_pal = os.path.join(WORK, "theme", "hl-theme.conf")
def write_hl_palette(c4):
    open(hl_pal + ".tmp", "w").write(f"foreground #e0e1e1\nbackground #131617\ncolor3 #de9b4b\ncolor4 {c4}\n")
    os.replace(hl_pal + ".tmp", hl_pal)
write_hl_palette("#4d9bdb")
open(cfg_file, "w").write("""
[[highlights.group]]
name = "Names"
terms = ["Alice", "release day"]
fg = "color4"
underline = true
""")
fh = os.path.join(WORK, "hl.txt"); hl_text = "TODO: ask Alice about release day\nunAlice todo\n"
open(fh, "w").write(hl_text)
t = Term(["--palette", hl_pal, fh])
x = t.col_of(1, "TODO")
check("sample TODO uses palette color3, bold", t.cell(x, 1).fg.lower() == "de9b4b" and t.cell(x, 1).bold, (t.cell(x, 1).fg, t.cell(x, 1).bold))
xa = t.col_of(1, "Alice")
check("user group uses color4, underlined", t.cell(xa, 1).fg.lower() == "4d9bdb" and t.cell(xa, 1).underscore, (t.cell(xa, 1).fg, t.cell(xa, 1).underscore))
xr = t.col_of(1, "release day")
check("phrase highlighted across the space", all(t.cell(xr + k, 1).fg.lower() == "4d9bdb" for k in range(11)), [t.cell(xr + k, 1).fg for k in range(11)])
xu = t.col_of(2, "unAlice")
check("whole word: unAlice not highlighted", t.cell(xu + 2, 2).fg.lower() == "e0e1e1", t.cell(xu + 2, 2).fg)
xt = t.col_of(2, "todo")
check("case-sensitive sample: lowercase todo not highlighted", t.cell(xt, 2).fg.lower() == "e0e1e1", t.cell(xt, 2).fg)
write_hl_palette("#c678dd")
t.pump(0.8)
check("linked highlight follows live palette", t.cell(xa, 1).fg.lower() == "c678dd", t.cell(xa, 1).fg)
xb = t.col_of(1, "about")
t.click(xb + 1, 1); t.send("\x1bh", 0.4)  # Alt+H: highlight word in this document
check("Alt+H marks the word with the accent background", t.cell(xb, 1).bg.lower() == "c678dd" and t.cell(xb + 4, 1).bg.lower() == "c678dd", t.cell(xb, 1).bg)
docs_toml = os.path.join(cfg_dir, "documents.toml")
check("document rule stored outside the file", os.path.exists(docs_toml) and os.path.realpath(fh) in open(docs_toml).read())
t.send("\x11", 0.5)
check("highlighting never touches the file", open(fh).read() == hl_text and not t.p.isalive())
t = Term(["--palette", hl_pal, fh])
check("document highlight persists across restarts", t.cell(xb, 1).bg.lower() == "c678dd", t.cell(xb, 1).bg)
t.click(xb + 1, 1); t.send("\x1b[19~", 0.4)  # F8 (fallback key) removes it
check("F8 toggles the document highlight off", t.cell(xb, 1).bg.lower() != "c678dd" and "about" not in open(docs_toml).read(), t.cell(xb, 1).bg)
t.send("\x11", 0.5)
os.remove(docs_toml)
os.remove(cfg_file)

# ---------------------------------------------------------------- session 11b: built-in themes, overrides
t = Term(["--theme", "solarized-light", fp])
check("--theme solarized-light", t.cell(10, 5).bg.lower() == "fdf6e3" and "Solarized Light" in t.line(ROWS - 1), (t.cell(10, 5).bg, t.line(ROWS - 1)))
t.send("\x11", 0.5)
open(cfg_file, "w").write('[appearance]\ntheme = "nord"\n[appearance.colours]\nbackground = "color1"\n')
t = Term([fp])
check("colour override follows the theme's palette slot (Nord color1)", t.cell(10, 5).bg.lower() == "bf616a", t.cell(10, 5).bg)
check("override with poor contrast is reported", "low contrast" in t.line(ROWS - 1), t.line(ROWS - 1))
t.send("\x11", 0.5)
os.remove(cfg_file)

# ---------------------------------------------------------------- session 12: Highlights in Preferences
fq = os.path.join(WORK, "hlprefs.txt"); open(fq, "w").write("call Zed today\n")
t = Term(["--builtin-theme", fq])
t.send("\x1b[20~", 0.4)  # F9
row = next(y for y in range(ROWS) if "Highlights" in t.line(y) and "│" in t.line(y))
t.click(t.col_of(row, "Highlights") + 1, row); t.pump(0.3)
check("Highlights page lists the samples", "IMPORTANT" in t.text() and "Sample groups" in t.text(), t.text())
row = next(y for y in range(ROWS) if "Fixme" in t.line(y))
t.click(t.col_of(row, "[x] Fixme") + 1, row); t.pump(0.3)
check("clicking a sample's checkbox switches it off", "[ ] Fixme" in t.line(row), t.line(row))
t.send("n", 0.3)  # new group (the list scrolls in this small terminal)
t.send("Zed", 0.2); t.send("\r", 0.4)
prow = next(y for y in range(ROWS) if "Preview" in t.line(y))
xz = t.col_of(prow, "Zed")
check("group editor previews in the group's colour (Dracula accent)", t.cell(xz, prow).fg.lower() == "bd93f9", t.cell(xz, prow).fg)
t.send("\x1b", 0.2); t.send("\x13", 0.5)  # Esc (back to list), Ctrl+S
check("Save writes the group to config.toml", os.path.exists(cfg_file) and 'terms = ["Zed"]' in open(cfg_file).read(), open(cfg_file).read() if os.path.exists(cfg_file) else "missing")
xz = t.col_of(1, "Zed")
check("document shows the new group", t.cell(xz, 1).fg.lower() == "bd93f9", t.cell(xz, 1).fg)
t.send("\x11", 0.5)
os.remove(cfg_file)

# ---------------------------------------------------------------- session 13: Phase 3 gate (shared palette)
gdir = os.path.join(WORK, "gate"); os.makedirs(gdir, exist_ok=True)
def pal_text(bg, fg, c0, sel, c3):
    return f"foreground {fg}\nbackground {bg}\nselection_background {sel}\ncolor0 {c0}\ncolor3 {c3}\ncolor8 #808080\n"
DARK = ("#131617", "#e0e1e1", "#232627", "#189abf", "#de9b4b")
LIGHT = ("#fafafa", "#202020", "#e0e0e0", "#a0c4ff", "#b35900")
def put_pal(path, vals, mode=None):
    tmp = path + ".new"
    open(tmp, "w").write(pal_text(*vals))
    if mode is not None: os.chmod(tmp, mode)
    os.replace(tmp, path)
gpal = os.path.join(gdir, "current-theme.conf")
put_pal(gpal, DARK)
fg_doc = os.path.join(WORK, "gate.txt"); open(fg_doc, "w").write("TODO check every region\n")
t = Term(["--palette", gpal, fg_doc])
t.send("\x06", 0.3); t.send("region", 0.4)  # find bar open, "region" selected as the match
def regions():
    fy = next(y for y in range(ROWS) if "Find:" in t.line(y))
    xs = t.col_of(1, "region")
    return {
        "editor": t.cell(10, 4).bg.lower(), "title bar": t.cell(2, 0).bg.lower(),
        "status bar": t.cell(2, ROWS - 1).bg.lower(), "find bar": t.cell(t.col_of(fy, " of "), fy).bg.lower(),
        "find label (accent)": t.cell(1, fy).bg.lower(),
        "selection": t.cell(xs, 1).bg.lower(), "TODO highlight": t.cell(t.col_of(1, "TODO"), 1).fg.lower(),
    }
want_dark = {"editor": "131617", "title bar": "232627", "status bar": "232627", "find bar": "232627", "find label (accent)": "189abf", "selection": "189abf", "TODO highlight": "de9b4b"}
want_light = {"editor": "fafafa", "title bar": "e0e0e0", "status bar": "e0e0e0", "find bar": "e0e0e0", "find label (accent)": "a0c4ff", "selection": "a0c4ff", "TODO highlight": "b35900"}
check("gate: dark palette in every region", regions() == want_dark, regions())
put_pal(gpal, LIGHT); t.pump(0.8)
check("gate: switching the shared file updates every region (dark -> light)", regions() == want_light, regions())
# A dialog open during a switch is recoloured too.
t.send("\x1b", 0.2); t.send("x", 0.2); t.send("\x11", 0.4)  # close find, edit, Ctrl+Q -> unsaved dialog
dy = next(y for y in range(ROWS) if "Save" in t.line(y) and "Cancel" in t.line(y))
before = t.cell(t.col_of(dy, "Cancel") - 2, dy).bg.lower()
put_pal(gpal, DARK); t.pump(0.8)
after = t.cell(t.col_of(dy, "Cancel") - 2, dy).bg.lower()
check("gate: open dialog recoloured live", before == "e0e0e0" and after == "232627", (before, after))
t.send("\x1b", 0.3)  # cancel the dialog
# Corrupt / unreadable / removed files keep the last good colours.
for name, action in [
    ("unreadable file", lambda: put_pal(gpal, LIGHT, mode=0)),
    ("truncated file", lambda: (open(gpal + ".new", "w").write("foreground #202020\nbackgr"), os.replace(gpal + ".new", gpal))),
    ("invalid hex", lambda: (open(gpal + ".new", "w").write(pal_text("#zzzzzz", "#202020", "#e0e0e0", "#a0c4ff", "#b35900")), os.replace(gpal + ".new", gpal))),
    ("deleted file", lambda: os.remove(gpal)),
]:
    action(); t.pump(0.8)
    check(f"gate: {name} keeps last good colours", t.cell(10, 4).bg.lower() == "131617" and t.p.isalive(), t.cell(10, 4).bg)
check("gate: problem shown in the status bar", "⚠" in t.line(ROWS - 1), t.line(ROWS - 1))
put_pal(gpal, LIGHT); t.pump(0.8)
check("gate: restored file applies", t.cell(10, 4).bg.lower() == "fafafa", t.cell(10, 4).bg)
# Rapid switching settles on the last palette.
for i in range(15):
    put_pal(gpal, LIGHT if i % 2 else DARK)
    time.sleep(0.02)
put_pal(gpal, ("#102030",) + DARK[1:]); t.pump(1.0)
check("gate: rapid switching settles on the final palette", t.cell(10, 4).bg.lower() == "102030", t.cell(10, 4).bg)
t.send("\x11", 0.4)
if t.p.isalive(): t.send("n", 0.4)
check("gate: session ends cleanly", not t.p.isalive())

# Symlinked palette path, re-pointed by a theme switcher, then edited in place.
d1 = os.path.join(gdir, "themes1"); d2 = os.path.join(gdir, "themes2"); os.makedirs(d1); os.makedirs(d2)
put_pal(os.path.join(d1, "one.conf"), DARK); put_pal(os.path.join(d2, "two.conf"), LIGHT)
link = os.path.join(gdir, "linked.conf"); os.symlink(os.path.join(d1, "one.conf"), link)
t = Term(["--palette", link, fg_doc])
check("gate: symlinked palette read", t.cell(10, 4).bg.lower() == "131617", t.cell(10, 4).bg)
os.symlink(os.path.join(d2, "two.conf"), link + ".new"); os.replace(link + ".new", link); t.pump(0.8)
check("gate: re-pointed symlink applies", t.cell(10, 4).bg.lower() == "fafafa", t.cell(10, 4).bg)
put_pal(os.path.join(d2, "two.conf"), ("#304050",) + LIGHT[1:]); t.pump(0.8)
check("gate: edits to the new link target are followed", t.cell(10, 4).bg.lower() == "304050", t.cell(10, 4).bg)
t.send("\x11", 0.4)
if t.p.isalive(): t.send("n", 0.4)

# ---------------------------------------------------------------- session 14: text encodings
edir = os.path.join(WORK, "enc"); os.makedirs(edir)
lat = os.path.join(edir, "latin1.txt")
lat_bytes = "Café crème brûlée, déjà vu et naïveté.\n".encode("cp1252")
open(lat, "wb").write(lat_bytes)
t = Term([lat])
check("enc: windows-1252 file decoded", "Café crème brûlée" in t.text(), t.text())
check("enc: guess announced", "windows-1252" in t.line(ROWS - 1), t.line(ROWS - 1))
t.send("\x1b[1;5F")  # Ctrl+End
t.send("é")
t.send("\x13", 0.4)
check("enc: saved back in windows-1252", open(lat, "rb").read() == lat_bytes + b"\xe9", open(lat, "rb").read())
# A char windows-1252 lacks: dialog, nothing written; then save as UTF-8.
t.send("✓")
t.send("\x13", 0.4)
check("enc: unmappable char stops the save", "Can't save in windows-1252" in t.text() and open(lat, "rb").read() == lat_bytes + b"\xe9", t.text())
t.send("u", 0.4)
check("enc: converted to UTF-8 on request", open(lat, "rb").read() == (lat_bytes.decode("cp1252") + "é✓").encode(), open(lat, "rb").read())
# Encoding menu from the status bar (after the message has gone).
t.pump(5.5)
y = ROWS - 1
x = t.col_of(y, "UTF-8")
t.click(x + 1, y)
check("enc: status bar opens the encoding menu", "Reopen with" in t.text() and "Save with" in t.text(), t.text())
t.send("s", 0.3)
check("enc: picker lists encodings", "Save with encoding" in t.text() and "Filter:" in t.text(), t.text())
t.send("16 le", 0.3)
t.send("\r", 0.5)
data = open(lat, "rb").read()
check("enc: saved as UTF-16 LE with BOM", data.startswith(b"\xff\xfe") and data[2:].decode("utf-16-le").startswith("Café"), data[:12])
t.send("\x11", 0.4)
check("enc: clean quit", not t.p.isalive())

# Stray bytes in UTF-8 are shown as \xNN and survive an edit.
stray = os.path.join(edir, "stray.txt")
open(stray, "wb").write("naïve – ".encode() + b"\xe9t\xe9\n")
t = Term([stray])
check("enc: stray bytes drawn as \\xNN", "naïve – \\xE9t\\xE9" in t.text(), t.text())
t.send("\x1b[1;5F"); t.send("ok")
t.send("\x13", 0.4)
check("enc: stray bytes written back unchanged", open(stray, "rb").read() == "naïve – ".encode() + b"\xe9t\xe9\nok", open(stray, "rb").read())
t.send("\x11", 0.4)

# Shift_JIS guessed; --encoding forces; F7 reopen via the picker.
sj = os.path.join(edir, "sjis.txt")
open(sj, "wb").write("日本語のテキストファイルです。文字コードの判定テスト。\n".encode("shift_jis"))
t = Term([sj])
check("enc: Shift_JIS decoded", "日本語のテキスト" in t.text(), t.text())
t.send("\x11", 0.4)
ru = os.path.join(edir, "ru.txt")
open(ru, "wb").write("Привет, мир\n".encode("koi8_r"))
t = Term(["--encoding", "koi8-r", ru])
check("enc: --encoding forces the decoder", "Привет, мир" in t.text(), t.text())
t.send("\x1b[18~", 0.3)  # F7
t.send("r", 0.3)
t.send("1251", 0.3)
t.send("\r", 0.5)
check("enc: reopen with another encoding", "Привет" not in t.text() and "windows-1251" in t.line(ROWS - 1), t.text())
t.send("\x1a", 0.3)  # Ctrl+Z back to the KOI8-R text
check("enc: reopen is undoable", "Привет, мир" in t.text(), t.text())
t.send("\x11", 0.4)
if t.p.isalive(): t.send("n", 0.4)

# Binary files are refused with a clear message.
binf = os.path.join(edir, "prog.bin"); open(binf, "wb").write(b"\x7fELF\x02\x01\x00\x00\xff\xfe\x80\x81")
out = pexpect.run(BIN, args=[binf], env=dict(os.environ, XDG_STATE_HOME=os.path.join(WORK, "state"), XDG_CONFIG_HOME=os.path.join(WORK, "config")), timeout=5).decode()
check("enc: binary file refused", "binary" in out, out)

# ---------------------------------------------------------------- session 15: Markdown tables
tdoc = os.path.join(WORK, "gear.md")
TSRC = "# Gear\n\n| Device | Type |\n| ------ | ---- |\n| MPC    | Samp |\n\nDone.\n"
open(tdoc, "w").write(TSRC)
t = Term([tdoc])
t.send("\x1b[B" * 4)  # caret into the MPC row
check("table: status shows table context", "Table" in t.text(), t.text())
t.send("\x1b[F")      # End (after the last pipe = last cell)
t.send("\x1b[Z")      # Shift+Tab: previous cell = Device column, selected
t.send(" One Plus")   # typing replaces the selection...
t.send("\x13", 0.4)
saved = open(tdoc).read()
check("table: typing realigns its column", "| One Plus | Type |\n| -------- | ---- |\n| One Plus" not in saved and "| Device   | Type |\n| -------- | ---- |\n| One Plus | Samp |" in saved, saved)
t.send("\x1a\x1a\x1a", 0.4)
t.send("\x13", 0.4)
check("table: undo restores the structure", open(tdoc).read() == TSRC, open(tdoc).read())
# Tab from the last cell adds a row; Esc gives a plain Tab.
y = 4 + 1  # screen row of the MPC line (title bar at row 0)
t.click(t.col_of(y, "Samp") + 1, y)
t.send("\t")
t.send("Volca")
t.send("\x13", 0.4)
check("table: Tab in the last cell adds a row", "| MPC    | Samp |\n| Volca  |      |\n" in open(tdoc).read(), open(tdoc).read())
t.send("\x1b", 0.2)
check("table: Esc turns table keys off", "keys off" in t.text(), t.text())
t.send("\t")
t.send("\x13", 0.4)
check("table: plain Tab after Esc", "Volca\t" in open(tdoc).read(), open(tdoc).read())
t.send("\x1a", 0.3)
t.send("\x13", 0.4)  # save, so the rewrite below reloads cleanly
t.send("\x1b", 0.2)
# Table menu → Format on a sloppy table.
open(tdoc, "w").write("|a|bb|\n|-|-|\n|ccc|d|\n"); t.pump(0.8)
t.send("\x1b[1;5H")  # Ctrl+Home: into the table
t.send("\x1bt", 0.3)
check("table: menu opens", "Format table" in t.text(), t.text())
t.send("\r", 0.3)
t.send("\x13", 0.4)
check("table: format command", open(tdoc).read() == "|a  |bb |\n|---|---|\n|ccc|d  |\n", open(tdoc).read())
# Right-click menu: insert a column right of "ccc"; Alt+Down moves a row.
open(tdoc, "w").write("| k | v |\n|---|---|\n| 1 | a |\n| 2 | b |\n"); t.pump(0.8)
y = 3  # screen row of "| 1 | a |" (title bar is row 0)
t.click(t.col_of(y, "| 1") + 2, y, btn=2)
check("table: right-click opens the table menu", "Insert column right" in t.text(), t.text())
t.send("column right", 0.3)
t.send("\r", 0.3)
t.send("x", 0.2)
t.send("\x1b[1;3B", 0.3)  # Alt+Down
t.send("\x13", 0.4)
check("table: menu column insert and Alt+Down row move", open(tdoc).read() == "| k |   | v |\n|---|---|---|\n| 2 |   | b |\n| 1 | x | a |\n", open(tdoc).read())
# Drag across cells selects them; Ctrl+C copies tab-separated text; a
# tab-separated paste into the table fills cells.
open(tdoc, "w").write("| k | v |\n|---|---|\n| 1 | a |\n| 2 | b |\n\nafter\n"); t.pump(0.8)
t.drag(t.col_of(3, "| 1") + 2, 3, t.col_of(4, "b"), 4)
sel_bg = t.cell(t.col_of(4, "b"), 4).bg
check("table: drag across cells selects cells", sel_bg == t.cell(t.col_of(3, "| 1") + 2, 3).bg and sel_bg != t.cell(t.col_of(1, "k"), 1).bg, sel_bg)
t.send("\x03", 0.3)  # Ctrl+C
t.click(t.col_of(6, "after") + 5, 6)
t.send("\r\x16", 0.4)  # Enter, Ctrl+V outside the table: plain text
t.send("\x13", 0.4)
check("table: cells copy as tab-separated text", "after\n1\ta\n2\tb\n" in open(tdoc).read(), open(tdoc).read())
t.click(t.col_of(3, "a |"), 3)
t.send("\x1b[200~X\tY\nZ\tW\n\x1b[201~", 0.4)  # bracketed paste
t.send("\x13", 0.4)
check("table: tab-separated paste fills and extends the table", open(tdoc).read().startswith("| k | v |   |\n|---|---|---|\n| 1 | X | Y |\n| 2 | Z | W |\n"), open(tdoc).read())
# Save, quit and reopen: the pasted table is read back as a table.
t.send("\x11", 0.5)
if t.p.isalive(): t.send("n", 0.4)
t = Term([tdoc])
t.send("\x1b[B\x1b[B", 0.3)
check("table: reopened file is a table with the pasted values", "Table" in t.line(ROWS - 1) or "Table:" in t.text(), t.text())
check("table: pasted values survive reopening", "| 1 | X | Y |" in t.text() and "| 2 | Z | W |" in t.text(), t.text())
# A table wider than the window doesn't wrap; the caret's table scrolls.
wide = "| " + " | ".join(f"column{i}" for i in range(8)) + " |\n" + "|---" * 8 + "|\n" + "| " + " | ".join(f"v{i}" for i in range(8)) + " |\n"
open(tdoc, "w").write("intro\n" + wide); t.pump(0.8)
check("table: wide rows stay on one line", t.line(3).lstrip().startswith("3") and "›" in t.line(2) and "column0" in t.line(2), t.text())
t.send("\x1b[1;5H\x1b[B", 0.2)
t.send("\t" * 7, 0.4)
check("table: caret's table scrolls sideways", "‹" in t.line(2) and "column7" in t.line(2) and "intro" in t.line(1), t.text())
t.send("\x11", 0.4)
if t.p.isalive(): t.send("n", 0.4)
check("table: clean quit", not t.p.isalive())

# ---------------------------------------------------------------- session 16: bordered tables
bdoc = os.path.join(WORK, "box.txt")
open(bdoc, "w").write("Gear:\n")
t = Term([bdoc])
t.send("\x1bt", 0.3)
check("bordered: Alt+T outside a table offers new tables", "Insert table" in t.text() and "New ASCII table" in t.text(), t.text())
t.send("ascii\r", 0.3)
t.send("Device\tType\tState\tMPC\tSampler", 0.5)
t.send("\x13", 0.4)
check("bordered: new ASCII table typed into", open(bdoc).read() == "Gear:\n\n+--------+---------+-------+\n| Device | Type    | State |\n+--------+---------+-------+\n| MPC    | Sampler |       |\n+--------+---------+-------+\n", open(bdoc).read())
t.send("\x1bt", 0.3)
t.send("convert box", 0.3)
t.send("\r", 0.3)
t.send("\x13", 0.4)
check("bordered: converted to a Unicode box table", "│ MPC    │ Sampler │       │\n└────────┴─────────┴───────┘" in open(bdoc).read(), open(bdoc).read())
t.send("\x11", 0.4)
if t.p.isalive(): t.send("n", 0.4)

# ---------------------------------------------------------------- session 17: terminal window closed
# Closing the window hangs up the tty. Plica must exit (not spin on the dead
# terminal) and snapshot unsaved work, whether or not SIGHUP reaches it: in
# its own session (setsid) the kernel's SIGHUP goes elsewhere.
def proc_gone(pid):
    try:
        return open(f"/proc/{pid}/stat").read().split()[2] == "Z"
    except FileNotFoundError:
        return True
hdoc = os.path.join(WORK, "hangup.txt")
for label, wrap in (("SIGHUP", ()), ("no SIGHUP", ("setsid", "-w"))):
    for f in snaps():
        os.remove(os.path.join(rec_dir, f))
    t = Term([hdoc], wrap=wrap)
    t.send(f"left open {label}", 0.5)
    pid = t.p.pid if not wrap else int(open(f"/proc/{t.p.pid}/task/{t.p.pid}/children").read().split()[0])
    t.p.ptyproc.fileobj.close()  # the terminal window goes away
    t.p.ptyproc.closed = t.p.closed = True
    end = time.time() + 2
    while time.time() < end and not proc_gone(pid):
        time.sleep(0.05)
    gone = proc_gone(pid)
    if not gone:
        os.kill(pid, signal.SIGKILL)
    check(f"window closed ({label}): plica exits", gone)
    check(f"window closed ({label}): unsaved work snapshotted", any(f"left open {label}" in x for x in snap_texts()), str(snap_texts()))
    if not wrap:
        os.waitpid(pid, 0)
check("window closed: the file itself is untouched", not os.path.exists(hdoc))

# ---------------------------------------------------------------- session 18: overwrite mode
mdoc = os.path.join(WORK, "map.txt")
open(mdoc, "w").write("#....#\n#....#\n")
t = Term([mdoc])
t.send("d", 0.4)  # Discard the snapshots session 17 left
check("overwrite: starts in insert mode", "OVR" not in t.line(ROWS - 1), t.line(ROWS - 1))
t.send("\x1b[B\x1b[C")  # Down, Right
mark = len(t.raw)
t.send("\x1b[2~")  # Insert
check("overwrite: Insert shows OVR", " OVR " in t.line(ROWS - 1), t.line(ROWS - 1))
check("overwrite: block cursor", b"\x1b[2 q" in t.raw[mark:], repr(t.raw[mark:][-200:]))
t.send("@~~~~~")
check("overwrite: typing replaces, line end extends", "#@~~~~~" in t.line(2) and "#....#" in t.line(1), t.line(2))
t.send("\x13")
check("overwrite: saved bytes", open(mdoc, "rb").read() == b"#....#\n#@~~~~~\n", repr(open(mdoc, "rb").read()))
mark = len(t.raw)
t.click(t.col_of(ROWS - 1, "OVR"), ROWS - 1)
check("overwrite: clicking OVR switches back", "Insert: typing inserts" in t.line(ROWS - 1) and "OVR" not in t.line(ROWS - 1), t.line(ROWS - 1))
check("overwrite: bar cursor again", b"\x1b[6 q" in t.raw[mark:], repr(t.raw[mark:][-200:]))
t.send("x\x1a")
t.send("\x11", 0.6)
check("overwrite: quits", not t.p.isalive())

print(f"\nwork dir: {WORK}")
print(f"{len(fails)} failure(s)" + (": " + ", ".join(fails) if fails else ""))
sys.exit(1 if fails else 0)

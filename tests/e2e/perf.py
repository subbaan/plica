"""Large-file responsiveness check (dev only): make perf

Builds a ~35 MB, 400k-line file with mixed Unicode, then checks startup time,
jumps, typing and idle CPU against generous thresholds. Uses a temp dir.
"""
import os, sys, tempfile, time
import pexpect, pyte

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(REPO, "plica")
WORK = tempfile.mkdtemp(prefix="plica-perf-")
big = os.path.join(WORK, "big.txt")
with open(big, "w") as f:
    for i in range(400_000):
        f.write(f"{i:07d} The quick brown fox jumps over the lazy dog, café 中文 emoji 👍 tab\there.\n")
env = dict(os.environ, TERM="xterm-256color", DISPLAY="", WAYLAND_DISPLAY="",
           XDG_STATE_HOME=os.path.join(WORK, "state"), XDG_CONFIG_HOME=os.path.join(WORK, "config"))
scr = pyte.Screen(100, 30); st = pyte.ByteStream(scr)
fails = []

def check(name, ok, detail):
    print(("PASS " if ok else "FAIL ") + f"{name}: {detail}")
    if not ok:
        fails.append(name)

t0 = time.time()
p = pexpect.spawn(BIN, [big], env=env, dimensions=(30, 100))
scr.write_process_input = lambda d: p.send(d)

def pump_until(cond, limit=10.0):
    end = time.time() + limit
    while time.time() < end:
        try:
            st.feed(p.read_nonblocking(1 << 20, timeout=0.01))
        except pexpect.TIMEOUT:
            pass
        except pexpect.EOF:
            return False
        if cond():
            return True
    return False

screen = lambda: "\n".join(scr.display)
pump_until(lambda: "0000000 The quick" in screen())
check("startup to first render", time.time() - t0 < 1.5, f"{time.time() - t0:.2f}s (limit 1.5s)")

def timed(name, keys, cond, limit_ms):
    t = time.time(); p.send(keys); ok = pump_until(cond, 5)
    ms = (time.time() - t) * 1000
    check(name, ok and ms < limit_ms, f"{ms:.0f}ms (limit {limit_ms}ms)")

timed("Ctrl+End", "\x1b[1;5F", lambda: "0399999" in screen(), 500)
timed("type at end", "Z", lambda: "Z" in scr.display[scr.cursor.y], 300)
timed("Ctrl+Home", "\x1b[1;5H", lambda: "0000000" in screen(), 500)
timed("find a late line", "\x06" + "0399998", lambda: "0399998" in screen() and "of 1" in screen(), 3000)
p.send("\x1b")
pump_until(lambda: False, 3.0)  # let recovery snapshot (2 s idle) and redraws settle

def cpu():
    v = open(f"/proc/{p.pid}/stat").read().rsplit(")", 1)[1].split()
    return int(v[11]) + int(v[12])  # utime + stime
c0 = cpu(); time.sleep(3); c1 = cpu()
check("idle CPU over 3s", c1 - c0 <= 5, f"{c1 - c0} ticks (limit 5)")
rss = int([l for l in open(f"/proc/{p.pid}/status") if l.startswith("VmRSS")][0].split()[1]) // 1024
check("memory", rss < 400, f"{rss} MB RSS (limit 400)")
p.send("\x11"); pump_until(lambda: "Unsaved" in screen() or not p.isalive(), 2); p.send("n")
pump_until(lambda: not p.isalive(), 3)
os.remove(big)

# ---- a large source file: syntax colouring stays incremental ----
code = os.path.join(WORK, "big.py")
with open(code, "w") as f:
    for i in range(30_000):
        f.write(f'def f{i:05d}(x, y=3):\n    """Doc {i}."""\n    return x + y * {i}  # note\n\n')
print(f"\n{os.path.getsize(code) / 1e6:.1f} MB of Python")
scr = pyte.Screen(100, 30); st = pyte.ByteStream(scr)
t0 = time.time()
p = pexpect.spawn(BIN, [code], env=dict(env, COLORTERM="truecolor"), dimensions=(30, 100))
scr.write_process_input = lambda d: p.send(d)
pump_until(lambda: "def f00000" in screen())
check("code: startup to first render", time.time() - t0 < 1.5, f"{time.time() - t0:.2f}s (limit 1.5s)")
pump_until(lambda: False, 0.5)
kw = scr.buffer[1][scr.display[1].index("def")].fg
check("code: keyword coloured", kw not in ("default", scr.buffer[1][scr.display[1].index("f00000")].fg), kw)
timed("code: Ctrl+End", "\x1b[1;5F", lambda: "f29999" in screen(), 500)
timed("code: type at end", "Z", lambda: "Z" in scr.display[scr.cursor.y], 300)
timed("code: Ctrl+Home", "\x1b[1;5H", lambda: "f00000" in screen(), 500)
# Opening a string at the top changes the colour of everything below it.
timed("code: type a quote at the top", '"""', lambda: '"""def' in screen(), 300)
pump_until(lambda: False, 3.0)
c0 = cpu(); time.sleep(3); c1 = cpu()
check("code: idle CPU over 3s", c1 - c0 <= 5, f"{c1 - c0} ticks (limit 5)")
rss = int([l for l in open(f"/proc/{p.pid}/status") if l.startswith("VmRSS")][0].split()[1]) // 1024
check("code: memory", rss < 400, f"{rss} MB RSS (limit 400)")
p.send("\x11"); pump_until(lambda: "Unsaved" in screen() or not p.isalive(), 2); p.send("n")
pump_until(lambda: not p.isalive(), 3)
os.remove(code)
print(f"\nwork dir: {WORK}")
print(f"{len(fails)} failure(s)")
sys.exit(1 if fails else 0)

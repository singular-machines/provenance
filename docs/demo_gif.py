# Draws docs/demo.gif: typing `provenance prs`, the table, then `provenance go`
# reopening a session. Uses the renderer's colours and layout, on fictional data.
# Run: python3 docs/demo_gif.py docs/demo.gif   (needs Pillow)
import sys
from PIL import Image, ImageDraw, ImageFont

FG, DIM, GREEN, YELLOW, BG, BAR = "#e6edf3", "#7d8590", "#3fb950", "#d29922", "#0d1117", "#161b22"
B = "bold"


# Each line: list of (column, text, colour, weight). Columns match src/render.rs
# for these rows: title at 10, app 55, harness 66, ID 73, ~ 82, time 84.
def row(glyph, gcol, num, numw, title, tcol, tw, place, harness, hid, tilde, when, wcol, live, ww=""):
    out = [(2, glyph, gcol, B if gcol == YELLOW else ""), (4, num, FG, numw), (10, title, tcol, tw)]
    if place is None:
        return out + [(55, "— no session found", DIM, "")]
    pc = FG if live else DIM
    return out + [(55, place, pc, ""), (66, harness, pc, ""), (73, hid, DIM, ""),
                  (82, tilde, DIM, ""), (84, when.rjust(3), wcol, ww)]


def heading(repo, counts):
    return [(0, repo, FG, B), (87 - len(counts), counts, DIM, "")]


def prompt(cmd):
    return [(0, "$", GREEN, B), (2, cmd, FG, B)]


TABLE = [
    heading("acme/storefront", "3 open · 1 running · 1 waiting"),
    row("●", GREEN, "#927", B, "Add retry when checkout times out", FG, "", "T3 Code", "Claude", "4f2a91c0", "~", "55m", GREEN, True),
    row("◐", YELLOW, "#931", B, "Show stock levels on the product page", FG, B, "CLI", "Codex", "01a1c0de", "", "1h", YELLOW, True, B),
    row("", DIM, "#896", "", "fix: round totals before applying discounts", DIM, "", "CLI", "Claude", "d361dde1", "", "10h", DIM, False),
    [],
    heading("acme/api", "2 open"),
    row("", DIM, "#950", "", "fix: remove unused exports from the client", DIM, "", "web", "Claude", "01Hq3xKe", "", "7h", DIM, False),
    row("", DIM, "#68", "", "Ask before rebuilding the local cache", DIM, "", "Conductor", "Claude", "7be310aa", "~", "3d", DIM, False),
    [],
    heading("example/web", "1 open"),
    row("", DIM, "#41", "", "Bump deps", DIM, "", None, None, None, None, None, None, False),
    [],
    [(0, "6 open PRs in 3 repos · 1 with no session found  ·  ", DIM, ""), (52, "●", GREEN, ""), (54, "running  ", DIM, ""),
     (63, "◐", YELLOW, B), (65, "waiting for you  ~ matched by branch, not stamped", DIM, "")],
]
GO = "provenance go acme/storefront#896"
GO_OUT = [
    [(0, "acme/storefront#896 → Claude session d361dde1-7a0c-4e1b-9f42-5c8d2e6b1a37 (CLI, ended)", FG, "")],
    [(0, "Resuming in ~/code/storefront: claude --resume d361dde1-7a0c-4e1b-9f42-5c8d2e6b1a37", FG, "")],
]

S = 2  # draw at 2x for sharp text when the README scales it down
FS, LH, PADX, TOP, COLS = 14 * S, 22 * S, 24 * S, 52 * S, 115
regular = ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", FS, index=0)
bold = ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", FS, index=1)
CW = regular.getlength("M")
ROWS = 2 + len(TABLE) + 1 + 1 + len(GO_OUT)
W, H = int(PADX * 2 + COLS * CW), int(TOP + LH * ROWS + 10 * S)


def frame(lines, cursor=None):
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    d.rectangle([0, 0, W, 34 * S], fill=BAR)
    for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"]):
        x = (20 + 20 * i) * S
        d.ellipse([x - 6 * S, 11 * S, x + 6 * S, 23 * S], fill=c)
    for i, line in enumerate(lines):
        y = TOP + i * LH - FS
        for col, text, colour, weight in line:
            if text:
                d.text((PADX + col * CW, y), text, fill=colour, font=bold if weight else regular)
    if cursor is not None:
        row, col = cursor
        y = TOP + row * LH - FS
        d.rectangle([PADX + col * CW, y + 2 * S, PADX + (col + 1) * CW - 2 * S, y + FS + 4 * S], fill=FG)
    return img


frames, durations = [], []


def add(lines, ms, cursor=None):
    frames.append(frame(lines, cursor))
    durations.append(ms)


def typed(before, row, cmd):
    """Frames typing `cmd` at `row` under `before`."""
    for blink in range(2):
        add(before + [prompt("")], 400, (row, 2))
        add(before + [prompt("")], 300)
    for n in range(1, len(cmd) + 1):
        add(before + [prompt(cmd[:n])], 55, (row, 2 + n))
    add(before + [prompt(cmd)], 500, (row, 2 + len(cmd)))


typed([], 0, "provenance prs")
screen = [prompt("provenance prs"), []] + TABLE + [[]]
add(screen, 3800)
typed(screen, len(screen), GO)
screen += [prompt(GO)]
add(screen + GO_OUT[:1], 700)
add(screen + GO_OUT, 3500)

# One palette from the fullest frame keeps the colours steady across frames.
# Swatches of the window buttons' colours, too small otherwise to win a slot.
sample = frames[-1].copy()
for i, c in enumerate(["#ff5f57", "#febc2e", "#28c840"]):
    sample.paste(c, (i * 40 * S, H - 10 * S, (i + 1) * 40 * S, H))
palette = sample.quantize(colors=64, method=Image.Quantize.MEDIANCUT)
frames = [f.quantize(palette=palette, dither=Image.Dither.NONE) for f in frames]
frames[0].save(sys.argv[1], save_all=True, append_images=frames[1:], duration=durations, loop=0, optimize=True)

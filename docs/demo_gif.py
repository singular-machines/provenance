# Draws the README's animations from fictional data, with the renderer's
# colours and layout:
#   docs/demo.gif     typing `provenance prs`, the table, then `go` resuming a session
#   docs/demo-go.gif  `go` switching to the terminal tab where a session waits
# Run: python3 docs/demo_gif.py docs   (needs Pillow)
import os
import sys
from PIL import Image, ImageDraw, ImageFont

FG, DIM, GREEN, YELLOW, BG, BAR = "#e6edf3", "#7d8590", "#3fb950", "#d29922", "#0d1117", "#161b22"
CYAN, TAB = "#79c0ff", "#21262d"
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
    row("○", DIM, "#896", "", "fix: round totals before applying discounts", DIM, "", "CLI", "Claude", "d361dde1", "", "10h", DIM, False),
    [],
    heading("acme/api", "2 open"),
    row("", DIM, "#950", "", "fix: remove unused exports from the client", DIM, "", "web", "Claude", "01Hq3xKe", "", "7h", DIM, False),
    row("", DIM, "#68", "", "Ask before rebuilding the local cache", DIM, "", "Conductor", "Claude", "7be310aa", "~", "3d", DIM, False),
    [],
    heading("example/web", "1 open"),
    row("", DIM, "#41", "", "Bump deps", DIM, "", None, None, None, None, None, None, False),
    [],
    [(0, "6 open PRs in 3 repos · 1 with no session found  ·  ", DIM, ""), (52, "●", GREEN, ""), (54, "running  ", DIM, ""),
     (63, "◐", YELLOW, B), (65, "waiting for you  ", DIM, ""), (82, "○", DIM, ""),
     (84, "ended  ~ matched by branch, not stamped", DIM, "")],
]
GO = "provenance go acme/storefront#896"
GO_OUT = [
    [(0, "acme/storefront#896 → Claude session d361dde1-7a0c-4e1b-9f42-5c8d2e6b1a37 (CLI, ended)", FG, "")],
    [(0, "Resuming in ~/code/storefront: claude --resume d361dde1-7a0c-4e1b-9f42-5c8d2e6b1a37", FG, "")],
]

# What the second animation shows: `go` from one tab switches to the tab where
# the session waits. The Codex screen is a sketch, not Codex's real layout.
SWITCH = "provenance go acme/storefront#931"
SWITCH_OUT = [[(0, "acme/storefront#931 → Codex session 01a1c0de-3b5e-7f21-9c4d-8e2a6b0f5d13 (CLI, waiting for you)", FG, "")]]
CODEX = [
    [(0, "›", CYAN, B), (2, "Show stock levels on the product page", FG, "")],
    [],
    [(0, "•", DIM, ""), (2, "Added a stock badge to the product page, with tests.", FG, "")],
    [(2, "Opened acme/storefront#931. Waiting for your review.", FG, "")],
    [],
]
TABS = ["~/code/api", "codex · ~/code/storefront"]

S = 2  # draw at 2x for sharp text when the README scales it down
FS, LH, PADX, TOP, COLS = 14 * S, 22 * S, 24 * S, 52 * S, 124
regular = ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", FS, index=0)
bold = ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", FS, index=1)
CW = regular.getlength("M")
BUTTONS = ["#ff5f57", "#febc2e", "#28c840"]
# Colours too small on screen to win a palette slot by themselves.
SWATCHES = BUTTONS + [CYAN]


class Gif:
    def __init__(self, rows, cols=COLS, tabs=None):
        self.w, self.h = int(PADX * 2 + cols * CW), int(TOP + LH * rows + 10 * S)
        self.tabs = tabs
        self.frames, self.durations = [], []

    def frame(self, lines, cursor=None, active=0):
        img = Image.new("RGB", (self.w, self.h), BG)
        d = ImageDraw.Draw(img)
        d.rectangle([0, 0, self.w, 34 * S], fill=BAR)
        for i, c in enumerate(BUTTONS):
            x = (20 + 20 * i) * S
            d.ellipse([x - 6 * S, 11 * S, x + 6 * S, 23 * S], fill=c)
        for i, name in enumerate(self.tabs or []):
            x0 = 90 * S + i * 300 * S
            if i == active:
                d.rounded_rectangle([x0, 5 * S, x0 + 290 * S, 29 * S], radius=5 * S, fill=TAB)
            d.text((x0 + 12 * S, 17 * S - FS // 2 - 2 * S), name, fill=FG if i == active else DIM, font=regular)
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

    def add(self, lines, ms, cursor=None, active=0):
        self.frames.append(self.frame(lines, cursor, active))
        self.durations.append(ms)

    def typed(self, before, cmd):
        """Frames typing `cmd` on the line after `before`."""
        row = len(before)
        for _ in range(2):
            self.add(before + [prompt("")], 400, (row, 2))
            self.add(before + [prompt("")], 300)
        for n in range(1, len(cmd) + 1):
            self.add(before + [prompt(cmd[:n])], 55, (row, 2 + n))
        self.add(before + [prompt(cmd)], 500, (row, 2 + len(cmd)))

    def save(self, path):
        # One palette from the fullest frame keeps the colours steady across
        # frames, with swatches of the colours that are only a few pixels.
        sample = self.frames[-1].copy()
        for i, c in enumerate(SWATCHES):
            sample.paste(c, (i * 40 * S, self.h - 10 * S, (i + 1) * 40 * S, self.h))
        palette = sample.quantize(colors=64, method=Image.Quantize.MEDIANCUT)
        frames = [f.quantize(palette=palette, dither=Image.Dither.NONE) for f in self.frames]
        frames[0].save(path, save_all=True, append_images=frames[1:], duration=self.durations, loop=0, optimize=True)


out = sys.argv[1]

g = Gif(2 + len(TABLE) + 1 + 1 + len(GO_OUT))
g.typed([], "provenance prs")
screen = [prompt("provenance prs"), []] + TABLE + [[]]
g.add(screen, 3800)
g.typed(screen, GO)
screen += [prompt(GO)]
g.add(screen + GO_OUT[:1], 700)
g.add(screen + GO_OUT, 3500)
g.save(os.path.join(out, "demo.gif"))

g = Gif(7, 96, TABS)
g.typed([], SWITCH)
g.add([prompt(SWITCH)] + SWITCH_OUT, 900)
g.add(CODEX + [[(0, "›", CYAN, B)]], 3500, (len(CODEX), 2), active=1)
g.add(CODEX + [[(0, "›", CYAN, B)]], 500, None, active=1)
g.add(CODEX + [[(0, "›", CYAN, B)]], 1500, (len(CODEX), 2), active=1)
g.save(os.path.join(out, "demo-go.gif"))

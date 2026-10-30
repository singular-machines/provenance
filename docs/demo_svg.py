# Draws docs/demo.svg: the `provenance prs` table with the renderer's colours, on fictional data.
# Run: python3 docs/demo_svg.py docs/demo.svg
import html, sys
FG, DIM, GREEN, YELLOW, RED, BG, BAR = "#e6edf3", "#7d8590", "#3fb950", "#d29922", "#f85149", "#0d1117", "#161b22"
B = "bold"
# Each line: list of (column, text, colour, weight)
def row(glyph, gcol, num, numw, title, tcol, tw, place, harness, hid, tilde, when, wcol, live, ww=""):
    out = []
    out.append((2, glyph, gcol, B if gcol == YELLOW else ""))
    out.append((4, num, FG, numw))
    out.append((11, title, tcol, tw))
    if place is None:
        out.append((58, "— no session found", DIM, ""))
        return out
    pc = FG if live else DIM
    out.append((58, place, pc, ""))
    out.append((69, harness, pc, ""))
    out.append((76, hid, DIM, ""))
    out.append((85, tilde, DIM, ""))
    out.append((87, when.rjust(3), wcol, ww))
    return out
def heading(repo, counts):
    return [(0, repo, FG, B), (90 - len(counts), counts, DIM, "")]
lines = [
    [(0, "$", GREEN, B), (2, "provenance prs", FG, B)],
    [],
    heading("acme/storefront", "3 open · 1 running · 1 waiting"),
    row("●", GREEN, "#927", B, "Add retry when checkout times out", FG, "", "T3 Code", "Claude", "4f2a91c0", "~", "55m", GREEN, True),
    row("◐", YELLOW, "#931", B, "Show stock levels on the product page", FG, B, "T3 Code", "Codex", "01a1c0de", "", "1h", YELLOW, True, B),
    row("", DIM, "#896", "", "fix: round totals before applying discounts", DIM, "", "T3 Code", "Claude", "d361dde1", "~", "10h", DIM, False),
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
CW, LH, FS, PADX, TOP = 8.4, 22, 14, 24, 52
W = int(PADX * 2 + 115 * CW)
H = int(TOP + LH * len(lines) + 18)
o = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" aria-label="provenance prs output">',
     f'<rect width="{W}" height="{H}" rx="10" fill="{BG}"/>',
     f'<path d="M0 10a10 10 0 0 1 10-10h{W-20}a10 10 0 0 1 10 10v24H0z" fill="{BAR}"/>',
     f'<circle cx="20" cy="17" r="6" fill="#ff5f57"/><circle cx="40" cy="17" r="6" fill="#febc2e"/><circle cx="60" cy="17" r="6" fill="#28c840"/>',
     f'<g font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, monospace" font-size="{FS}" xml:space="preserve">']
for i, line in enumerate(lines):
    y = TOP + i * LH
    for col, text, colour, weight in line:
        if not text:
            continue
        wt = ' font-weight="bold"' if weight else ""
        o.append(f'<text x="{PADX + col * CW:.1f}" y="{y}" fill="{colour}"{wt}>{html.escape(text)}</text>')
o.append("</g></svg>")
open(sys.argv[1], "w").write("\n".join(o) + "\n")

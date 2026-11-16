"""Builds the Software Factory demo's Presentations workspace: Priya's Q3
engineering review of the feature pipeline, with its source material.

The text files live in software-factory/ (the README and the Source Notes);
this script draws the charts, assembles the Word narrative from the notes,
builds the PowerPoint deck from the narrative and the charts, and writes
src/demo/fixtures/software-factory-presentation*.ts. All of it is synthetic.

    pip install -r requirements.txt
    python software_factory.py
"""
import os
from datetime import datetime

from presentation_kit import (
    H, INK, MUTED, GRID, PAD_B, PAD_L, PAD_R, PAD_T, W, Deck, axis, canvas, finish, font, legend, narrative, write_fixtures,
)

FOLDER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "software-factory")
WHEN = datetime(2026, 9, 4, 16, 0, 0)

BRAND = (8, 145, 178)
ORANGE = (234, 88, 12)
SLATE_SOFT = (203, 206, 218)

SOURCES = [
    "Source Notes/01 The Pipeline.md",
    "Source Notes/02 One Issue End to End.md",
    "Source Notes/03 Delivery.md",
    "Source Notes/04 CI and Releases.md",
    "Source Notes/05 Next Quarter.md",
]
NARRATIVE = "Q3 Engineering Review - Narrative.docx"
DECK = "Q3 Engineering Review.pptx"
LEAD_TIME = "Images/Lead Time per Feature.png"
PRS = "Images/Pull Requests by Origin.png"
CI = "Images/CI Job Durations.png"

# ---------------------------------------------------------------------------
# Charts
# ---------------------------------------------------------------------------

def lead_time_chart():
    features = [
        ("1388", 62, False), ("1395", 41, False), ("1402", 38, False), ("1405", 55, False),
        ("1411", 7.5, True), ("1414", 4.2, True), ("1418", 2.1, True), ("1421", 1.3, True),
    ]
    image, draw = canvas("Lead time per feature", "Hours from the ready-for-build label to the last pull request open")
    right = W - PAD_R - 330
    y = axis(draw, 0, 70, 10, lambda v: f"{int(v)} h", right)
    slot = (right - PAD_L) / len(features)
    bar = slot * 0.6
    for i, (key, hours, pipeline) in enumerate(features):
        x0 = PAD_L + i * slot + (slot - bar) / 2
        draw.rounded_rectangle((x0, y(hours), x0 + bar, y(0)), radius=8, fill=BRAND if pipeline else SLATE_SOFT)
        label = f"{hours:g}"
        draw.text((x0 + (bar - draw.textlength(label, font=font(25, "semi"))) / 2, y(hours) - 38), label, font=font(25, "semi"), fill=INK)
        name = f"LIN-{key}"
        draw.text((x0 + (bar - draw.textlength(name, font=font(22))) / 2, H - PAD_B + 14), name, font=font(22), fill=MUTED)
    legend(draw, right + 50, PAD_T + 20, [("By hand", SLATE_SOFT), ("Pipeline", BRAND)])
    draw.text((right + 50, PAD_T + 150), "Median", font=font(25), fill=MUTED)
    draw.text((right + 50, PAD_T + 188), "48 h → 3.2 h", font=font(40, "semi"), fill=BRAND)
    return finish(image, f"Source: Linear label history, GitHub · {SOURCES[2]}")

def prs_chart():
    weeks = [29, 30, 31, 32, 33, 34, 35, 36]
    people = [14, 12, 15, 11, 13, 12, 10, 12]
    pipeline = [0, 0, 0, 2, 4, 6, 7, 9]
    image, draw = canvas("Merged pull requests a week", "Stacked by who opened them · the pipeline started in week 32")
    right = W - PAD_R - 330
    y = axis(draw, 0, 25, 5, lambda v: f"{int(v)}", right)
    slot = (right - PAD_L) / len(weeks)
    bar = slot * 0.6
    for i, (week, a, b) in enumerate(zip(weeks, people, pipeline)):
        x0 = PAD_L + i * slot + (slot - bar) / 2
        draw.rectangle((x0, y(a), x0 + bar, y(0)), fill=SLATE_SOFT)
        if b:
            draw.rectangle((x0, y(a + b), x0 + bar, y(a)), fill=BRAND)
        total = f"{a + b}"
        draw.text((x0 + (bar - draw.textlength(total, font=font(25, "semi"))) / 2, y(a + b) - 38), total, font=font(25, "semi"), fill=INK)
        name = f"W{week}"
        draw.text((x0 + (bar - draw.textlength(name, font=font(24))) / 2, H - PAD_B + 14), name, font=font(24), fill=MUTED)
    legend(draw, right + 50, PAD_T + 20, [("Opened by people", SLATE_SOFT), ("Opened by the pipeline", BRAND)])
    draw.text((right + 50, PAD_T + 150), "Week 36", font=font(25), fill=MUTED)
    draw.text((right + 50, PAD_T + 188), "9 of 21", font=font(40, "semi"), fill=BRAND)
    draw.text((right + 50, PAD_T + 246), "from the pipeline", font=font(25), fill=MUTED)
    return finish(image, f"Source: GitHub, pull requests merged to main · {SOURCES[2]}")

def ci_chart():
    jobs = ["install", "lint", "test (node 22)", "build image"]
    series = [
        ("Last week", [41, 48, 190, 125], SLATE_SOFT),
        ("Slow week", [178, 51, 202, 131], ORANGE),
        ("After #486", [44, 49, 188, 126], BRAND),
    ]
    image, draw = canvas("CI job medians", "Seconds per job, ci.yml on main · only install moved")
    top = PAD_T + 20
    x0 = PAD_L + 150
    x_max = W - PAD_R - 420
    scale = lambda s: x0 + s / 240 * (x_max - x0)
    for s in range(0, 241, 60):
        x = scale(s)
        draw.line((x, top, x, H - PAD_B), fill=GRID, width=2)
        label = f"{s // 60}m" if s else "0"
        draw.text((x - draw.textlength(label, font=font(22)) / 2, H - PAD_B + 10), label, font=font(22), fill=MUTED)
    row = (H - PAD_B - top) / len(jobs)
    bar = row * 0.22
    duration = lambda s: f"{s // 60}m {s % 60:02d}s" if s >= 60 else f"{s}s"
    for j, job in enumerate(jobs):
        ry = top + j * row + row * 0.12
        draw.text((PAD_L - 40, ry + bar * 1.5 - 16), job, font=font(26, "semi"), fill=INK)
        for k, (_, values, color) in enumerate(series):
            by = ry + k * (bar + 4)
            draw.rectangle((x0, by, scale(values[j]), by + bar), fill=color)
            draw.text((scale(values[j]) + 10, by + bar / 2 - 14), duration(values[j]), font=font(21), fill=MUTED)
    lx = W - PAD_R - 330
    legend(draw, lx, top + 10, [(name, color) for name, _, color in series])
    draw.text((lx, top + 190), "#483 dropped", font=font(25), fill=MUTED)
    draw.text((lx, top + 226), "cache: npm", font=font(34, "semi"), fill=ORANGE)
    draw.text((lx, top + 280), "#486 restored it:", font=font(25), fill=MUTED)
    draw.text((lx, top + 316), "≈2 min off every run", font=font(30, "semi"), fill=BRAND)
    return finish(image, f"Source: GitHub Actions, ci.yml runs on main · {SOURCES[3]}")

IMAGES = {LEAD_TIME: lead_time_chart(), PRS: prs_chart(), CI: ci_chart()}

# ---------------------------------------------------------------------------
# Narrative
# ---------------------------------------------------------------------------

DOCX = narrative(
    FOLDER,
    SOURCES,
    title="The feature pipeline — Q3 engineering review",
    subtitle="Acme engineering all-hands · Priya Natarajan · v2",
    note=(
        "**This document is source material, not the presentation.** It is assembled from the Markdown notes in Source Notes/ "
        f"(01 The Pipeline to 05 Next Quarter), one section per note, and the deck {DECK} is built from it, "
        "with the charts in Images/. It also goes out as the pre-read. Correct a number in its source note, rebuild this document, then the slide."
    ),
    brand=BRAND,
    table_style="Light Grid Accent 5",
    properties=dict(
        title="The feature pipeline — Q3 engineering review",
        subject="Source material for the Q3 engineering review deck",
        author="Priya Natarajan",
        last_modified_by="Priya Natarajan",
        created=WHEN,
        modified=WHEN,
        revision=2,
        keywords="feature pipeline; LIN-1421; CI; narrative; source",
    ),
)

# ---------------------------------------------------------------------------
# Deck
# ---------------------------------------------------------------------------

deck = Deck(
    brand=BRAND,
    soft=(232, 246, 250),
    footer="Q3 engineering review",
    images=IMAGES,
    properties=dict(
        title="The feature pipeline — Q3 engineering review",
        subject="Acme engineering all-hands",
        author="Priya Natarajan",
        last_modified_by="Priya Natarajan",
        created=WHEN,
        modified=WHEN,
        revision=2,
        keywords="feature pipeline; deck",
    ),
)

deck.title_slide(
    "The feature pipeline",
    "One issue in, reviewed pull requests out.",
    ["Q3 engineering review · Acme engineering all-hands", "Priya Natarajan"],
    f"Source: narrative §1 ({NARRATIVE}), from {SOURCES[0]} — the one line for the title slide.",
)

s = deck.slide("Six bots, one hand-off each", "§1", f"Source: narrative §1, from {SOURCES[0]}, which follows runbooks/feature-pipeline.md. Stress the three places people stay in the loop.")
stages = [("intake", "spec"), ("planner", "tasks"), ("implementer", "pull requests"), ("ci-watch", "CI failures"), ("pr-reviewer", "review"), ("release-scribe", "changelog")]
for i, (bot, does) in enumerate(stages):
    deck.card(s, 0.7 + i * 2.05, 1.7, bot, does, w=1.85, size=15)
    if i < len(stages) - 1:
        deck.arrow(s, 0.7 + i * 2.05 + 1.87, 2.18)
deck.text(s, 0.7, 3.4, 12, 3.2, [
    "**People stay in the loop where it matters**",
    "**People merge:** nothing in the pipeline pushes to main.",
    "**People approve production:** Octopus Deploy waits for a second person.",
    "**People own the spec:** edit it in the specs workspace and replay task.ready to rebuild a task.",
], size=19)

s = deck.slide("LIN-1421: label to three pull requests in 80 minutes", "§2", f"Source: narrative §2, from {SOURCES[1]}; the specs workspace and the bots' threads. Open item: the /api-only assumption — update when Linear is reconnected.")
deck.text(s, 0.7, 1.6, 7.3, 5, [
    "**0 min** · ready-for-build fires the Linear webhook",
    "**2 min** · intake writes the spec, and finds #472 and the #482 refill bug",
    "**7 min** · planner splits it into three tasks: bucket, middleware, metrics",
    "**8–80 min** · implementer opens **#491**, **#492** and **#493**, each from its own sandbox",
    "ci-watch steers a lint failure into #492's task, which fixes it in the same PR",
    "#491 and #492 are approved; #493's review is waiting on a provider error",
], size=17)
deck.card(s, 8.6, 1.6, "Open question", "Does the limit apply to /api only? Linear returned 401, so the question is in the spec. Assumed: /api only.", w=4.1, h=2.0, size=22)
deck.card(s, 8.6, 3.85, "People's part", "Review and merge, and answering the spec's question.", w=4.1, h=1.4, size=22)

s = deck.slide("Lead time: 48 hours → 3.2", "§3", f"Source: narrative §3, from {SOURCES[2]}; chart {LEAD_TIME}. Lead time stops at PRs open — review and merge are still ours.")
deck.picture(s, LEAD_TIME)
deck.text(s, 9.3, 1.7, 3.6, 5, [
    "Median from the label to the last pull request open.",
    "The first pipeline runs were slower: the specs were still in the wiki, where the bots could not read them.",
    "Review and merge are not in this number.",
], size=17)

s = deck.slide("The pipeline's work is on top, not instead", "§3", f"Source: narrative §3, from {SOURCES[2]}; chart {PRS}. Land the bottleneck: open-to-merge went from 5 h to 9 h.")
deck.picture(s, PRS)
deck.card(s, 9.3, 1.6, "9 of 21", "pull requests merged in week 36 came from the pipeline")
deck.card(s, 9.3, 2.95, "5 h → 9 h", "median from open to merge: review is the bottleneck")

s = deck.slide("The slow CI week was one line", "§4", f"Source: narrative §4, from {SOURCES[3]}; chart {CI}. The status board was down (502), so the timings came from the workflow runs.")
deck.picture(s, CI)
deck.text(s, 9.3, 1.7, 3.6, 5, [
    "**#483** bumped setup-node to v5 and dropped **cache: npm**.",
    "install went from 41 s to 2 m 58 s; nothing else moved.",
    "**#486** restored it: about 2 minutes off every run.",
], size=17)

s = deck.slide("v2.14.1: both fixes, promoting now", "§4", f"Source: narrative §4 (the scheduler flake, v2.14.1), from {SOURCES[3]} and acme-web/CHANGELOG.md. Update to 'in production' once the second approval lands.")
deck.card(s, 0.7, 1.7, "#486", "CI restores the npm cache; install is back under a minute", w=5.8, h=1.3)
deck.card(s, 0.7, 3.2, "#487", "the scheduler retry test no longer uses the wall clock; the one-in-five flake is gone", w=5.8, h=1.3)
deck.text(s, 7.0, 1.7, 5.8, 4.5, [
    "**Deploy checklist**",
    "Staging soaked for 4 hours: no new Sentry issues, a flat dead-letter count.",
    "Promotion waits for the second approval in Octopus Deploy, as the runbook asks.",
], size=18)

s = deck.slide("Next quarter: review, questions, credentials", "§5", f"Source: narrative §5, from {SOURCES[4]}. The three asks are the close.")
deck.text(s, 0.7, 1.6, 6.2, 5, [
    "**What changes**",
    "A daily review slot per squad, starting from pr-reviewer's notes.",
    "Intake holds the build when a spec has an open question that changes the interface.",
    "Credential rotations go on the deploy checklist, with reconnecting as a step.",
    "Follow-ups: delete src/api/rate-limit.ts (#472); per-key overrides in the admin UI.",
], size=17)
deck.text(s, 7.3, 1.6, 5.5, 5, [
    "**Asks**",
    "1. One reviewer from each squad on the daily slot, from week 38.",
    "2. Someone with Linear admin to reconnect the workspace key this week.",
    "3. Agreement to track \"PRs open\" as lead time, with review time beside it.",
], size=17)

# ---------------------------------------------------------------------------

write_fixtures(
    "software-factory",
    """
The Presentations workspace in the Software Factory universe: Priya's Q3
engineering review of the feature pipeline, with the material it is built
from. The Markdown notes under Source Notes/ are the source; the Word
narrative is assembled from them; the PowerPoint deck is built from the
narrative and the charts under Images/. The content follows the rest of the
universe: LIN-1421 through the bots, the slow CI week (#483, #486), the
scheduler flake (#487), and v2.14.1. All of it is synthetic.
""",
    FOLDER,
    ["README.md", *SOURCES],
    {NARRATIVE: DOCX, DECK: deck.save(), **IMAGES},
)

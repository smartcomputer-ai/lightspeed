"""Builds the Personal Assistant demo's Presentations workspace: Ada's Series A
deck for Northlight's investment committee, with its source material.

The text files live in personal-assistant/ (the README and the Source Notes);
this script draws the charts, assembles the Word narrative from the notes,
builds the PowerPoint deck from the narrative and the charts, and writes
src/demo/fixtures/personal-assistant-presentation*.ts. All of it is synthetic.

    pip install -r requirements.txt
    python personal_assistant.py
"""
import os
from datetime import datetime

from presentation_kit import (
    H, INK, MUTED, PAD_B, PAD_L, PAD_R, PAD_T, W, Deck, axis, canvas, finish, font, narrative, write_fixtures,
)

FOLDER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "personal-assistant")
WHEN = datetime(2026, 9, 1, 9, 0, 0)

INDIGO = (79, 70, 229)
INDIGO_SOFT = (165, 160, 245)
TEAL = (13, 148, 136)
AMBER = (217, 119, 6)
ROSE = (225, 29, 72)
SLATE = (148, 152, 168)

SOURCES = [
    "Source Notes/01 The Story.md",
    "Source Notes/02 Traction.md",
    "Source Notes/03 Market and Pricing.md",
    "Source Notes/04 Product.md",
    "Source Notes/05 Team and Raise.md",
]
NARRATIVE = "Lumen Series A - Narrative.docx"
DECK = "Lumen Series A - Northlight IC.pptx"
MRR = "Images/MRR Growth.png"
COHORTS = "Images/Cohort Retention.png"
PRICING = "Images/Pricing Comparison.png"

# ---------------------------------------------------------------------------
# Charts
# ---------------------------------------------------------------------------

def mrr_chart():
    months = ["Sep 25", "Oct", "Nov", "Dec", "Jan 26", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug"]
    mrr = [231, 244, 259, 268, 281, 297, 314, 330, 349, 366, 384, 412]
    image, draw = canvas("MRR, last twelve months", "€k at month end · 1.8× in twelve months · ARR run rate €4.9M")
    y = axis(draw, 0, 450, 50, lambda v: f"€{int(v)}k")
    slot = (W - PAD_L - PAD_R) / len(mrr)
    bar = slot * 0.62
    for i, (month, value) in enumerate(zip(months, mrr)):
        x0 = PAD_L + i * slot + (slot - bar) / 2
        last = i == len(mrr) - 1
        draw.rounded_rectangle((x0, y(value), x0 + bar, y(0)), radius=8, fill=INDIGO if last else INDIGO_SOFT)
        label = f"{value}"
        f = font(24, "semi" if last else "regular")
        draw.text((x0 + (bar - draw.textlength(label, font=f)) / 2, y(value) - 36), label, font=f, fill=INK if last else MUTED)
        draw.text((x0 + (bar - draw.textlength(month, font=font(24))) / 2, H - PAD_B + 14), month, font=font(24), fill=MUTED)
    return finish(image, "Source: Monday metrics digest · Source Notes/02 Traction.md")

def cohort_chart():
    months = [0, 3, 6, 9, 12, 18]
    cohorts = [
        ("Q1 2025 · 7 customers", [100, 104, 111, 117, 124, 132], INDIGO, "132% at M18"),
        ("Q2 2025 · 6 customers", [100, 106, 112, 119, 125], TEAL, "125% at M12"),
        ("Q3 2025 · 5 customers", [100, 103, 110, 116], AMBER, "116% at M9"),
        ("Q4 2025 · 6 customers", [100, 105, 113], ROSE, "113% at M6"),
    ]
    image, draw = canvas("Net revenue retention by 2025 cohort", "Revenue as % of month 0, by months since the first invoice")
    y = axis(draw, 90, 140, 10, lambda v: f"{int(v)}%")
    plot_w = W - PAD_L - PAD_R - 80
    x = lambda m: PAD_L + 30 + m / 18 * (plot_w - 30)
    draw.line((PAD_L, y(100), PAD_L + plot_w, y(100)), fill=SLATE, width=3)
    for m in months:
        label = f"M{m}"
        draw.text((x(m) - draw.textlength(label, font=font(24)) / 2, H - PAD_B + 14), label, font=font(24), fill=MUTED)
    for _, values, color, _ in cohorts:
        points = [(x(m), y(v)) for m, v in zip(months, values)]
        draw.line(points, fill=color, width=6, joint="curve")
        for px, py in points:
            draw.ellipse((px - 9, py - 9, px + 9, py + 9), fill=color, outline="white", width=3)
    # The legend sits in the empty lower right, under the curves.
    lx = W - PAD_R - 520
    for i, (name, _, color, latest) in enumerate(cohorts):
        ly = y(121) + i * 50
        draw.rounded_rectangle((lx, ly + 6, lx + 30, ly + 30), radius=6, fill=color)
        draw.text((lx + 44, ly), name, font=font(25), fill=INK)
        draw.text((lx + 330, ly), latest, font=font(25, "semi"), fill=color)
    return finish(image, "Source: cohort cut sent with the data room · Source Notes/02 Traction.md")

def pricing_chart():
    tiers = ["Entry tier", "Mid tier"]
    vendors = [
        ("Lumen", [49, 89], INDIGO, "€", "+ usage: €0.004 a run above 50k a month"),
        ("Beacon Analytics", [39, 79], SLATE, "$", "+ usage: $0.50 per 1k events above 1M a month"),
        ("Orbit Insights", [45, 95], (190, 193, 206), "$", "seats only"),
    ]
    image, draw = canvas("List price per user per month", "Published prices, in the currency each vendor lists")
    y = axis(draw, 0, 100, 20, lambda v: f"{int(v)}")
    group = (W - PAD_L - PAD_R - 520) / len(tiers)
    bar = group * 0.22
    for t, tier in enumerate(tiers):
        gx = PAD_L + t * group + group * 0.12
        for v, (_, prices, color, currency, _) in enumerate(vendors):
            x0 = gx + v * (bar + 14)
            draw.rounded_rectangle((x0, y(prices[t]), x0 + bar, y(0)), radius=8, fill=color)
            label = f"{currency}{prices[t]}"
            draw.text((x0 + (bar - draw.textlength(label, font=font(26, "semi"))) / 2, y(prices[t]) - 40), label, font=font(26, "semi"), fill=INK)
        span = 3 * bar + 28
        draw.text((gx + (span - draw.textlength(tier, font=font(26))) / 2, H - PAD_B + 14), tier, font=font(26), fill=MUTED)
    lx = W - PAD_R - 500
    for i, (name, _, color, _, usage) in enumerate(vendors):
        ly = PAD_T + 30 + i * 110
        draw.rounded_rectangle((lx, ly + 6, lx + 30, ly + 30), radius=6, fill=color)
        draw.text((lx + 44, ly), name, font=font(27, "semi"), fill=INK)
        draw.text((lx + 44, ly + 40), usage, font=font(22), fill=MUTED)
    draw.text((lx, PAD_T + 30 + 3 * 110 + 20), "Only Lumen grows with runs.", font=font(27, "semi"), fill=INDIGO)
    return finish(image, "Source: /memory/research/competitor-pricing.md · Source Notes/03 Market and Pricing.md")

IMAGES = {MRR: mrr_chart(), COHORTS: cohort_chart(), PRICING: pricing_chart()}

# ---------------------------------------------------------------------------
# Narrative
# ---------------------------------------------------------------------------

DOCX = narrative(
    FOLDER,
    SOURCES,
    title="Lumen — Series A narrative",
    subtitle="Northlight Ventures investment committee · 8 September 2026 · v3",
    note=(
        "**This document is source material, not the presentation.** It is assembled from the Markdown notes in Source Notes/ "
        f"(01 The Story to 05 Team and Raise), one section per note, and the deck {DECK} is built from it: "
        "one slide per section, with the charts in Images/. Correct a number in its source note, rebuild this document, then the slide."
    ),
    brand=INDIGO,
    table_style="Light Grid Accent 1",
    properties=dict(
        title="Lumen — Series A narrative",
        subject="Source material for the Series A deck",
        author="Ada Demo",
        last_modified_by="Briefing writer",
        created=WHEN,
        modified=WHEN,
        revision=3,
        keywords="Series A; Northlight Ventures; narrative; source",
    ),
)

# ---------------------------------------------------------------------------
# Deck
# ---------------------------------------------------------------------------

deck = Deck(
    brand=INDIGO,
    soft=(241, 240, 253),
    footer="Lumen · Series A",
    images=IMAGES,
    properties=dict(
        title="Lumen — Series A",
        subject="Northlight Ventures investment committee, 8 September 2026",
        author="Ada Demo",
        last_modified_by="Briefing writer",
        created=WHEN,
        modified=WHEN,
        revision=3,
        keywords="Series A; Northlight Ventures; deck",
    ),
)

deck.title_slide(
    "Lumen",
    "Workflows that pay for themselves as they run.",
    ["Series A · Northlight Ventures investment committee · 8 September 2026", "Ada Demo, founder and CEO"],
    f"Source: narrative §1 ({NARRATIVE}), from {SOURCES[0]} — the one line for the title slide.",
    title_size=66,
)

s = deck.slide("Operations work is still run by hand", "§1", f"Source: narrative §1, from {SOURCES[0]}. Lead with the per-seat point: value grows with runs, not logins.")
deck.text(s, 0.7, 1.6, 5.8, 5, [
    "**The problem**",
    "Approvals, handoffs and exceptions live in email, spreadsheets and five or six SaaS tools.",
    "Each workflow is too small for an engineering project, so it stays manual.",
    "Workflow tools charge per seat, which caps what they earn and makes customers ration access.",
], size=18)
deck.text(s, 7.0, 1.6, 5.7, 5, [
    "**Why now**",
    "AI agents can do the long-tail steps: reading an invoice, chasing a missing field, routing an exception.",
    "Runs grow faster than headcount: customers' runs grew **3.4×** in twelve months, seats **1.3×**.",
    "Mid-market buyers are consolidating to one place for workflows.",
], size=18)

s = deck.slide("€412k MRR, 1.8× in twelve months", "§2", f"Source: narrative §2, from {SOURCES[1]}; chart {MRR}. Numbers from the Monday metrics digest; coverage from the corrected CRM export.")
deck.picture(s, MRR)
deck.card(s, 9.3, 1.6, "€4.9M", "ARR run rate")
deck.card(s, 9.3, 2.95, "1.1%", "net revenue churn a month")
deck.card(s, 9.3, 4.3, "31", "paying customers · Acme 9.3% of MRR")
deck.card(s, 9.3, 5.65, "3.1×", "Q4 pipeline coverage")

s = deck.slide("Every 2025 cohort expands, and keeps expanding", "§2", f"Source: narrative §2 (cohort retention), from {SOURCES[1]}; chart {COHORTS}. The same cut Elena has in the data room.")
deck.picture(s, COHORTS)
deck.text(s, 9.3, 1.7, 3.6, 5, [
    "Above **100%** by month 3 in every cohort.",
    "Q1 2025 cohort at **132%** by month 18.",
    "Expansion is runs, not seats: usage is **41%** of the Q1 cohort's revenue at month 18, against 12% at month 0.",
    "References: Acme Corp, Halden Logistics, Marrow Health.",
], size=17)

s = deck.slide("Priced to grow with the work", "§3", f"Source: narrative §3, from {SOURCES[2]}, which copies /memory/research/competitor-pricing.md; chart {PRICING}. Do not argue seat price; argue total cost at the customer's run volume.")
deck.picture(s, PRICING)
deck.text(s, 9.3, 1.7, 3.6, 5, [
    "Lumen costs more per seat than Beacon, but it is the only one of the three that grows with runs.",
    "Beacon: free for 3 users; raised Starter $29 → $39 in March.",
    "Orbit: seats only; Scale tier since January.",
    "Neither publishes net retention.",
], size=17)

s = deck.slide("One platform, and a second product on it", "§4", f"Source: narrative §4, from {SOURCES[3]}. Open item: September usage for Lumen Approvals once the month closes — Elena asked to see it before IC. Migration: one line, plan in September; no dates.")
deck.text(s, 0.7, 1.6, 5.8, 5, [
    "**The platform**",
    "140 templates for finance, procurement, logistics and HR operations.",
    "Every step is a person, an agent or an integration; a person approves anything that leaves the company.",
    "60 connectors; **9.8M** runs in August, from 2.9M a year ago.",
    "Platform migration: plan in September.",
], size=18)
deck.text(s, 7.0, 1.6, 5.7, 5, [
    "**Lumen Approvals** (since May 2026)",
    "Used by **11 of 31** customers; 4 came to Lumen through it.",
    "**2,140** weekly active approvers; **63k** approvals in August.",
    "Median time to approve: 2.1 days by email → **4 hours**.",
], size=18)

s = deck.slide("About 40 people across Munich, the Netherlands and the UK", "§5", f"Source: narrative §5 (team), from {SOURCES[4]}.")
for i, (name, role) in enumerate([
    ("Ada Demo", "Founder and CEO"),
    ("Marco Ruiz", "Chief of staff · board pack, go-to-market hiring"),
    ("Priya Natarajan", "Head of engineering · 14 engineers, three squads"),
]):
    deck.card(s, 0.7 + i * 4.1, 1.8, name, role, h=1.5)
deck.text(s, 0.7, 3.6, 12, 2, ["**Hiring now:** Staff engineer (offer out) · Account executives (two finalists for the first seat)"], size=18)

s = deck.slide("€12M Series A", "§5", f"Source: narrative §5 (the raise, use of funds, milestones), from {SOURCES[4]}. Runway from the finance memo of Aug 20.")
deck.card(s, 0.7, 1.6, "19 → 36 months", "runway, before and after the round")
deck.card(s, 0.7, 2.95, "€4.9M → €12M", "ARR in the next 18 months")
deck.card(s, 0.7, 4.3, "> 120%", "NRR for every cohort at month 12")
deck.card(s, 0.7, 5.65, "1 in 3", "new customers through Lumen Approvals")
deck.text(s, 5.0, 1.6, 7.8, 5, [
    "**Use of funds**",
    "**45% go-to-market:** 6 account executives across DE, NL and UK; partner channel.",
    "**35% product and engineering:** Lumen Approvals; the platform migration (plan in September).",
    "**20% customer success:** onboarding and expansion for usage-heavy accounts.",
], size=18)

# ---------------------------------------------------------------------------

write_fixtures(
    "personal-assistant",
    """
The Presentations workspace in the Personal Assistant universe: the Series A
deck Ada presents to Northlight's investment committee, with the material it
is built from. The Markdown notes under Source Notes/ are the source; the
Word narrative is assembled from them; the PowerPoint deck is built from the
narrative and the charts under Images/. The numbers match the rest of the
universe: the Monday digest, the competitor pricing the research bot wrote,
and the Q3 board draft. All of it is synthetic.
""",
    FOLDER,
    ["README.md", *SOURCES],
    {NARRATIVE: DOCX, DECK: deck.save(), **IMAGES},
)

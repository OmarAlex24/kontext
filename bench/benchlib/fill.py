"""Filler for the `kind = "fill"` turns: plausible documents from a credit and
collections department (minutes, tickets, logbooks, emails), generated
deterministically from a seed, with no network.

They fill the agent's window before compacting. The scenario's distractors
(facts almost identical to the real ones) are planted at fixed positions by the
seed, to punish confusion.

The same `(docs, seed, chars, distractors)` always gives the same text, on any
machine: `random.Random` with a text seed is stable across Python 3 versions.
"""

import hashlib
import random

# Characters per token of this filler. The 2.1 was measured with the count_tokens
# API on the Spanish version of this filler (haiku, sonnet and opus 5.5 gave the
# same): 96,395 characters = 45,836 tokens. It is less than the ~4 of English
# prose because of the numbers, folios and proper names. The English version has
# not been measured yet: re-measure it before trusting the token counts.
CHARS_PER_TOKEN = 2.1

DOCS = ("minutes", "tickets", "logbook", "emails", "mixed")

# Names that don't clash with the scenarios' facts (Ruiz, Ríos, Olvera,
# Barragán, Barrera): those only appear where a scenario puts them.
CUSTOMERS = [
    "Paint Shop La Esperanza", "Steelworks del Bajío", "Centauro Paints", "Quintero Lumber",
    "Ávalos Plumbing", "El Clavo Hardware", "Construrama Tepic", "Montaño Electrical",
    "Ferrecasa Group", "Lerma Building Materials", "Saucedo Fittings", "Peralta Screws & Bolts",
    "San Judas Hardware", "Cota Glass & Aluminum", "Ponce Hydraulics", "Ferremax Celaya",
    "Nava Distributors", "Lupita Hardware", "Ybarra Trading", "Mora Waterproofing",
    "Tapia Brothers Hardware", "Villalobos Cement", "Saltillo Hardware", "Galindo Flooring",
    "La Llave Hardware", "Bautista Home Center", "Del Valle Hardware", "Leal Pipes & Fittings",
]
PEOPLE = [
    "Rocío Mendoza", "Javier Treviño", "Lucía Paredes", "Héctor Salinas", "Marisol Cano",
    "Ernesto Villa", "Gabriela Fuentes", "Óscar Domínguez", "Patricia Lozano", "Raúl Gallardo",
    "Verónica Ibarra", "Arturo Medina", "Fernanda Robles", "Saúl Contreras", "Liliana Esquivel",
    "Tomás Aguirre", "Daniela Orozco", "Iván Cervantes", "Mónica Zavala", "Enrique Solís",
]
AREAS = ["credit", "collections", "treasury", "sales", "systems", "warehouse", "invoicing", "legal"]
SERVICES = ["reminders", "reconciliation", "invoicing", "spei-payments", "reports", "erp-sync"]
MONTHS = ["September", "October", "November"]
DAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"]

TOPICS = [
    ("SPEI payment reconciliation", [
        "Transfers keep arriving without a reference; {p} proposes asking for the receipt number in the description.",
        "Treasury reports {n} deposits that can't be identified this fortnight, for {monto}.",
        "The bank's layout was reviewed: the reference column comes cut at 20 characters.",
    ]),
    ("Email bounces", [
        "There were {n} bounces last week, almost all from domains with a full mailbox.",
        "{p} cleaned up {n} duplicate addresses in the customer catalog.",
        "Sales was asked to confirm the collections emails for {c} and {c2}.",
    ]),
    ("Overdue portfolio", [
        "The portfolio over 60 days came to {monto}, {pct} less than the previous month.",
        "{c} asked for a detailed statement; {p} sends it today.",
        "The five largest balances make up {pct} of what's overdue.",
    ]),
    ("Credit notes", [
        "{n} credit notes were issued for returns from {c}.",
        "{p} found two notes applied twice to the same invoice; they get corrected this week.",
        "Invoicing asks that the notes carry the source invoice in the description.",
    ]),
    ("ERP migration", [
        "The nightly sync took {n} minutes; systems will split it by branch.",
        "The ERP customer catalog still doesn't have the RFC for {n} customers.",
        "{p} asked to freeze changes to the payment-terms catalog until {dia}.",
    ]),
    ("Customer portal", [
        "{c} can't download their XML files from the portal; systems is looking at it.",
        "It was agreed to show the overdue balance on the portal's home page.",
        "{n} customers activated card payment from the portal this month.",
    ]),
    ("Collection visits", [
        "{p} visited {c}; they promise to pay {monto} before {dia}.",
        "The visit route for the industrial corridor was rescheduled because of the roadwork on the avenue.",
        "Visits in the south zone move to Thursdays.",
    ]),
]

PROBLEMS = [
    "The customer says they paid invoice {folio} but it still shows as overdue.",
    "The reminder arrived with the total at zero.",
    "They ask to change the address for statements to {email}.",
    "The PDF of the statement is cut off on the second page.",
    "They request copies of the invoices from {month} for their audit.",
    "They dispute a duplicate charge of {monto} on the statement.",
    "They don't recognize invoice {folio}; they say the purchase order was cancelled.",
    "They ask why their credit limit dropped to {monto}.",
]
REPLIES = [
    "The payment was confirmed at the bank and applied to the invoice; it's at zero now.",
    "The statement was resent with the corrected PDF.",
    "The contact was updated in the ERP and sales was notified.",
    "It was escalated to invoicing to review the purchase order.",
    "The proof of payment was requested so it can be applied.",
    "The limits policy was explained and a call with credit was scheduled.",
]
EVENTS = [
    "run finished: {n} records processed, {m} with errors",
    "retry {m}/3 of the bank connection",
    "{n} statements generated as PDF",
    "send queue empty; next run scheduled",
    "warning: {m} customers without a collections email",
    "reconciliation: {n} payments applied, {m} without a reference",
    "full backup of the portfolio database ({n} MB)",
    "{m} bounced emails removed from the catalog",
]
AGREEMENTS = [
    "{p} sends the corrected report before {dia}.",
    "{c} is asked for the proof of payment in writing.",
    "systems reviews the process and brings a proposal to the next meeting.",
    "it stays as is and is reviewed in a month.",
    "{p} and {p2} put together the list of affected customers.",
    "sales is told not to take orders from {c} until this is cleared up.",
]
SUBJECTS = [
    "Statement for {month}", "Follow-up on pending payments", "Change of collections contact",
    "Clarification on invoice {folio}", "Weekly portfolio report", "Pending items from the {dia} meeting",
]
BODIES = [
    "I'm sharing the portfolio report for the week. Overdue went down to {monto}.",
    "I'm waiting for confirmation of the payment on invoice {folio}.",
    "As we saw at the meeting, {c} will pay in two parts; the first one is {monto}.",
    "Can you help me check why {c} shows up with two email addresses?",
    "Attaching the reconciliation for {month}; {n} deposits are still unidentified.",
    "Systems asks for a window on {dia} at night to update the ERP.",
]


def _fmt(rng, template):
    return template.format(
        p=rng.choice(PEOPLE),
        p2=rng.choice(PEOPLE),
        c=rng.choice(CUSTOMERS),
        c2=rng.choice(CUSTOMERS),
        n=rng.randint(3, 480),
        m=rng.randint(1, 40),
        monto=f"${rng.randint(1_000, 900_000):,}.{rng.randint(0, 99):02d}",
        pct=f"{rng.randint(2, 38)}%",
        folio=f"{rng.choice('ABCF')}-{rng.randint(1000, 99999)}",
        dia=f"{rng.choice(DAYS)} {rng.randint(1, 28)}",
        month=rng.choice(MONTHS),
        email=f"payments{rng.randint(1, 99)}@{rng.choice(['mail.mx', 'company.com.mx', 'group.mx'])}",
    )


def _date(rng):
    return f"2026-{rng.randint(9, 11):02d}-{rng.randint(1, 28):02d}"


def _minutes(rng, k):
    date = _date(rng)
    who = ", ".join(rng.sample(PEOPLE, 5))
    lines = [f"Minutes {k} of the credit and collections meeting, {date}", f"Attendees: {who}", ""]
    for i, (topic, phrases) in enumerate(rng.sample(TOPICS, 3), 1):
        body = " ".join(_fmt(rng, f) for f in rng.sample(phrases, 2))
        action = _fmt(rng, rng.choice(AGREEMENTS))
        action = action[0].upper() + action[1:]
        lines.append(f"{i}. {topic}. {body} Action: {action} Owner: {rng.choice(PEOPLE)}, due {_date(rng)}.")
    return lines


def _ticket(rng, k):
    c = rng.choice(CUSTOMERS)
    return [
        f"Ticket #{40000 + k * 7 + rng.randint(0, 6)} · {c} · priority {rng.choice(['low', 'medium', 'high'])}",
        f"Opened by {rng.choice(PEOPLE)} ({rng.choice(AREAS)}) on {_date(rng)}.",
        _fmt(rng, rng.choice(PROBLEMS)),
        f"Reply from {rng.choice(PEOPLE)}: {rng.choice(REPLIES)}",
        f"Status: {rng.choice(['closed', 'waiting on the customer', 'escalated', 'resolved'])}.",
    ]


def _logbook(rng, k):
    date = _date(rng)
    out = []
    for _ in range(6):
        hour = f"{rng.randint(0, 23):02d}:{rng.randint(0, 59):02d}:{rng.randint(0, 59):02d}"
        out.append(f"{date} {hour} [{rng.choice(SERVICES)}] {_fmt(rng, rng.choice(EVENTS))}")
    return out


def _email(rng, k):
    sender, to = rng.sample(PEOPLE, 2)
    user = sender.split()[0].lower().replace("ó", "o").replace("í", "i").replace("é", "e").replace("á", "a").replace("ú", "u")
    return [
        f"From: {sender} <{user}@arteaga.mx>",
        f"To: {to}",
        f"Subject: {_fmt(rng, rng.choice(SUBJECTS))}",
        "",
        " ".join(_fmt(rng, c) for c in rng.sample(BODIES, 3)),
        "",
        f"Regards,\n{sender}",
    ]


MAKERS = {"minutes": _minutes, "tickets": _ticket, "logbook": _logbook, "emails": _email}
CARRIERS = {
    "minutes": "Other business: {t}.",
    "tickets": "Internal note on the ticket: {t}.",
    "logbook": "operator note: {t}",
    "emails": "By the way, {t}.",
}


def generate(docs, chars, seed, distractors=()):
    """Blocks of `docs` until it passes `chars` characters, with the distractors
    (whole sentences) planted in the blocks in the middle."""
    if docs not in DOCS:
        raise ValueError(f"unknown filler {docs!r} (there are {', '.join(DOCS)})")
    # The seed prefix stays `ctx-bench:`: it is only a label for the seed, never shown.
    rng = random.Random(f"ctx-bench:{docs}:{seed}")
    blocks = []
    size = 0
    k = 1
    while size < chars:
        kind = rng.choice(list(MAKERS)) if docs == "mixed" else docs
        text = "\n".join(MAKERS[kind](rng, k))
        blocks.append((kind, text))
        size += len(text) + 2
        k += 1
    # The distractors go between 20 % and 80 % of the text, never at the end.
    n = len(blocks)
    for i, d in enumerate(distractors):
        lo, hi = max(0, n // 5), max(0, (4 * n) // 5 - 1)
        at = rng.randint(lo, max(lo, hi))
        kind, text = blocks[at]
        blocks[at] = (kind, text + "\n" + CARRIERS[kind].format(t=d.rstrip(".")))
    return "\n\n".join(t for _, t in blocks)


def prompt(ask, docs, tokens, seed, distractors=(), scale=1.0):
    """The whole turn: the request and the document. Returns (text, characters of
    the document)."""
    chars = max(200, int(tokens * scale * CHARS_PER_TOKEN))
    doc = generate(docs, chars, seed, distractors)
    return f"{ask.strip()}\n\n---\n{doc}\n---\n", len(doc)


def sha256(text):
    return hashlib.sha256(text.encode()).hexdigest()

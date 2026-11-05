#!/usr/bin/env bash
# Builds a sample project with realistic data to see kontext working:
#
#   examples/demo.sh [folder]
#
# Creates in <folder> (by default, a temporary one) a kontext-home `billing`, its
# machine configuration, and a code repo, and writes three days of work: a
# signed charter and goals, guarded decisions (one superseded), an epic with
# tasks in each state, a fact that went stale, an unreviewed distill proposal,
# a task that serves no goal, questions, a second writer (ana@laptop), and three
# readers with their cursor at different points (omar, pm, and epic:reminders).
#
# It does not touch ~/.config/kontext: everything uses KONTEXT_CONFIG_DIR inside
# <folder>. It uses the binary from $KONTEXT_BIN, or `kontext` from the PATH, or
# target/debug/kontext of this repo. The `kontext ui` tests use it as is.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -n "${KONTEXT_BIN:-}" ]]; then
  kontext_bin="$KONTEXT_BIN"
elif [[ -x "$here/../target/debug/kontext" ]]; then
  kontext_bin="$here/../target/debug/kontext"
elif command -v kontext >/dev/null 2>&1; then
  kontext_bin="$(command -v kontext)"
else
  echo "demo.sh: kontext not found; run cargo build or set KONTEXT_BIN" >&2
  exit 1
fi

dir="${1:-$(mktemp -d "${TMPDIR:-/tmp}/kontext-demo.XXXXXX")}"
mkdir -p "$dir"
dir="$(cd "$dir" && pwd)"
if [[ -e "$dir/home" ]]; then
  echo "demo.sh: $dir already has a sample project; use another folder" >&2
  exit 1
fi

home="$dir/home"
repo="$dir/billing"
cfg="$dir/config"
cfg_ana="$dir/config-ana"
mkdir -p "$cfg" "$cfg_ana"
printf 'writer = "omar@mac"\n' >"$cfg/projects.toml"

now="$(date -u +%s)"
# The time N seconds ago, in ISO-8601 (GNU date or BSD date).
iso() {
  local t=$((now - $1))
  date -u -d "@$t" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -r "$t" +%Y-%m-%dT%H:%M:%SZ
}
H=3600
D=$((24 * H))

# kontext as omar (or whoever KONTEXT_ACTOR says), at the instant "N seconds ago".
c() {
  local ago="$1"
  shift
  KONTEXT_NOW="$(iso "$ago")" KONTEXT_CONFIG_DIR="$cfg" KONTEXT_PROJECT=billing "$kontext_bin" "$@"
}
# The same, as another actor: a <ago> <actor> args…
a() {
  local ago="$1" actor="$2"
  shift 2
  KONTEXT_ACTOR="$actor" c "$ago" "$@"
}
# From ana's machine.
ana() {
  local ago="$1"
  shift
  KONTEXT_NOW="$(iso "$ago")" KONTEXT_CONFIG_DIR="$cfg_ana" KONTEXT_PROJECT=billing \
    KONTEXT_ACTOR=human:ana "$kontext_bin" "$@"
}
# The id of what a command writes.
id() {
  local ago="$1" actor="$2"
  shift 2
  a "$ago" "$actor" --json "$@" | sed -n 's/^  "id": "\([^"]*\)".*/\1/p' | head -n 1
}
# The assignment mark from `kontext next --claim`.
claim() {
  local ago="$1" actor="$2" epic="$3"
  KONTEXT_ROLE=implementer a "$ago" "$actor" --json next --epic "$epic" --claim |
    sed -n 's/^  "mark": "\([^"]*\)".*/\1/p'
}
g() { git -C "$repo" -c user.name=Omar -c user.email=omar@example.com "$@"; }

# --- The kontext-home and the code repo ---------------------------------------

KONTEXT_CONFIG_DIR="$cfg" "$kontext_bin" init billing --home "$home" >/dev/null
# The owner comes from git config user.name; in the example it is omar. Claims
# last 3 days, so a claimed task is still claimed if you open this tomorrow.
sed -i.bak -e 's/^owners = .*/owners = ["omar"]/' -e 's/^lease = .*/lease = "3d"/' "$home/project.toml"
rm -f "$home/project.toml.bak"
cat >>"$home/project.toml" <<'EOF'

[[repos]]
name = "billing"
docs = ["docs/**/*.md", "**/README.md"]
EOF
cat >>"$cfg/projects.toml" <<EOF

[projects.billing.repos]
billing = "$repo"
EOF
printf 'writer = "ana@laptop"\n\n[projects.billing]\nhome = "%s"\n' "$home" >"$cfg_ana/projects.toml"

mkdir -p "$repo/src" "$repo/tests"
cat >"$repo/README.md" <<'EOF'
# billing

Payment reminders for overdue invoices.
EOF
cat >"$repo/src/invoices.py" <<'EOF'
"""Overdue invoices: they come from the overdue_invoices view."""

VIEW = "overdue_invoices"


def overdue(db):
    return db.query(f"select * from {VIEW}")
EOF
cat >"$repo/src/amounts.py" <<'EOF'
def cents(text):
    """'1,250.50' -> 125050"""
    whole, _, dec = text.replace(",", "").partition(".")
    return int(whole) * 100 + int((dec + "00")[:2])
EOF
g init -q -b main
g add -A
g commit -q -m "feat: overdue invoices and amounts in cents"

# --- Day 1: charter, goals, decisions, and the epic ---------------------------

OMAR=human:omar
A=$(id $((3 * D)) $OMAR charter \
  --title "Collect overdue invoices without calling anyone" \
  --objective "Customers with overdue invoices get reminders and pay without anyone calling them" \
  --non-goal "Sending SMS or WhatsApp" \
  --non-goal "Changing how invoices are issued" \
  --done-when "80% of overdue invoices get at least one reminder")
G1=$(id $((3 * D - 60)) $OMAR goal add "Email reminders to customers with overdue invoices" \
  --done-when "Every morning one email goes out per customer with their overdue invoices")
G2=$(id $((3 * D - 90)) $OMAR goal add "Record partial payments" \
  --done-when "A partial payment lowers the balance and does not stop the reminders")
G3=$(id $((3 * D - 100)) $OMAR goal add "Weekly collections report for management")
a $((3 * D - 120)) $OMAR sign "$A" >/dev/null
a $((3 * D - 130)) $OMAR sign "$G1" >/dev/null
a $((3 * D - 140)) $OMAR sign "$G2" >/dev/null
a $((3 * D - 145)) $OMAR sign "$G3" >/dev/null

D1=$(id $((3 * D - 300)) $OMAR decide "Send the emails with Postmark, not our own SMTP" \
  --why "Our own SMTP lands in spam; Postmark already has the domain verified" \
  --tags email --guard-cmd "! grep -rqi smtplib src/")
D2=$(id $((3 * D - 360)) $OMAR decide "Amounts are stored in cents, as integers" \
  --why "With floats, balances don't add up when partial payments are summed" \
  --tags amounts --guard-cmd "! grep -rn 'float(' src/")
D3=$(id $((3 * D - 400)) $OMAR decide "Send the reminders at 8:00" \
  --why "Before people open their email for the day" --tags email)

E=$(id $((3 * D - 600)) $OMAR epic add "Payment reminders" --goal "$G1" --name reminders)
COORD=agent:epic:reminders
T1=$(id $((3 * D - 700)) $COORD task add "Reminder email template" --epic reminders \
  --acceptance "The template lists the overdue invoices with amount and days late")
T2=$(id $((3 * D - 710)) $COORD task add "Compute days late per invoice" --epic reminders \
  --acceptance "tests/test_days_late.py passes")
T3=$(id $((3 * D - 720)) $COORD task add "Send the reminders every morning" --epic reminders \
  --depends-on "$T1" --depends-on "$T2" --acceptance "A cron sends one email per customer")
T4=$(id $((3 * D - 730)) $COORD task add "Send log per customer" --epic reminders \
  --acceptance "Each send records date, customer, and result")
T5=$(id $((3 * D - 740)) $COORD task add "Tests for amounts in cents" --epic reminders \
  --goal "$G2" --acceptance "tests/test_amounts.py covers commas and decimals")
KONTEXT_ROLE=coordinator a $((3 * D - 800)) $COORD brief reminders >/dev/null
a $((3 * D - 820)) $COORD assign "$T4" --to agent:impl-2 >/dev/null

M=$(claim $((3 * D - 900)) agent:impl-1 reminders)
a $((3 * D - 3000)) agent:impl-1 log "Template in templates/reminder.html with the invoice table" \
  --task "$T1" --next "Review the subject line with omar" >/dev/null
a $((3 * D - 3600)) agent:impl-1 done "$T1" --since "$M" \
  --evidence 'cmd:"pytest tests/test_template.py"=0' --evidence "pr:https://example.com/billing/pull/12" >/dev/null

(cd "$repo" && KONTEXT_ROLE=scout a $((3 * D - 4000)) agent:scout-1 fact \
  "Overdue invoices come from the overdue_invoices view" --path src/invoices.py --epic reminders >/dev/null)
(cd "$repo" && KONTEXT_ROLE=scout a $((3 * D - 4100)) agent:scout-1 fact \
  "Amounts are converted to cents in src/amounts.py" --path src/amounts.py >/dev/null)

# Omar reviews at the end of the day: his cursor stays here.
a $((3 * D - 5000)) $OMAR standup >/dev/null

# --- Day 2 --------------------------------------------------------------------

Q1=$(KONTEXT_NOW="$(iso $((2 * D)))" KONTEXT_CONFIG_DIR="$cfg_ana" KONTEXT_PROJECT=billing KONTEXT_ACTOR=human:ana \
  "$kontext_bin" --json ask "Should the reminder be copied to the account manager?" --to owner \
  --body "Some large customers only answer their account manager" |
  sed -n 's/^  "id": "\([^"]*\)".*/\1/p' | head -n 1)
ana $((2 * D - 600)) log "I reviewed the large-customer list: 14 have an account manager assigned" >/dev/null

D4=$(id $((2 * D - 1200)) $OMAR decide "Send the reminders at 10:00, in the customer's time zone" \
  --why "At 8:00, the Tijuana customers receive them at 6:00" --tags email --supersedes "$D3")

# What the distill proposed when it compacted a session: unreviewed.
a $((2 * D - 2000)) agent:distill propose "Retry sending three times if Postmark fails" \
  --why "The session showed a 503 from Postmark that left 3 customers without email" --tags email >/dev/null

# An implementer on an old version logged a task with no goal.
printf '%s\n' "{\"kind\":\"task\",\"actor\":{\"type\":\"agent\",\"name\":\"impl-2\",\"role\":\"implementer\"},\"title\":\"Export the billing report to Excel\",\"refs\":[\"$E\"],\"fields\":{\"epic\":\"$E\",\"goals\":[],\"depends_on\":[],\"acceptance\":\"\"},\"hlc\":null}" |
  KONTEXT_NOW="$(iso $((2 * D - 2500)))" KONTEXT_CONFIG_DIR="$cfg" KONTEXT_PROJECT=billing "$kontext_bin" debug append >/dev/null

# The code changed: the fact about invoices.py goes stale.
cat >"$repo/src/invoices.py" <<'EOF'
"""Overdue invoices: now they come from the invoices table, with their due date."""


def overdue(db, today):
    return db.query("select * from invoices where due < ? and balance > 0", today)
EOF
g commit -q -am "refactor: overdue invoices from the table, without the view"

# The pm catches up: his cursor stays here.
KONTEXT_ROLE=pm a $((2 * D - 3000)) agent:pm resume --as pm >/dev/null

# --- Day 3 (today) ------------------------------------------------------------

ana $((5 * H)) fact "60% of customers pay within 3 days of the reminder" \
  --body "Measured with the manual sends from October" >/dev/null
KONTEXT_ROLE=coordinator a $((4 * H)) $COORD ask "Does a partial payment stop the reminders for that invoice?" \
  --to pm --body "The partial-payments goal says no, but the template says «full balance»" >/dev/null
M2=$(claim $((40 * 60)) agent:impl-1 reminders)
a $((20 * 60)) agent:impl-1 log "Days late against the due date, not counting the issue day" \
  --task "$T2" --next "Month-end cases" >/dev/null
a $((10 * 60)) agent:impl-2 log "sends table with customer, date, and result; the index per customer is missing" \
  --task "$T4" >/dev/null

# The "read" date comes from the date of the cursor files: it must be the
# date when they read it in the history, not now.
stamp() {
  local f="$home/cursors/$1/omar@mac.json"
  touch -d "$(iso "$2")" "$f" 2>/dev/null || true
}
stamp omar $((3 * D - 5000))
stamp pm $((2 * D - 3000))
stamp epic:reminders $((3 * D - 800))

cat <<EOF
Sample project ready in $dir
  kontext-home:  $home
  repo:      $repo
  writers: omar@mac and ana@laptop; readers: omar, pm, and epic:reminders
  charter $A, goals $G1 $G2 $G3, epic $E (tasks $T1 $T2 $T3 $T4 $T5)
  decisions $D1 $D2 $D4 (supersedes $D3); ana's question $Q1; impl-1's mark ${M2:-?}

To look at it:
  KONTEXT_CONFIG_DIR=$cfg KONTEXT_ACTOR=human:omar $kontext_bin --project billing ui --open
  KONTEXT_CONFIG_DIR=$cfg KONTEXT_ACTOR=human:omar $kontext_bin --project billing standup --peek
EOF

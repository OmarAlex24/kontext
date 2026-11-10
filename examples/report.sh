#!/usr/bin/env bash
# Builds a sample project to look at the human's inbox and the report:
#
#   examples/report.sh [folder]
#
# Creates in <folder> (by default, a temporary one) a kontext-home `card-payments`
# with three days of work by a team of agents: 3 epics (payments, notices after
# payments, and refunds), 6 pending items for omar (a goal and a proposal to
# sign, three questions, one of them not urgent because of the brake, and what
# the distill recorded), 2 problems (a reopened task and an expired claim),
# 4 screenshots (the payment screen before and after, the receipt email, and the
# approved refunds mockup), the status paragraph, and the code repo
# <folder>/app with omar's ideas in `.pm/` (skill kontext-pm) for "Your ideas".
#
# At the end it writes <folder>/report.html with `kontext report --html` (run
# from <folder>/app, where it finds `.pm/`) and prints `kontext inbox`. It does not touch ~/.config/kontext: everything uses
# KONTEXT_CONFIG_DIR inside <folder>. It uses $KONTEXT_BIN, or target/debug/kontext
# of this repo, or the kontext on the PATH.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -n "${KONTEXT_BIN:-}" ]]; then
  kontext_bin="$KONTEXT_BIN"
elif [[ -x "$here/../target/debug/kontext" ]]; then
  kontext_bin="$here/../target/debug/kontext"
elif command -v kontext >/dev/null 2>&1; then
  kontext_bin="$(command -v kontext)"
else
  echo "report.sh: kontext not found; run cargo build or set KONTEXT_BIN" >&2
  exit 1
fi

dir="${1:-$(mktemp -d "${TMPDIR:-/tmp}/kontext-report.XXXXXX")}"
mkdir -p "$dir"
dir="$(cd "$dir" && pwd)"
if [[ -e "$dir/home" ]]; then
  echo "report.sh: $dir already has a sample project; use another folder" >&2
  exit 1
fi
home="$dir/home"
cfg="$dir/config"
shots="$here/screenshots"
mkdir -p "$cfg"
printf 'writer = "omar@mac"\n' >"$cfg/projects.toml"

now="$(date -u +%s)"
iso() {
  local t=$((now - $1))
  date -u -d "@$t" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -r "$t" +%Y-%m-%dT%H:%M:%SZ
}
H=3600
D=$((24 * H))

# kontext as <actor> at the instant "N seconds ago": a <ago> <actor> args…
a() {
  local ago="$1" actor="$2"
  shift 2
  KONTEXT_NOW="$(iso "$ago")" KONTEXT_CONFIG_DIR="$cfg" KONTEXT_PROJECT=card-payments \
    KONTEXT_ACTOR="$actor" "$kontext_bin" "$@"
}
id() { a "$@" --json | sed -n 's/^  "id": "\([^"]*\)".*/\1/p' | head -n 1; }
# id with --json before the subcommand (for those that do not accept --json at the end).
idj() {
  local ago="$1" actor="$2"
  shift 2
  a "$ago" "$actor" --json "$@" | sed -n 's/^  "id": "\([^"]*\)".*/\1/p' | head -n 1
}
mark() {
  local ago="$1" actor="$2" epic="$3"
  KONTEXT_ROLE=implementer a "$ago" "$actor" --json next --epic "$epic" --claim |
    sed -n 's/^  "mark": "\([^"]*\)".*/\1/p'
}
coord() {
  local ago="$1" epic="$2"
  shift 2
  KONTEXT_ROLE=coordinator a "$ago" "agent:epic:$epic" "$@"
}

KONTEXT_CONFIG_DIR="$cfg" "$kontext_bin" init card-payments --home "$home" >/dev/null
# Claims expire after 4 h without activity (the preferences one, from 7 h ago, expired).
sed -i.bak -e 's/^owners = .*/owners = ["omar"]/' -e 's/^lease = .*/lease = "4h"/' "$home/project.toml"
rm -f "$home/project.toml.bak"
cd "$dir"

OMAR=human:omar
# --- Day 1: phase 0 with omar -------------------------------------------------
A=$(idj $((3 * D)) $OMAR charter --title "Charge invoices by card in the app" \
  --objective "Customers pay their invoice by card from the link in the email and get their receipt" \
  --non-goal "Electronic tax invoices (CFDI)" --done-when "First real charge from Lopez Hardware")
a $((3 * D)) $OMAR sign "$A" >/dev/null
G1=$(idj $((3 * D)) $OMAR goal add "Card payments" --done-when "3 charges in a row without error")
G2=$(idj $((3 * D)) $OMAR goal add "Payment notices to customers")
a $((3 * D)) $OMAR sign "$G1" >/dev/null
a $((3 * D)) $OMAR sign "$G2" >/dev/null
DSTRIPE=$(idj $((3 * D)) $OMAR decide "Charge with Stripe, not Conekta" --why "3D Secure and recurring charges" --tags payments)
# The pm proposed the refunds goal and omar has not signed it.
G3=$(KONTEXT_ROLE=pm idj $((3 * D - H)) agent:pm goal add "Refunds from the dashboard")
a $((3 * D - H)) agent:pm epic add "Card payments" --goal "$G1" --name payments >/dev/null
a $((3 * D - H)) agent:pm epic add "Payment notices" --goal "$G2" --name notices --after payments >/dev/null
a $((3 * D - H)) agent:pm epic add "Refunds" --goal "$G3" --name refunds --after payments >/dev/null
# The refunds mockup, approved by omar as the reference.
a $((3 * D - 2 * H)) $OMAR decide "The refunds screen follows the approved mockup" \
  --why "omar reviewed it in phase 0" --epic refunds --tags refunds \
  --attach "$shots/refund.png" --screen refund >/dev/null

# --- Day 2: payments (time always moves forward: "ago" keeps shrinking) ------
tid() { sed -n 's/^  "id": "\([^"]*\)".*/\1/p' | head -n 1; }
T1=$(coord $((2 * D)) payments --json task add "Payment screen" --epic payments --acceptance "pnpm e2e payment passes" | tid)
T2=$(coord $((2 * D)) payments --json task add "Charge webhooks" --epic payments --acceptance "pnpm test webhooks passes" | tid)
T3=$(coord $((2 * D)) payments --json task add "Receipt by email" --epic payments --depends-on "$T2" --acceptance "pnpm test receipt passes" | tid)
m=$(mark $((2 * D - H)) agent:payments-impl-1 payments)
KONTEXT_ROLE=implementer a $((2 * D - 2 * H)) agent:payments-impl-1 done "$T1" --since "$m" \
  --evidence 'cmd:"pnpm e2e payment"=0' --evidence "$shots/payment-v1.png" --screen payment >/dev/null
m=$(mark $((2 * D - 3 * H)) agent:payments-impl-2 payments)
KONTEXT_ROLE=implementer a $((2 * D - 4 * H)) agent:payments-impl-2 done "$T2" --since "$m" \
  --evidence 'cmd:"pnpm test webhooks"=0' >/dev/null
coord $((D + 9 * H)) payments assign "$T3" --to agent:payments-impl-2 >/dev/null
T4=$(coord $((D + 8 * H)) payments --json task add "Payment screen with 3D Secure" --epic payments --acceptance "pnpm e2e payment-3ds passes" | tid)
m=$(mark $((D + 7 * H)) agent:payments-impl-1 payments)
# The tester found that the webhook charges twice: reopens it.
KONTEXT_ROLE=tester a $((D + 6 * H)) agent:payments-tester reopen "$T2" \
  --why "charges twice when Stripe retries the webhook" >/dev/null
KONTEXT_ROLE=implementer a $((D + 5 * H)) agent:payments-impl-1 done "$T4" --since "$m" \
  --evidence 'cmd:"pnpm e2e payment-3ds"=0' --evidence "$shots/payment-v2.png" --screen payment >/dev/null
KONTEXT_ROLE=implementer a $((D + 4 * H)) agent:payments-impl-2 ask "Does the receipt include the customer's tax ID (RFC)?" --task "$T3" --to owner >/dev/null
coord $((D + 4 * H)) payments propose "Save cards in a Stripe Customer for recurring charges" \
  --why "avoids asking for the card every month" --epic payments >/dev/null

# --- Days 2 and 3: notices ----------------------------------------------------
T5=$(coord $((D + 3 * H)) notices --json task add "Receipt email" --epic notices --acceptance "pnpm test email passes" | tid)
coord $((D + 3 * H)) notices task add "Customer notice preferences" --epic notices >/dev/null
T6=$(coord $((D + 3 * H)) notices --json task add "Reminder SMS" --epic notices | tid)
coord $((D + 3 * H)) notices task add "Schedule reminders" --epic notices --depends-on "$T6" >/dev/null
m=$(mark $((D + 2 * H)) agent:notices-impl-1 notices)
KONTEXT_ROLE=implementer a $((D + H)) agent:notices-impl-1 done "$T5" --since "$m" \
  --evidence 'cmd:"pnpm test email"=0' --evidence "$shots/email.png" --screen email >/dev/null
QSMS=$(coord $((D)) notices --json ask "Should we send SMS in addition to email?" --to owner --task "$T6" --blocking | tid)
KONTEXT_ROLE=implementer a $((20 * H)) agent:notices-impl-2 ask "Which SMS provider do we use?" --to owner --epic notices >/dev/null
# An agent claimed the preferences and never wrote again: the claim expires.
mark $((7 * H)) agent:notices-impl-3 notices >/dev/null

# --- What the distill recorded from the conversation with omar ----------------
for t in "The pilot customer is Lopez Hardware" "Reminders go out at 9 a.m., Mexico City time"; do
  printf '{"kind":"fact","title":"%s","actor":{"type":"agent","name":"distill"},"fields":{"source":"distill","origin":"user","fingerprints":[]}}\n' "$t" |
    KONTEXT_NOW="$(iso $((6 * H)))" KONTEXT_CONFIG_DIR="$cfg" KONTEXT_PROJECT=card-payments "$kontext_bin" debug append >/dev/null
done

# --- Today: the pm writes the status and wants to ask something else ----------
KONTEXT_ROLE=pm a $((2 * H)) agent:pm log --status \
  "Card payments with 3D Secure work in test, and the receipt email goes out fine. Notices are stuck until you decide whether we send SMS, and the webhook that charges twice has the receipt on hold. I need your signature on the refunds goal and your answers to the notices questions." >/dev/null
# The brake: omar already has many pending items, so this one is marked not urgent.
KONTEXT_ROLE=pm a $H agent:pm ask "Should we publish the app to the Play Store this week?" --to owner || true

# --- omar's ideas, as the pm keeps them in the code repo (.pm/) ---------------
app="$dir/app"
mkdir -p "$app/.pm/dumps" "$app/.pm/shots"
git -C "$app" init -q
day3="$(iso $((3 * D)) | cut -c1-10)"
day1="$(iso $((D + 2 * H)) | cut -c1-10)"
cat >"$app/.pm/dumps/$day3.md" <<EOF
## $day3 10:12

**User:**

customers should pay their invoice by card straight from the link in the email
let's go with Stripe, it does 3D Secure and we will want recurring charges later
after paying they get the receipt by email, with the invoice attached
and I want refunds from the dashboard, without calling support
some customers asked to pay cash at an OXXO store
EOF
cat >"$app/.pm/dumps/$day1.md" <<EOF
## $day1 18:40

**User:**

text them a reminder the day before the invoice is due
what about faxing the invoices to the old customers? no, forget it
EOF
cp "$shots/payment-v2.png" "$app/.pm/shots/payment.png"
cat >"$app/.pm/items.md" <<EOF
# Ideas

| id | idea | source | state | where | evidence |
|---|---|---|---|---|---|
| P1 | Pay the invoice by card from the email link | dumps/$day3.md:5 | in progress | $T1, $T4 | PR #14, shots/payment.png |
| P2 | Charge with Stripe | dumps/$day3.md:6 | decided | $DSTRIPE | |
| P3 | Receipt by email after paying | dumps/$day3.md:7 | pending | $T3 | |
| P4 | Refunds from the dashboard | dumps/$day3.md:8 | pending | Refunds epic, after payments | |
| P5 | Pay cash at an OXXO store | dumps/$day3.md:9 | pending | | |
| P6 | SMS reminder the day before it is due | dumps/$day1.md:5 | pending | $QSMS | |
| P7 | Fax the invoices to old customers | dumps/$day1.md:6 | discarded | | |
EOF

cd "$app"
a 0 $OMAR report --html -o "$dir/report.html"
echo
a 0 $OMAR inbox
echo
echo "Project in $home"
echo "Report: $dir/report.html"
echo "To see it live: cd $app && KONTEXT_CONFIG_DIR=$cfg KONTEXT_ACTOR=human:omar $kontext_bin --project card-payments ui --open   (and open /report)"

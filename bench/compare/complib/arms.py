"""The six arms (design §1.2): image, the secrets each one needs, its own
environment, and its --allowedTools on top of the common list."""

from dataclasses import dataclass, field
from pathlib import Path

ARMS_DIR = Path(__file__).resolve().parent.parent / "arms"
ARM_NAMES = ["plain", "kontext", "gentle-ai", "mem0", "beads", "openspec"]
# Every environment variable a real run reads from run.py's environment, and
# which arms get it. Nothing else of the host's environment reaches a container.
KEY_NAMES = {
    "ANTHROPIC_API_KEY": ARM_NAMES,
    "MEM0_API_KEY": ["mem0"],
}


@dataclass
class Arm:
    name: str
    sidecar: bool = False
    env: dict = field(default_factory=dict)

    @property
    def image(self):
        return f"bench-{self.name}:latest"

    @property
    def dir(self):
        return ARMS_DIR / self.name

    def secret_names(self, dry_run):
        """The keys this arm's containers get (none in dry runs)."""
        if dry_run:
            return []
        return [k for k, arms in KEY_NAMES.items() if self.name in arms]

    def run_env(self, run_id):
        """Non-secret environment of this arm's containers."""
        e = dict(self.env)
        if self.name == "mem0":
            # Its own personal memory per run; the repo's remote is unique per
            # run too, which keys the shared project memory (design §6.1).
            e["MEM0_USER_ID"] = f"bench-{run_id}"
            e["MEM0_TELEMETRY"] = "false"
        if self.name == "openspec":
            e["OPENSPEC_TELEMETRY"] = "0"
        return e

    def allowed_tools(self):
        common = _lines(ARMS_DIR / "allowed_common.txt")
        own = _lines(self.dir / "allowed_tools.txt")
        return common + [t for t in own if t not in common]

    def script(self, name):
        p = self.dir / f"{name}.sh"
        return p if p.exists() else None


def _lines(p):
    if not p.exists():
        return []
    return [x.strip() for x in p.read_text().splitlines() if x.strip() and not x.startswith("#")]


ARMS = {n: Arm(n, sidecar=(n == "kontext")) for n in ARM_NAMES}

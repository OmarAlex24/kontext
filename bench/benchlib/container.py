"""Docker for the comparison benchmark (bench/compare, design §6.1 and §7).

A run is a private network, a named volume that holds the bare git remote, one
long-lived container per machine (A, and B from day 4), the kontext sidecar for
the kontext arm, and a verification container with no network. Every turn is a
`docker exec … claude -p …` into the machine's container.

Isolation rules, kept by construction and checked by the tests:

- no bind mount, ever: files go in and out as tar streams through `docker exec`
  (so they are owned by the container's user, and no host path is exposed);
- a secret (API key, admin secret, token) reaches docker by name only
  (`-e NAME`), with its value in the environment of that one docker process, so
  it never appears in an argv, a log or a file;
- the docker CLI itself runs with an environment stripped of every
  ANTHROPIC_*, MEM0_* and KONTEXT_* variable of the host, plus only the
  secrets that call needs.

Every argv is built by a pure `argv_*` function, so the tests can check these
rules without Docker.
"""

import io
import os
import re
import secrets as _secrets
import shutil
import subprocess
import tarfile
import time

DOCKER = os.environ.get("BENCH_DOCKER", "docker")
# Variables of the host that never reach a docker process unless a call names them.
STRIPPED_PREFIXES = ("ANTHROPIC_", "MEM0_", "KONTEXT_", "CLAUDE_", "OPENAI_", "CLOUDFLARE_")
LABEL = "org.kontext.bench.run"
SIDECAR_ALIAS = "kontext-sidecar"
SIDECAR_PORT = 8787
REMOTES = "/remotes"
HOME = "/home/bench"
# Resources per agent container: 4 at once fit in the ~8 GB Docker Desktop VM.
MEMORY = os.environ.get("BENCH_CONTAINER_MEMORY", "1536m")
CPUS = os.environ.get("BENCH_CONTAINER_CPUS", "2")


class DockerError(RuntimeError):
    pass


# ------------------------------------------------------------ argv builders


def _env_args(env=None, secret_names=()):
    out = []
    for k, v in (env or {}).items():
        if any(k.startswith(p) for p in ("ANTHROPIC_", "MEM0_API_KEY")) or k in secret_names:
            raise ValueError(f"{k} is a secret: pass it by name (secret_names), never by value")
        out += ["-e", f"{k}={v}"]
    for name in secret_names:
        out += ["-e", name]
    return out


def argv_network_create(name, run_id):
    return [DOCKER, "network", "create", "--label", f"{LABEL}={run_id}", name]


def argv_volume_create(name, run_id):
    return [DOCKER, "volume", "create", "--label", f"{LABEL}={run_id}", name]


def argv_run_machine(name, image, network, volume, run_id, hostname, env=None, secret_names=()):
    """A machine: `sleep infinity`, its own hostname, the run's network and the
    remotes volume (a named volume, not a host path)."""
    return [
        DOCKER, "run", "-d", "--init",
        "--name", name,
        "--hostname", hostname,
        "--network", network,
        "--label", f"{LABEL}={run_id}",
        "--memory", MEMORY, "--cpus", CPUS,
        "--mount", f"type=volume,src={volume},dst={REMOTES}",
        *_env_args(env, secret_names),
        image, "sleep", "infinity",
    ]  # fmt: skip


def argv_run_sidecar(name, image, network, run_id):
    return [
        DOCKER, "run", "-d", "--init",
        "--name", name,
        "--network", network,
        "--network-alias", SIDECAR_ALIAS,
        "--label", f"{LABEL}={run_id}",
        "--memory", "768m",
        "-e", f"KONTEXT_SIDECAR_PORT={SIDECAR_PORT}",
        "-e", "KONTEXT_DEV_ADMIN_SECRET",
        image,
    ]  # fmt: skip


def argv_run_verify(name, image, run_id):
    """The verification container: no network, no secret, no volume."""
    return [
        DOCKER, "run", "-d", "--init",
        "--name", name,
        "--network", "none",
        "--label", f"{LABEL}={run_id}",
        "--memory", "512m",
        image, "sleep", "infinity",
    ]  # fmt: skip


def argv_exec(container, argv, env=None, secret_names=(), workdir=None, stdin=True, user=None):
    out = [DOCKER, "exec"]
    if stdin:
        out.append("-i")
    if user is not None:
        out += ["-u", str(user)]
    if workdir:
        out += ["-w", workdir]
    out += _env_args(env, secret_names)
    return out + [container, *argv]


def argv_rm(names):
    return [DOCKER, "rm", "-f", "-v", *names]


# ------------------------------------------------------------ running docker


def docker_env(secret_env=None):
    """The environment of a docker CLI process: the host's, without any
    variable that could carry a key, plus only `secret_env`."""
    env = {k: v for k, v in os.environ.items() if not k.startswith(STRIPPED_PREFIXES)}
    env.update(secret_env or {})
    return env


class Docker:
    """Runs docker commands. `calls` keeps every argv (never an env value)."""

    def __init__(self, log=print):
        self.log = log
        self.calls = []

    def available(self):
        return shutil.which(DOCKER) is not None

    def run(self, argv, input=None, secret_env=None, timeout=None, check=True, text=True):
        self.calls.append(list(argv))
        try:
            r = subprocess.run(
                argv,
                input=input,
                capture_output=True,
                text=text,
                env=docker_env(secret_env),
                timeout=timeout,
            )
        except subprocess.TimeoutExpired as e:
            r = subprocess.CompletedProcess(argv, 124, e.stdout or ("" if text else b""), f"timed out after {timeout} s")
        if check and r.returncode != 0:
            err = r.stderr if text else r.stderr.decode(errors="replace")
            raise DockerError(f"{' '.join(argv[:4])} … failed ({r.returncode}): {str(err).strip()[-800:]}")
        return r

    def image_id(self, image):
        r = self.run([DOCKER, "image", "inspect", image, "--format", "{{.Id}}"], check=False)
        return r.stdout.strip() if r.returncode == 0 else None

    def labels(self, image):
        r = self.run([DOCKER, "image", "inspect", image, "--format", "{{json .Config.Labels}}"], check=False)
        return r.stdout.strip() if r.returncode == 0 else None


# ------------------------------------------------------------ tar streams


def tar_of_dir(path, prefix=".", exclude=()):
    """A tar (bytes) of a host folder, every entry owned by uid/gid 1001."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as t:

        def owner(ti):
            name = ti.name.split("/", 1)[-1] if "/" in ti.name else ti.name
            if any(part in exclude for part in name.split("/")):
                return None
            ti.uid = ti.gid = 1001
            ti.uname = ti.gname = "bench"
            return ti

        t.add(str(path), arcname=prefix, filter=owner)
    return buf.getvalue()


def tar_of_files(files):
    """A tar of {arcname: bytes}."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as t:
        for name, data in files.items():
            ti = tarfile.TarInfo(name)
            ti.size = len(data)
            ti.mode = 0o755 if name.endswith((".sh", ".py")) else 0o644
            ti.uid = ti.gid = 1001
            t.addfile(ti, io.BytesIO(data))
    return buf.getvalue()


# ------------------------------------------------------------ a machine


class Machine:
    """One container that lives for the whole run (or until the machine changes)."""

    def __init__(self, docker, name, image, network, volume, run_id, hostname, env=None, secret_env=None):
        self.docker = docker
        self.name = name
        self.image = image
        self.network = network
        self.volume = volume
        self.run_id = run_id
        self.hostname = hostname
        self.env = dict(env or {})
        # name -> value; only the names go in an argv, and only a `docker exec`
        # argv: a key given to `docker run` would be stored in the container's
        # config (`docker inspect` shows it) for as long as the container exists.
        # Every process the agent starts inherits it from its exec instead.
        self.secret_env = dict(secret_env or {})
        self.started = False

    def start(self):
        argv = argv_run_machine(
            self.name, self.image, self.network, self.volume, self.run_id, self.hostname,
            env=self.env,
        )  # fmt: skip
        self.docker.run(argv)
        self.started = True

    def exec(self, argv, input=None, env=None, secret_env=None, workdir=None, timeout=None, check=False, text=True, user=None):
        secret_env = {**self.secret_env, **(secret_env or {})}
        full = argv_exec(
            self.name, argv, env=env, secret_names=tuple(secret_env), workdir=workdir, stdin=input is not None, user=user
        )
        return self.docker.run(full, input=input, secret_env=secret_env, timeout=timeout, check=check, text=text)

    def sh(self, script, env=None, secret_env=None, workdir=None, timeout=600, check=True):
        return self.exec(["bash", "-c", script], env=env, secret_env=secret_env, workdir=workdir, timeout=timeout, check=check)

    def put_tar(self, data, dest):
        self.exec(["mkdir", "-p", dest], check=True)
        self.exec(["tar", "-x", "-C", dest, "--no-same-owner"], input=data, check=True, text=False)

    def put_dir(self, host_dir, dest, exclude=("__pycache__",)):
        self.put_tar(tar_of_dir(host_dir, exclude=exclude), dest)

    def put_files(self, files, dest):
        self.put_tar(tar_of_files(files), dest)

    def get_tar(self, src, exclude=()):
        """A tar (bytes) of a folder in the container, or b"" if it is missing."""
        ex = []
        for e in exclude:
            ex += [f"--exclude={e}"]
        r = self.exec(["bash", "-c", f'[ -d "$1" ] && tar -c -C "$1" {" ".join(ex)} . || true', "_", src], check=False, text=False)
        return r.stdout or b""

    def remove(self):
        self.docker.run(argv_rm([self.name]), check=False)
        self.started = False


class Sidecar:
    """The local kontext server (server/dev.mjs's options, host 0.0.0.0)."""

    def __init__(self, docker, name, image, network, run_id):
        self.docker = docker
        self.name = name
        self.image = image
        self.network = network
        self.run_id = run_id
        # A fresh admin secret per run: only this run's setup sees it.
        self.admin_secret = _secrets.token_urlsafe(24)
        self.url = f"http://{SIDECAR_ALIAS}:{SIDECAR_PORT}"

    def start(self, timeout=60):
        self.docker.run(
            argv_run_sidecar(self.name, self.image, self.network, self.run_id),
            secret_env={"KONTEXT_DEV_ADMIN_SECRET": self.admin_secret},
        )
        t0 = time.monotonic()
        while time.monotonic() - t0 < timeout:
            r = self.docker.run([DOCKER, "logs", self.name], check=False)
            if "listening on" in (r.stdout or "") + (r.stderr or ""):
                return self.url
            time.sleep(0.5)
        raise DockerError("the kontext sidecar did not start")

    def remove(self):
        self.docker.run(argv_rm([self.name]), check=False)


# ------------------------------------------------------------ verification


def parse_unittest_verbose(text):
    """{test name: "ok" | "FAIL" | "ERROR" | "skipped"} from `unittest -v` output."""
    out = {}
    for m in re.finditer(r"^(test_\w+) \([^)]*\)(?:\n[^\n]*?)? \.\.\. (ok|FAIL|ERROR|skipped)", text, re.M):
        out[m.group(1)] = m.group(2)
    return out


class Verifier:
    """Runs hidden checks against a repo snapshot in a container with no network.
    The checks enter only this container, and only after the agent's
    containers of that day are done with the snapshot."""

    def __init__(self, docker, name, image, run_id):
        self.docker = docker
        self.name = name
        self.image = image
        self.run_id = run_id

    def check(self, snapshot_tar, checks_dir, modules, env=None, timeout=300):
        """Returns {module: {"tests": {...}, "exit_code", "output_tail"}}."""
        self.docker.run(argv_run_verify(self.name, self.image, self.run_id))
        try:
            m = Machine(self.docker, self.name, self.image, "none", None, self.run_id, "verify")
            m.put_tar(snapshot_tar, "/home/verify/repo")
            m.put_dir(checks_dir, "/home/verify/checks")
            out = {}
            for mod in modules:
                e = {"PYTHONPATH": "/home/verify/repo", "BENCH_REPO": "/home/verify/repo", "PYTHONDONTWRITEBYTECODE": "1"}
                e.update(env or {})
                r = m.exec(
                    ["python3", "-m", "unittest", "-v", mod],
                    env=e,
                    workdir="/home/verify/checks",
                    timeout=timeout,
                )
                text = (r.stdout or "") + (r.stderr or "")
                out[mod] = {"tests": parse_unittest_verbose(text), "exit_code": r.returncode, "output_tail": text[-3000:]}
            return out
        finally:
            self.docker.run(argv_rm([self.name]), check=False)


# ------------------------------------------------------------ cleanup


def cleanup_run(docker, run_id):
    """Removes every container, network and volume labelled with this run."""
    flt = f"label={LABEL}={run_id}"
    r = docker.run([DOCKER, "ps", "-aq", "--filter", flt], check=False)
    ids = r.stdout.split()
    if ids:
        docker.run(argv_rm(ids), check=False)
    for kind in ("network", "volume"):
        r = docker.run([DOCKER, kind, "ls", "-q", "--filter", flt], check=False)
        for x in r.stdout.split():
            docker.run([DOCKER, kind, "rm", x], check=False)

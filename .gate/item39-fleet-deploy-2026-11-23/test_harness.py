#!/usr/bin/env python3
"""Deterministic protocol test for the item-39 fleet deploy scripts.

SELF-CONTAINED: resolves the deploy scripts and stubs relative to its own
location, so it runs from any writable directory containing:
  deploy_dns3.sh, deploy_pi.sh, test_stub_systemctl.sh, test_stub_cargo.sh.
Everything it writes goes into ./protocol-test-run/ (RESULTS.md,
transcripts.log, sandboxes, compiled fake daemons).

Stubs: systemctl (state machine that spawns REAL ELF daemons so
/proc/<pid>/exe is a genuine running inode), cargo (installs a fake new
binary via temp+rename, modeling cargo's write discipline), and a local
JSON-RPC HTTP stub (validates the Basic auth the scripts derive from the
conf, serves controlled responses incl. JSON-RPC error objects and bad tip
hashes). Real: bash, git, install, sha256sum, timeout, python3.

Each scenario runs a deploy script in a fresh sandbox against the stubs and
MEASURES: exit code, final binary digest, backup digest, repo HEAD/branch,
running-inode digest, daemon liveness, restart count.
"""
import base64
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = f"{HERE}/protocol-test-run"
STUBS = f"{ROOT}/stubs"
DNS3 = f"{HERE}/deploy_dns3.sh"
PI = f"{HERE}/deploy_pi.sh"
FAKE_OLD = f"{ROOT}/fake_old"
FAKE_NEW = f"{ROOT}/fake_new"
CONF_USER, CONF_PASS = "stubu", "stubp"
MIN_TIP = "10"
TIP_OK = "a" * 64
HEALTHY = {"mode": "ok", "height": 100, "tip": TIP_OK}
RPC_ERROR = {"mode": "error", "height": 100, "tip": TIP_OK}

results = []
log_lines = []


def log(msg):
    log_lines.append(msg)
    print(msg, flush=True)


def sha(path):
    r = subprocess.run(["sha256sum", path], capture_output=True, text=True)
    if r.returncode != 0:
        return None
    return r.stdout.split()[0]


def exe_sha(pid):
    return sha(f"/proc/{pid}/exe")


class RPCStub(threading.Thread):
    """Local JSON-RPC stub. Validates the Basic auth the scripts derive from conf."""

    def __init__(self, statefile):
        super().__init__(daemon=True)
        self.statefile = statefile
        self.auth = "Basic " + base64.b64encode(f"{CONF_USER}:{CONF_PASS}".encode()).decode()
        self.httpd = HTTPServer(("127.0.0.1", 0), self._handler())
        self.port = self.httpd.server_address[1]

    def _handler(self):
        statefile, auth = self.statefile, self.auth

        class H(BaseHTTPRequestHandler):
            def _send(self, obj):
                data = json.dumps(obj).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
                mode = json.load(open(statefile))
                if self.headers.get("Authorization") != auth:
                    self._send({"error": {"code": -401, "message": "stub auth mismatch"}})
                    return
                m = mode.get("mode", "ok")
                if m == "error":
                    self._send({"error": {"code": -1, "message": "stub controlled rpc error"}})
                elif body.get("method") == "getblockcount":
                    self._send({"result": mode["height"]})
                elif body.get("method") == "getbestblockhash":
                    if m == "badtip":
                        self._send({"result": "NOT-A-HASH"})
                    else:
                        self._send({"result": mode["tip"]})
                else:
                    self._send({"error": {"code": -32601, "message": "unknown method"}})

            def log_message(self, *a):
                pass

        return H

    def run(self):
        self.httpd.serve_forever()

    def stop(self):
        self.httpd.shutdown()


def setup():
    os.makedirs(ROOT, exist_ok=True)
    shutil.rmtree(STUBS, ignore_errors=True)
    os.makedirs(STUBS)
    shutil.copy(f"{HERE}/test_stub_systemctl.sh", f"{STUBS}/systemctl")
    shutil.copy(f"{HERE}/test_stub_cargo.sh", f"{STUBS}/cargo")
    os.chmod(f"{STUBS}/systemctl", 0o755)
    os.chmod(f"{STUBS}/cargo", 0o755)
    oldc = f"{ROOT}/fake_old.c"
    newc = f"{ROOT}/fake_new.c"
    open(oldc, "w").write('#include <unistd.h>\nstatic const char tag[] = "OLD-DAEMON";\nint main(void){ for(;;) pause(); }\n')
    open(newc, "w").write('#include <unistd.h>\nstatic const char tag[] = "NEW-BINARY-BYTES";\nint main(void){ for(;;) pause(); }\n')
    for src, out in ((oldc, FAKE_OLD), (newc, FAKE_NEW)):
        r = subprocess.run(["cc", "-O0", "-o", out, src], capture_output=True, text=True)
        assert r.returncode == 0, r.stderr
    assert sha(FAKE_OLD) != sha(FAKE_NEW), "fake binaries must differ"
    log(f"setup: stubs installed from {HERE}, fakes old={sha(FAKE_OLD)[:12]} new={sha(FAKE_NEW)[:12]}")


def build_repo(sb):
    repo = f"{sb}/repo"
    os.makedirs(repo)
    env = dict(os.environ)
    env.update({"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
                "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"})

    def g(*a, cwd=repo):
        return subprocess.run(["git", "-C", str(cwd)] + list(a), check=True, capture_output=True, text=True, env=env)

    g("init", "-q")
    open(f"{repo}/.gitignore", "w").write("/target/\n")  # mirrors the real repo: build output ignored
    open(f"{repo}/src.rs", "w").write("old\n")
    g("add", ".")
    g("commit", "-q", "-m", "old")
    old = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    subprocess.run(["git", "clone", "-q", "--bare", repo, f"{sb}/origin.git"], check=True, capture_output=True)
    g("remote", "add", "origin", f"{sb}/origin.git")
    g("checkout", "-q", "-b", "tmp")
    open(f"{repo}/src.rs", "w").write("new\n")
    g("add", ".")
    g("commit", "-q", "-m", "new")
    new = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    subprocess.run(["git", "-C", repo, "push", "-q", "origin", "tmp:refs/heads/carry"],
                   check=True, capture_output=True, env=env)
    g("checkout", "-q", "master")
    g("branch", "-q", "-D", "tmp")
    return repo, old, new


def new_sb(name, with_repo=False):
    sb = f"{ROOT}/sb-{name}"
    shutil.rmtree(sb, ignore_errors=True)
    os.makedirs(f"{sb}/bin")
    os.makedirs(f"{sb}/backups")
    os.makedirs(f"{sb}/stubstate")
    conf = f"{sb}/triangles.conf"
    open(conf, "w").write("rpcuser=stubu\nrpcpassword=stubp\nrpcport=19112\n")
    repo = old = new = None
    if with_repo:
        # Pi layout: the live binary IS the repo's build output
        repo, old, new = build_repo(sb)
        binpath = f"{repo}/target/release/follow_tip"
        os.makedirs(f"{repo}/target/release", exist_ok=True)
    else:
        binpath = f"{sb}/bin/follow_tip"
    shutil.copy(FAKE_OLD, binpath)  # live binary == old bytes
    daemon = subprocess.Popen([binpath], stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, start_new_session=True)
    json.dump({"active": True, "mainpid": daemon.pid, "exe": binpath},
              open(f"{sb}/stubstate/systemctl.json", "w"))
    shutil.copy(FAKE_NEW, f"{sb}/stubstate/newbin")
    rpc = RPCStub(f"{sb}/stubstate/rpc.json")
    json.dump(HEALTHY, open(f"{sb}/stubstate/rpc.json", "w"))
    rpc.start()
    return dict(sb=sb, conf=conf, daemon=daemon, rpc=rpc, repo=repo, old=old, new=new,
                stub=f"{sb}/stubstate", binpath=binpath)


def script_env(S, extra=None):
    env = {"PATH": f"{STUBS}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
           "HOME": "/root", "STUB_STATE": S["stub"],
           "ITEM39_UNIT": "tri7d", "ITEM39_BIN": f'{S["sb"]}/bin/follow_tip',
           "ITEM39_BACKUP": f'{S["sb"]}/backups/follow_tip.bak',
           "ITEM39_RPC": f"http://127.0.0.1:{S['rpc'].port}", "ITEM39_CONF": S["conf"]}
    if S["repo"]:
        env["ITEM39_REPO"] = S["repo"]
    env.update(extra or {})
    return env


def run(script, args, S, timeout=200, extra_env=None):
    t0 = time.time()
    r = subprocess.run(["bash", script] + args, env=script_env(S, extra_env), cwd=S["sb"],
                       capture_output=True, text=True, timeout=timeout)
    return r, time.time() - t0


def stub_state(S):
    return json.load(open(f'{S["stub"]}/systemctl.json'))


def check(label, cond, detail):
    results.append((label, bool(cond), detail))
    log(f"  {'PASS' if cond else 'FAIL'}  {label}  {detail}")


def finish(S):
    pid = stub_state(S)["mainpid"]
    try:
        os.kill(pid, 9)
    except ProcessLookupError:
        pass
    try:
        os.kill(S["daemon"].pid, 9)
    except ProcessLookupError:
        pass
    S["rpc"].stop()


def repo_state(repo):
    head = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    branch = subprocess.run(["git", "-C", repo, "symbolic-ref", "-q", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
    return head, branch


def main():
    setup()
    OLD_SHA, NEW_SHA = sha(FAKE_OLD), sha(FAKE_NEW)

    log("\n=== DNS3 leg (deploy_dns3.sh) ===")

    # D1: success — new binary installed, running inode == new, backup == old
    S = new_sb("D1")
    r, dur = run(DNS3, [f'{S["sb"]}/stubstate/newbin', NEW_SHA, MIN_TIP], S)
    log(f"D1 success: rc={r.returncode} ({dur:.1f}s) {r.stdout.strip().splitlines()[-1] if r.stdout.strip() else ''}")
    st = stub_state(S)
    check("D1 exit=0 (deployed+healthy)", r.returncode == 0, f"rc={r.returncode}")
    check("D1 binary==new", sha(f'{S["sb"]}/bin/follow_tip') == NEW_SHA, "")
    check("D1 backup==old", sha(f'{S["sb"]}/backups/follow_tip.bak') == OLD_SHA, "")
    check("D1 running inode==new", exe_sha(st["mainpid"]) == NEW_SHA, f"pid={st['mainpid']}")
    finish(S)

    # D2: health failure — clean rollback (exit 1), old bytes restored AND running
    S = new_sb("D2")
    json.dump(RPC_ERROR, open(f'{S["sb"]}/stubstate/rpc.json', "w"))
    r, dur = run(DNS3, [f'{S["sb"]}/stubstate/newbin', NEW_SHA, MIN_TIP], S, timeout=200)
    log(f"D2 health-fail->rollback: rc={r.returncode} ({dur:.1f}s)")
    st = stub_state(S)
    check("D2 exit=1 (clean rollback)", r.returncode == 1, f"rc={r.returncode}")
    check("D2 binary restored==old", sha(f'{S["sb"]}/bin/follow_tip') == OLD_SHA, "")
    check("D2 backup intact==old", sha(f'{S["sb"]}/backups/follow_tip.bak') == OLD_SHA, "")
    check("D2 running inode==old", exe_sha(st["mainpid"]) == OLD_SHA, f"pid={st['mainpid']}")
    check("D2 unit active", st["active"] is True, "")
    check("D2 poll bounded <=140s", dur <= 140, f"{dur:.1f}s")
    finish(S)

    # D3: wrong expected sha — abort pre-mutation, no backup written
    S = new_sb("D3")
    r, dur = run(DNS3, [f'{S["sb"]}/stubstate/newbin', "0" * 64, MIN_TIP], S)
    log(f"D3 wrong-sha abort: rc={r.returncode}")
    check("D3 exit=3 (pre-mutation abort)", r.returncode == 3, f"rc={r.returncode}")
    check("D3 binary untouched==old", sha(f'{S["sb"]}/bin/follow_tip') == OLD_SHA, "")
    check("D3 no backup created", not os.path.exists(f'{S["sb"]}/backups/follow_tip.bak'), "")
    finish(S)

    # D5: unit inactive pre-state — abort
    S = new_sb("D5")
    st = json.load(open(f'{S["stub"]}/systemctl.json')); st["active"] = False
    json.dump(st, open(f'{S["stub"]}/systemctl.json', "w"))
    r, dur = run(DNS3, [f'{S["sb"]}/stubstate/newbin', NEW_SHA, MIN_TIP], S)
    log(f"D5 unit-inactive abort: rc={r.returncode}")
    check("D5 exit=3 (unit inactive)", r.returncode == 3, f"rc={r.returncode}")
    check("D5 no backup created", not os.path.exists(f'{S["sb"]}/backups/follow_tip.bak'), "")
    finish(S)

    # D6: live binary path missing (daemon still running its inode) — phase-0 abort
    S = new_sb("D6")
    os.unlink(f'{S["sb"]}/bin/follow_tip')
    r, dur = run(DNS3, [f'{S["sb"]}/stubstate/newbin', NEW_SHA, MIN_TIP], S)
    log(f"D6 missing-binary abort: rc={r.returncode}")
    check("D6 exit=3 (missing live binary)", r.returncode == 3, f"rc={r.returncode}")
    check("D6 daemon still alive", exe_sha(stub_state(S)["mainpid"]) is not None, "")
    check("D6 no backup created", not os.path.exists(f'{S["sb"]}/backups/follow_tip.bak'), "")
    finish(S)

    # D7: rollback restart FAILS -> FATAL exit 2 (deploy restart #1 ok, poll
    #     unhealthy, rollback restart #2 fails)
    S = new_sb("D7")
    json.dump(RPC_ERROR, open(f'{S["sb"]}/stubstate/rpc.json', "w"))
    open(f'{S["stub"]}/restart_fail_at', "w").write("2")
    r, dur = run(DNS3, [f'{S["sb"]}/stubstate/newbin', NEW_SHA, MIN_TIP], S, timeout=200)
    log(f"D7 rollback-restart-fail: rc={r.returncode} ({dur:.1f}s)")
    check("D7 exit=2 (FATAL rollback restart failed)", r.returncode == 2, f"rc={r.returncode}")
    check("D7 binary restored==old", sha(f'{S["sb"]}/bin/follow_tip') == OLD_SHA, "")
    finish(S)

    # D8: rollback restart spawns the WRONG binary -> running-inode proof
    #     catches it -> FATAL exit 2
    S = new_sb("D8")
    json.dump(RPC_ERROR, open(f'{S["sb"]}/stubstate/rpc.json', "w"))
    shutil.copy(FAKE_NEW, f'{S["sb"]}/altbin')  # wrong bytes at another path
    st = json.load(open(f'{S["stub"]}/systemctl.json'))
    st["alt_exe"] = f'{S["sb"]}/altbin'
    json.dump(st, open(f'{S["stub"]}/systemctl.json', "w"))
    open(f'{S["stub"]}/alt_exe_after', "w").write("1")  # deploy restart ok, rollback restart wrong
    r, dur = run(DNS3, [f'{S["sb"]}/stubstate/newbin', NEW_SHA, MIN_TIP], S, timeout=200)
    log(f"D8 rollback-inode-mismatch: rc={r.returncode} ({dur:.1f}s)")
    check("D8 exit=2 (FATAL running inode != restored)", r.returncode == 2, f"rc={r.returncode}")
    check("D8 binary path restored==old", sha(f'{S["sb"]}/bin/follow_tip') == OLD_SHA, "")
    check("D8 running inode==WRONG bytes (mismatch detected)", exe_sha(stub_state(S)["mainpid"]) == NEW_SHA, "")
    finish(S)

    log("\n=== Pi leg (deploy_pi.sh) ===")

    # P1: success — build from pinned commit, new binary running, repo at pinned commit
    S = new_sb("P1", with_repo=True)
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S)
    log(f"P1 success: rc={r.returncode} ({dur:.1f}s)")
    st = stub_state(S)
    repo = S["repo"]
    head = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    branch = subprocess.run(["git", "-C", repo, "symbolic-ref", "-q", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
    check("P1 exit=0 (deployed+healthy)", r.returncode == 0, f"rc={r.returncode}")
    check("P1 binary==new (built)", sha(f"{repo}/target/release/follow_tip") == NEW_SHA, "")
    check("P1 running inode==new", exe_sha(st["mainpid"]) == NEW_SHA, f"pid={st['mainpid']}")
    check("P1 repo HEAD==pinned commit", head == S["new"], head[:12])
    check("P1 repo detached (on pinned)", branch == "", branch or "detached")
    check("P1 backup==old", sha(f'{S["sb"]}/backups/follow_tip.bak') == OLD_SHA, "")
    finish(S)

    # P2: health failure — clean rollback of binary AND repo state (exit 1)
    S = new_sb("P2", with_repo=True)
    json.dump(RPC_ERROR, open(f'{S["sb"]}/stubstate/rpc.json', "w"))
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S, timeout=200)
    log(f"P2 health-fail->rollback: rc={r.returncode} ({dur:.1f}s)")
    st = stub_state(S)
    repo = S["repo"]
    head = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    branch = subprocess.run(["git", "-C", repo, "symbolic-ref", "-q", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
    check("P2 exit=1 (clean rollback)", r.returncode == 1, f"rc={r.returncode}")
    check("P2 binary restored==old", sha(f"{repo}/target/release/follow_tip") == OLD_SHA, "")
    check("P2 repo HEAD==orig", head == S["old"], head[:12])
    check("P2 repo branch restored==master", branch == "master", branch)
    check("P2 running inode==old", exe_sha(st["mainpid"]) == OLD_SHA, f"pid={st['mainpid']}")
    check("P2 unit active", st["active"] is True, "")
    finish(S)

    # P3: build failure — exit 3, binary+repo restored, daemon NEVER restarted
    S = new_sb("P3", with_repo=True)
    open(f'{S["stub"]}/cargo_fail', "w").write("1")
    pre_pid = stub_state(S)["mainpid"]
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S)
    log(f"P3 build-fail: rc={r.returncode} ({dur:.1f}s)")
    st = stub_state(S)
    repo = S["repo"]
    head = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    branch = subprocess.run(["git", "-C", repo, "symbolic-ref", "-q", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()
    check("P3 exit=3 (build failed, proven restore)", r.returncode == 3, f"rc={r.returncode}")
    check("P3 binary restored==old", sha(f"{repo}/target/release/follow_tip") == OLD_SHA, "")
    check("P3 repo HEAD==orig", head == S["old"], head[:12])
    check("P3 repo branch==master", branch == "master", branch)
    check("P3 daemon NEVER restarted (same pid)", st["mainpid"] == pre_pid,
          f"pre={pre_pid} post={st['mainpid']}")
    check("P3 running inode==old", exe_sha(st["mainpid"]) == OLD_SHA, "")
    finish(S)

    # P4: dirty working tree — refuse pre-mutation, nothing touched
    S = new_sb("P4", with_repo=True)
    open(f'{S["repo"]}/src.rs', "a").write("operator WIP\n")
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S)
    log(f"P4 dirty-tree abort: rc={r.returncode}")
    check("P4 exit=3 (dirty tree refused)", r.returncode == 3, f"rc={r.returncode}")
    check("P4 binary untouched==old", sha(f'{S["repo"]}/target/release/follow_tip') == OLD_SHA, "")
    check("P4 no backup created", not os.path.exists(f'{S["sb"]}/backups/follow_tip.bak'), "")
    check("P4 dirty tree left as-is", "operator WIP" in open(f'{S["repo"]}/src.rs').read(), "")
    finish(S)

    # P5: unknown commit — checkout FAILS pre-restart: proven restore, NO restart, exit 3
    S = new_sb("P5", with_repo=True)
    pre_pid = stub_state(S)["mainpid"]
    bogus = "1234567890123456789012345678901234567890"
    r, dur = run(PI, [bogus, OLD_SHA, MIN_TIP], S)
    log(f"P5 unknown-commit: rc={r.returncode} ({dur:.1f}s)")
    st = stub_state(S)
    head, branch = repo_state(S["repo"])
    check("P5 exit=3 (checkout failed, proven restore, no restart)", r.returncode == 3, f"rc={r.returncode}")
    check("P5 binary restored==old", sha(f"{S['repo']}/target/release/follow_tip") == OLD_SHA, "")
    check("P5 repo HEAD==orig", head == S["old"], head[:12])
    check("P5 repo branch==master", branch == "master", branch)
    check("P5 daemon NEVER restarted (same pid)", st["mainpid"] == pre_pid,
          f"pre={pre_pid} post={st['mainpid']}")
    check("P5 running inode==old", exe_sha(st["mainpid"]) == OLD_SHA, f"pid={st['mainpid']}")
    finish(S)

    # P6: rollback restart FAILS -> FATAL exit 2, binary+repo still restored
    #     (health-fail path: deploy restart #1 ok, poll unhealthy, rollback restart #2 fails)
    S = new_sb("P6", with_repo=True)
    json.dump(RPC_ERROR, open(f'{S["sb"]}/stubstate/rpc.json', "w"))
    open(f'{S["stub"]}/restart_fail_at', "w").write("2")
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S, timeout=200)
    log(f"P6 rollback-restart-fail: rc={r.returncode} ({dur:.1f}s)")
    head, branch = repo_state(S["repo"])
    check("P6 exit=2 (FATAL rollback restart failed)", r.returncode == 2, f"rc={r.returncode}")
    check("P6 binary restored==old", sha(f'{S["repo"]}/target/release/follow_tip') == OLD_SHA, "")
    check("P6 repo HEAD==orig", head == S["old"], head[:12])
    finish(S)

    # P10: git fetch failure — pre-restart phase, explicitly guarded: exit 3, NO restart
    S = new_sb("P10", with_repo=True)
    subprocess.run(["git", "-C", S["repo"], "remote", "set-url", "origin",
                    f'{S["sb"]}/nonexistent-remote.git'], check=True, capture_output=True)
    pre_pid = stub_state(S)["mainpid"]
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S)
    log(f"P10 fetch-fail: rc={r.returncode} ({dur:.1f}s)")
    st = stub_state(S)
    head, branch = repo_state(S["repo"])
    check("P10 exit=3 (fetch failed, proven restore, no restart)", r.returncode == 3, f"rc={r.returncode}")
    check("P10 binary restored==old", sha(f'{S["repo"]}/target/release/follow_tip') == OLD_SHA, "")
    check("P10 repo HEAD==orig", head == S["old"], head[:12])
    check("P10 repo branch==master", branch == "master", branch)
    check("P10 daemon NEVER restarted (same pid)", st["mainpid"] == pre_pid,
          f"pre={pre_pid} post={st['mainpid']}")
    check("P10 running inode==old", exe_sha(st["mainpid"]) == OLD_SHA, "")
    finish(S)

    # P11: rollback restart spawns the WRONG binary (ExecStart resolves elsewhere)
    #      -> running-inode proof catches it -> FATAL exit 2
    S = new_sb("P11", with_repo=True)
    json.dump(RPC_ERROR, open(f'{S["sb"]}/stubstate/rpc.json', "w"))
    shutil.copy(FAKE_NEW, f'{S["sb"]}/altbin')  # wrong bytes at another path
    st = json.load(open(f'{S["stub"]}/systemctl.json'))
    st["alt_exe"] = f'{S["sb"]}/altbin'
    json.dump(st, open(f'{S["stub"]}/systemctl.json', "w"))
    open(f'{S["stub"]}/alt_exe_after', "w").write("1")  # deploy restart ok, rollback restart wrong
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S, timeout=200)
    log(f"P11 rollback-inode-mismatch: rc={r.returncode} ({dur:.1f}s)")
    head, branch = repo_state(S["repo"])
    check("P11 exit=2 (FATAL running inode != restored)", r.returncode == 2, f"rc={r.returncode}")
    check("P11 binary path restored==old", sha(f'{S["repo"]}/target/release/follow_tip') == OLD_SHA, "")
    check("P11 repo HEAD==orig", head == S["old"], head[:12])
    check("P11 running inode==WRONG bytes (mismatch detected)", exe_sha(stub_state(S)["mainpid"]) == NEW_SHA, "")
    finish(S)

    # P7: live binary path missing (daemon running its inode) — phase-0 abort, exit 3
    S = new_sb("P7", with_repo=True)
    os.unlink(f'{S["repo"]}/target/release/follow_tip')
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S)
    log(f"P7 missing-binary abort: rc={r.returncode}")
    check("P7 exit=3 (missing live binary)", r.returncode == 3, f"rc={r.returncode}")
    check("P7 daemon still alive", exe_sha(stub_state(S)["mainpid"]) is not None, "")
    check("P7 repo untouched==orig", repo_state(S["repo"])[0] == S["old"], "")
    check("P7 no backup created", not os.path.exists(f'{S["sb"]}/backups/follow_tip.bak'), "")
    finish(S)

    # P8: backup directory cannot be created — phase-0 abort, exit 3
    S = new_sb("P8", with_repo=True)
    open(f'{S["sb"]}/blockfile', "w").write("")  # regular file where a dir is needed
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S,
                 extra_env={"ITEM39_BACKUP": f'{S["sb"]}/blockfile/follow_tip.bak'})
    log(f"P8 backup-dir-uncreatable abort: rc={r.returncode}")
    check("P8 exit=3 (backup dir not creatable)", r.returncode == 3, f"rc={r.returncode}")
    check("P8 binary untouched==old", sha(f'{S["repo"]}/target/release/follow_tip') == OLD_SHA, "")
    check("P8 repo untouched==orig", repo_state(S["repo"])[0] == S["old"], "")
    finish(S)

    # P9: repo is not a git repository — phase-0 abort, exit 3
    S = new_sb("P9", with_repo=True)
    os.makedirs(f'{S["sb"]}/notarepo')
    r, dur = run(PI, [S["new"], OLD_SHA, MIN_TIP], S,
                 extra_env={"ITEM39_REPO": f'{S["sb"]}/notarepo'})
    log(f"P9 repo-not-git abort: rc={r.returncode}")
    check("P9 exit=3 (not a git repo)", r.returncode == 3, f"rc={r.returncode}")
    check("P9 no backup created", not os.path.exists(f'{S["sb"]}/backups/follow_tip.bak'), "")
    finish(S)

    # Write RESULTS.md
    ok = sum(1 for _, p, _ in results if p)
    total = len(results)
    with open(f"{ROOT}/RESULTS.md", "w") as f:
        f.write("# item39-fleet-deploy — deterministic protocol test results\n\n")
        f.write(f"Run {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}. "
                "systemctl/cargo/RPC stubbed; bash/git/install/sha256sum/timeout/python3 real.\n")
        f.write("Harness resolves scripts and stubs relative to its own directory; "
                "all output under ./protocol-test-run/. transcripts.log holds per-scenario stdout.\n\n")
        f.write(f"**{ok}/{total} checks passed.**\n\n")
        f.write("| scenario | check | result | detail |\n|---|---|---|---|\n")
        for label, p, detail in results:
            f.write(f"| {label.split(' ', 1)[0]} | {label.split(' ', 1)[1]} | {'PASS' if p else 'FAIL'} | {detail} |\n")
    with open(f"{ROOT}/transcripts.log", "w") as f:
        f.write("\n".join(log_lines))
    log(f"\nDONE: {ok}/{total} checks passed -> {ROOT}/RESULTS.md")
    return 0 if ok == total else 1


if __name__ == "__main__":
    sys.exit(main())

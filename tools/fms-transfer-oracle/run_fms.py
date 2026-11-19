#!/usr/bin/env python3
"""Run import/export probe scripts on a real FileMaker Server.

The local script harness (tools/fms-script-harness) has libFMEngine's shared
runtime, not fmsased's: Import Records, Export Records, Save Records as Excel
and Export Field Contents return error 3 there. This tool runs them on a lab
FileMaker Server instead, touching nothing but its own probe database, one
schedule and files named omprobe_* in the server's Documents folder:

    run_fms.py upload <probe.fmp12>      upload as OM_Probe_ImportExport.fmp12 and open it
    run_fms.py put <file>...             copy import sources (omprobe_* names) to Documents
    run_fms.py run <script> [param]      run a script as a schedule and wait for it
    run_fms.py fetch <dir>               copy Documents/omprobe_* here, then delete them there
    run_fms.py cleanup                   delete the schedule, close and remove the database,
                                         delete every omprobe_* file

Admin API calls use the admin console account from the project .env
(OM_ENV, default the repository root's .env: FM_TEST_HOST, FM_TEST_USER,
FM_TEST_PASS); the schedule runs as the probe
file's Admin account with the password FM_TESTFILE_PASS (build.sh with
ACCOUNTS_XML gives the file that account); files move over root SSH.
Credentials are never printed or logged. Every call is appended to the
action log (OM_PROBE_LOG, default ./fms-actions.log) without secrets.
"""
import base64
import json
import os
import shlex
import ssl
import subprocess
import sys
import time
import urllib.request
import uuid

DB = "OM_Probe_ImportExport"
SCHEDULE = "OM probe ImportExport"
PREFIX = "omprobe_"
DOCS = "/opt/FileMaker/FileMaker Server/Data/Documents"
ENV = os.environ.get("OM_ENV") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".env")
LOG = os.environ.get("OM_PROBE_LOG", "fms-actions.log")


def env():
    out = {}
    for line in open(ENV, encoding="utf-8"):
        line = line.strip()
        if "=" in line and not line.startswith("#"):
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip().strip('"').strip("'")
    return out


def log(text):
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(time.strftime("%Y-%m-%dT%H:%M:%S ") + text + "\n")


class Admin:
    def __init__(self):
        e = env()
        self.host = e["FM_TEST_HOST"]
        self.base = f"https://{self.host}/fmi/admin/api/v2"
        self.ctx = ssl.create_default_context()
        basic = base64.b64encode(f'{e["FM_TEST_USER"]}:{e["FM_TEST_PASS"]}'.encode()).decode()
        r = self.call("POST", "/user/auth", headers={"Authorization": "Basic " + basic}, body=b"")
        self.token = r["response"]["token"]

    def call(self, method, path, body=None, headers=None, content_type="application/json"):
        h = dict(headers or {})
        if getattr(self, "token", None):
            h["Authorization"] = "Bearer " + self.token
        data = body
        if isinstance(body, (dict, list)):
            data = json.dumps(body).encode()
        if data is not None and content_type:
            h.setdefault("Content-Type", content_type)
        req = urllib.request.Request(self.base + path, data=data, method=method, headers=h)
        try:
            with urllib.request.urlopen(req, context=self.ctx, timeout=600) as resp:
                text = resp.read().decode("utf-8", "replace")
                status = resp.status
        except urllib.error.HTTPError as err:
            text = err.read().decode("utf-8", "replace")
            status = err.code
        shown = path.replace(self.token, "<token>") if getattr(self, "token", None) else path
        log(f"admin {method} {shown} -> {status}")
        out = json.loads(text) if text.strip().startswith("{") else {"raw": text}
        if status >= 300:
            raise SystemExit(f"{method} {path}: HTTP {status}: {json.dumps(out.get('messages', out))}")
        return out

    def close_session(self):
        self.call("DELETE", f"/user/auth/{self.token}")

    def database(self):
        dbs = self.call("GET", "/databases")["response"]["databases"]
        mine = [d for d in dbs if d["filename"] == DB + ".fmp12"]
        return mine[0] if mine else None

    def schedule(self):
        for s in self.call("GET", "/schedules")["response"]["schedules"]:
            if s["name"] == SCHEDULE:
                return s
        return None


def ssh(command, stdin=None):
    log(f"ssh {command}")
    host = env()["FM_TEST_HOST"]
    return subprocess.run(["ssh", f"root@{host}", command], input=stdin, capture_output=True, check=True).stdout


def upload(path):
    a = Admin()
    if a.database():
        raise SystemExit(f"{DB} is already on the server: run cleanup first")
    boundary = uuid.uuid4().hex
    data = open(path, "rb").read()
    parts = []
    for name, value in (("uploadDest", f"Databases/{DB}.fmp12"), ("openOnServer", "true")):
        parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode())
    parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="uploadFile"; filename="{DB}.fmp12"\r\n'
                 'Content-Type: application/octet-stream\r\n\r\n'.encode() + data + b"\r\n")
    parts.append(f"--{boundary}--\r\n".encode())
    a.call("POST", "/databases/upload", body=b"".join(parts),
           content_type=f"multipart/form-data; boundary={boundary}")
    for _ in range(30):
        db = a.database()
        if db and db["status"] in ("NORMAL", "CLOSED"):
            break
        time.sleep(2)
    if db and db["status"] == "CLOSED":
        a.call("PATCH", f"/databases/{db['id']}", body={"status": "OPENED"})
        for _ in range(30):
            time.sleep(2)
            db = a.database()
            if db["status"] != "OPENING":
                break
    print(json.dumps({k: db.get(k) for k in ("id", "filename", "folder", "status")}))
    a.close_session()


def put(paths):
    for p in paths:
        name = os.path.basename(p)
        if not name.startswith(PREFIX):
            raise SystemExit(f"{name}: import sources must be named {PREFIX}*")
        ssh(f"cat > {shlex.quote(DOCS + '/' + name)} && chown fmserver:fmsadmin {shlex.quote(DOCS + '/' + name)}",
            stdin=open(p, "rb").read())


def run(script, param=""):
    a = Admin()
    body = {"name": SCHEDULE, "enabled": True,
            "filemakerScriptType": {"resource": DB, "fmScriptName": script, "fmScriptParam": param,
                                    "fmScriptAccount": "Admin", "fmScriptPassword": env()["FM_TESTFILE_PASS"],
                                    "autoAbort": True, "timeout": 30},
            "onceType": {"startTimeStamp": "2036-01-01T00:00:00"}}
    s = a.schedule()
    if s is None:
        s = a.call("POST", "/schedules/filemakerscript", body=body)["response"]["schedule"]
    else:
        a.call("PATCH", f"/schedules/{s['id']}", body={"filemakerScriptType": body["filemakerScriptType"]})
    before = a.call("GET", f"/schedules/{s['id']}")["response"]["schedule"].get("lastRun")
    a.call("PATCH", f"/schedules/{s['id']}", body={"status": "RUNNING"})
    for _ in range(600):
        time.sleep(2)
        cur = a.call("GET", f"/schedules/{s['id']}")["response"]["schedule"]
        if cur.get("status") == "IDLE" and cur.get("lastRun") != before:
            print(json.dumps({k: cur.get(k) for k in ("lastRun", "lastError", "status")}))
            break
    a.close_session()


def fetch(out):
    os.makedirs(out, exist_ok=True)
    names = ssh(f"cd {shlex.quote(DOCS)} && ls -1A | grep '^{PREFIX}' || true").decode().split()
    if names:
        tar = ssh(f"cd {shlex.quote(DOCS)} && tar -cf - {' '.join(shlex.quote(n) for n in names)}")
        subprocess.run(["tar", "-xf", "-", "-C", out], input=tar, check=True)
        ssh(f"cd {shlex.quote(DOCS)} && rm -rf {' '.join(shlex.quote(n) for n in names)}")
    print("\n".join(names))


def cleanup():
    a = Admin()
    s = a.schedule()
    if s:
        a.call("DELETE", f"/schedules/{s['id']}")
    db = a.database()
    if db:
        if db["status"] != "CLOSED":
            a.call("PATCH", f"/databases/{db['id']}", body={"status": "CLOSED", "messageText": "", "force": True})
            for _ in range(60):
                time.sleep(2)
                if a.database()["status"] == "CLOSED":
                    break
        a.call("DELETE", f"/databases/{db['id']}")
    a.close_session()
    ssh(f"cd {shlex.quote(DOCS)} && rm -rf {PREFIX}*")


def main():
    cmd, args = sys.argv[1], sys.argv[2:]
    {"upload": lambda: upload(args[0]), "put": lambda: put(args), "run": lambda: run(*args),
     "fetch": lambda: fetch(args[0]), "cleanup": cleanup}[cmd]()


if __name__ == "__main__":
    main()

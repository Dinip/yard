"""Probe real Runner launches and App Group publication on a disposable simulator."""

import hashlib
import json
import subprocess
import sys
import time
import uuid
from pathlib import Path


def simctl(*arguments):
    return subprocess.check_output(["xcrun", "simctl", *arguments], text=True).strip()


def wait_for(check):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.1)
    raise AssertionError("Runner did not complete the capture command within 30 seconds")


device = sys.argv[1]
devices = json.loads(simctl("list", "devices", "available", "-j"))["devices"]
selected = next(d for group in devices.values() for d in group if d["udid"] == device)
if selected["state"] != "Booted":
    simctl("boot", device)
simctl("bootstatus", device, "-b")
bundle = "com.dinispimpao.yard.drop"
simctl("install", device, "build/ios/iphonesimulator/Runner.app")


def launch(*arguments):
    simctl("launch", "--terminate-running-process", device, bundle, *arguments)


capture = str(uuid.uuid4())
expiry = int(time.time() * 1000) + 600_000
launch("--yard-drop", "arm", capture, str(expiry))
group = Path(simctl("get_app_container", device, bundle, "group.com.dinispimpao.yard.drop"))
root = group / "Library/Caches/YARD Drop"
record = root / "active-capture.json"
wait_for(lambda: record.exists() and json.loads(record.read_text())["id"] == capture)

batch_id, file_id = str(uuid.uuid4()), str(uuid.uuid4())
source = root / "Incoming" / capture / batch_id
source.mkdir(parents=True)
payload = b"YARD launch handoff probe\n" * 4096
(source / f"{file_id}.bin").write_bytes(payload)
(source / "batch.json").write_text(json.dumps({
    "schemaVersion": 1, "batchId": batch_id, "captureId": capture,
    "createdAt": int(time.time() * 1000),
    "producer": {"appVersion": "probe", "buildNumber": 0, "commit": "probe"},
    "files": [{"id": file_id, "name": "report.zip", "typeIdentifier": "public.zip-archive",
               "mimeType": "application/zip", "size": len(payload), "stagedName": f"{file_id}.bin"}],
}))
(source / "_YARD_READY").touch()
documents = Path(simctl("get_app_container", device, bundle, "data")) / "Documents"
published = documents / "YARD Drop/Inbox" / capture / batch_id

launch()
time.sleep(1)
assert source.exists() and not published.exists(), "An ordinary launch must not publish"
launch("--yard-drop", "publish", str(uuid.uuid4()))
time.sleep(1)
assert source.exists() and not published.exists(), "A mismatched capture must not publish"
launch("--yard-drop", "publish", capture)
wait_for(lambda: (published / "_YARD_READY").exists())
assert hashlib.sha256((published / "report.zip").read_bytes()).digest() == hashlib.sha256(payload).digest()
assert not record.exists(), "Successful publication must clear the arm record"

next_capture = str(uuid.uuid4())
launch("--yard-drop", "arm", next_capture, str(expiry))
wait_for(lambda: record.exists() and json.loads(record.read_text())["id"] == next_capture)
assert not published.exists(), "A new capture must purge previous published files"
print("Passed: cold/warm launch arguments, ordinary/wrong-ID isolation, publication bytes, capture replacement")

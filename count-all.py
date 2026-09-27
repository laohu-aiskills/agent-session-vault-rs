import json, subprocess, sys

out = subprocess.run(
    [r".\target\release\asv.exe", "list", "--limit", "0", "--json"],
    capture_output=True,
).stdout.decode("utf-8")
rows = json.loads(out)
print("全部 limit0 实际返回:", len(rows))

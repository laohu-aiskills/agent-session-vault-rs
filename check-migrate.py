import json, sys, glob

files = glob.glob(sys.argv[1] + "/**/*.jsonl", recursive=True)
assert files, "没找到迁移产物"
j = files[0]
lines = [l for l in open(j, encoding="utf8").read().split("\n") if l.strip()]
ok = all(json.loads(l) is not None for l in lines)
print("lines =", len(lines), "all_json =", ok)
print("marker =", any("asv_migration" in l or "asv-migration" in l for l in lines))
print("title  =", any('"aiTitle"' in l for l in lines))
notes = glob.glob(sys.argv[1] + "/**/*.asv-migration.json", recursive=True)
print("note   =", len(notes) == 1)
sys.exit(0 if (ok and len(notes) == 1) else 1)

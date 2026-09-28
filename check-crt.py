import sys

for f in [r"target\release\asv.exe", r"target\release\asv-gui.exe"]:
    data = open(f, "rb").read()
    has_vcrt = b"VCRUNTIME140" in data
    has_api = b"api-ms-win-crt" in data
    print(f, "| VCRUNTIME140:", has_vcrt, "| api-ms-win-crt:", has_api)

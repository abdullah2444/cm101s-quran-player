# usage: mkcmds.py header PREFIX -> prints "NAME hexcmd" for every #define PREFIX* (compiled with the SDK headers)
import re, subprocess, sys, os
S = "/root/hisdk6/source"
h, p = sys.argv[1], sys.argv[2]
names = re.findall(r"#define\s+(%s\w*)" % p, open(h, encoding="latin-1").read())
NL = ";"
extra = open("/root/hiextra6").read().split()
inc = ["-I" + S + d for d in ["/common/include", "/common/drv/include", "/msp/include", "/msp/drv/include",
                              "/msp/api/include", "/msp/drv/vfmw/vfmw_v4.0"]] + ["-I" + os.path.dirname(h)]
inc += sys.argv[3:]

def build(ns):
    src = ['#include <stdio.h>', '#include "hi_type.h"', '#include "%s"' % os.path.basename(h), "int main(){"]
    src += ['printf("%%s %%08x%s","%s",(unsigned)(%s));' % (NL, n, n) for n in ns]
    src += ["return 0;}"]
    open("/tmp/cmds.c", "w").write("\n".join(src))
    return subprocess.run(["gcc", "-w", "-DCHIP_TYPE_hi3798mv100"] + extra + inc + ["-o", "/tmp/cmds", "/tmp/cmds.c"],
                          capture_output=True, text=True)

# drop names whose definition does not compile (unused macros with missing types)
while names:
    r = build(names)
    if r.returncode == 0:
        break
    bad = set(re.findall(r"\b(%s\w*)\b" % p, r.stderr))
    bad = [n for n in names if n in bad] or names[:1]
    names = [n for n in names if n not in bad]
print(subprocess.run(["/tmp/cmds"], capture_output=True, text=True).stdout.replace(";", chr(10)), end="")

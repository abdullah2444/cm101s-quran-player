#!/bin/sh
# Run on the box; builds only in this directory and installs nothing.
set -eu
cd "$(dirname "$0")"
SDK=${HISI_SDK_SOURCE:-/root/hisdk6/source}
CC=${CC:-gcc}
"$CC" -O2 -fPIC -Wall -Wextra -DCHIP_TYPE_hi3798mv100 \
  -I"$SDK/common/include" -I"$SDK/common/api/include" \
  -I"$SDK/msp/include" -I"$SDK/msp/api/include" -I"$SDK/msp/drv/include" \
  -shared hisi_raw.c -o libhisi_raw_test.so -L/usr/local/lib -lhimini
"$CC" -O2 -Wall -Wextra hisi_raw_test.c -o hisi_raw_test \
  -L. -lhisi_raw_test -Wl,-rpath,'$ORIGIN'
printf '%s\n' 'Built experimental backend and test. Hardware must be idle before running the test.'

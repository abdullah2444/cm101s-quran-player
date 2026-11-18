#!/bin/bash
# build HiSilicon UNF user libs (subset) natively on the box
S=/root/hisdk/source
O=/root/hibuild; mkdir -p $O/obj
INC="-I$S/common/include -I$S/common/api/include -I$S/common/drv/include -I$S/msp/include -I$S/msp/api/include -I$S/msp/drv/include \
 -I$S/msp/api/vo -I$S/msp/api/avplay -I$S/msp/api/vdec/vdec_v1.0 -I$S/msp/api/demux/demux_v2 -I$S/common/api/mem -I$S/common/api/module -I$S/common/api/log \
 -I$S/msp/drv/vfmw/vfmw_v4.0 -I$S/msp/drv/vdec/vdec_v1.0 -I$S/msp/drv/sync -I$S/msp/drv/vpss -I$S/msp/drv/vo $EXTRA_INC"
DEF="-DCHIP_TYPE_hi3798mv100 -DSDK_VERSION=HiSTBLinuxV100R005C00SPC050 -DHI_LOG_SUPPORT=1 -DHI_LOG_LEVEL=4 -DHI_PROC_SUPPORT=0 -DHI_HDMI_SUPPORT_1_4 \
 -D_GNU_SOURCE -D_XOPEN_SOURCE=600 -D_LARGEFILE_SOURCE -D_LARGEFILE64_SOURCE -D_FILE_OFFSET_BITS=64 -DCONFIG_HDMI_STB_SDK -DDVI_SUPPORT -DCA_FRAMEWORK_V100_SUPPORT $EXTRA_DEF"
CF="-O2 -fPIC -mcpu=cortex-a7 -w -fcommon"
fail=0
for f in "$@"; do
  o=$O/obj/$(echo $f | tr / _).o
  if ! gcc $CF $DEF $INC -c $S/$f -o $o 2>$o.err; then echo "FAIL $f"; grep -m3 "error" $o.err; fail=$((fail+1)); fi
done
echo "failures: $fail"

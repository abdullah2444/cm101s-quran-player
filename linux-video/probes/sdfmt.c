#include <stdio.h>
#include <string.h>
#include "hi_common.h"
#include "hi_unf_disp.h"
int main(int c, char **v){
  HI_UNF_ENC_FMT_E f0, f1;
  HI_SYS_Init(); HI_UNF_DISP_Init();
  HI_UNF_DISP_GetFormat(HI_UNF_DISPLAY0, &f0); HI_UNF_DISP_GetFormat(HI_UNF_DISPLAY1, &f1);
  printf("before sd=%d hd=%d (PAL=%d NTSC=%d)\n", f0, f1, HI_UNF_ENC_FMT_PAL, HI_UNF_ENC_FMT_NTSC);
  if (c > 1) printf("set %x\n", HI_UNF_DISP_SetFormat(HI_UNF_DISPLAY0, strcmp(v[1], "pal") ? HI_UNF_ENC_FMT_NTSC : HI_UNF_ENC_FMT_PAL));
  HI_UNF_DISP_GetFormat(HI_UNF_DISPLAY0, &f0); HI_UNF_DISP_GetFormat(HI_UNF_DISPLAY1, &f1);
  printf("after sd=%d hd=%d\n", f0, f1);
  HI_UNF_DISP_DeInit(); HI_SYS_DeInit();
  return 0;
}

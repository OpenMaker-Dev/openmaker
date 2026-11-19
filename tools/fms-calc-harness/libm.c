/* Reads lines "<fn> <hex double>" and prints glibc's result as %a and %.20g. */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(void) {
  char fn[16], hex[64];
  while (scanf("%15s %63s", fn, hex) == 2) {
    double x = strtod(hex, NULL), r = NAN;
    if (!strcmp(fn, "sin")) r = sin(x); else if (!strcmp(fn, "cos")) r = cos(x);
    else if (!strcmp(fn, "tan")) r = tan(x); else if (!strcmp(fn, "atan")) r = atan(x);
    else if (!strcmp(fn, "asin")) r = asin(x); else if (!strcmp(fn, "acos")) r = acos(x);
    printf("%s %s %a %.20g\n", fn, hex, r, r);
  }
  return 0;
}

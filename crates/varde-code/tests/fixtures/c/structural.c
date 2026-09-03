/* Structural fixtures: Function (function_definition, incl. a
   pointer-returning function to exercise declarator unwrapping), Class
   (struct/union/enum/typedef), Variable (file-scope + block-scope), Parameter,
   and Import (#include, both "..." and <...> forms). */

#include <stdio.h>
#include "helper.h"

typedef struct Point {
    int x;
    int y;
} Point;

union Value {
    int i;
    float f;
};

enum Color { RED, GREEN, BLUE };

typedef int Distance;

static int counter = 0;

int add(int a, int b) {
    int result = a + b;
    return result;
}

char *label(const char *prefix) {
    return 0;
}

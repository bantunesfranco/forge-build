#include <math.h>
#include <stdio.h>

#ifndef FEATURES_EXAMPLE
#error "the workspace define FEATURES_EXAMPLE did not reach the bin"
#endif

int main(void) {
    printf("level: %s\n", LEVEL_NAME);
    printf("scaled sqrt: %.1f\n", sqrt(16.0) * LEVEL_SCALE);
#ifdef FEATURES_LEGACY
    printf("legacy: on\n");
#else
    printf("legacy: removed\n");
#endif
#ifdef NDEBUG
    printf("asserts: off\n");
#else
    printf("asserts: on\n");
#endif
    return 0;
}

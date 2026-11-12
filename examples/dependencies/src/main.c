#include "math_utils.h"

#include <stdio.h>

int main(void)
{
    const double value = 25.0;

    printf("sqrt(%.1f) = %.1f\n", value, square_root(value));
    printf("clamp(42, 0, 10) = %.1f\n", clamp(42, 0, 10));

    return 0;
}

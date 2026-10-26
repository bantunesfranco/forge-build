#include "math_utils.h"

#include <math.h>
#include <stdio.h>

double square_root(double value)
{
    return sqrt(value);
}

double clamp(double value, double minimum, double maximum)
{
    if (value < minimum)
        return minimum;

    if (value > maximum)
        return maximum;

    return value;
}

int main(void)
{
    const double value = 25.0;

    printf("sqrt(%.1f) = %.1f\n", value, square_root(value));
    printf("clamp(42, 0, 10) = %.1f\n", clamp(42, 0, 10));

    return 0;
}

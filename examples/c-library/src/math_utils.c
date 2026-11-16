#include "../include/math_utils.h"

#include <math.h>

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

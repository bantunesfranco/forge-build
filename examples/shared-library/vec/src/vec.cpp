#include "vec.hpp"

#include <cmath>

namespace vec {

double length(double x, double y) {
    return std::sqrt(x * x + y * y);
}

}  // namespace vec

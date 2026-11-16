#include "geometry.hpp"

#include "vec.hpp"

namespace geometry {

double area(double width, double height) {
    return width * height;
}

double diagonal(double width, double height) {
    return vec::length(width, height);
}

}  // namespace geometry

#include <iostream>

#include "geometry.hpp"

int main() {
    std::cout << "geometry is " << LIB_KIND << "\n";
    std::cout << "area(3, 4) = " << geometry::area(3, 4) << "\n";
    std::cout << "diagonal(3, 4) = " << geometry::diagonal(3, 4) << "\n";
    return 0;
}

#include "greeting.hpp"

#include <iostream>

int main()
{
    std::cout << make_greeting("Forge") << '\n';

#ifdef NDEBUG
    std::cout << "Release configuration\n";
#else
    std::cout << "Debug configuration\n";
#endif

    return 0;
}

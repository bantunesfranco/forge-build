#include "greeting.hpp"

#include <string>

std::string make_greeting(const std::string& name)
{
#ifdef APP_DEBUG
    return "[debug] Hello, " + name + "!";
#else
    return "Hello, " + name + "!";
#endif
}

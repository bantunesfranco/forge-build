#include "checksum.h"

#include <cstdint>
#include <iomanip>
#include <iostream>
#include <string>

int main()
{
    const std::string message = "Hello from Forge";

    const auto *bytes =
        reinterpret_cast<const unsigned char *>(message.data());

    const std::uint32_t result =
        checksum_bytes(bytes, message.size());

    std::cout << "Message: " << message << '\n';
    std::cout << "Checksum: 0x"
              << std::hex << result << '\n';

    return 0;
}

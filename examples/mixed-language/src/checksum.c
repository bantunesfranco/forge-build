#include "checksum.h"

uint32_t checksum_bytes(const unsigned char *data, size_t length)
{
    uint32_t checksum = 0;

    for (size_t i = 0; i < length; ++i)
        checksum = (checksum * 31u) + data[i];

    return checksum;
}

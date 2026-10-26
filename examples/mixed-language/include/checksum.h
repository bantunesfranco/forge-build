#ifndef CHECKSUM_H
#define CHECKSUM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

uint32_t checksum_bytes(const unsigned char *data, size_t length);

#ifdef __cplusplus
}
#endif

#endif

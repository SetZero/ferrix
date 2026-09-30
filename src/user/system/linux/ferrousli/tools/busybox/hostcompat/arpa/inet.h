/* Byte order only, for fixdep; the host is little-endian x86-64. */
#ifndef HOSTCOMPAT_ARPA_INET_H
#define HOSTCOMPAT_ARPA_INET_H
#include <stdint.h>

#define ntohl(x) __builtin_bswap32((uint32_t)(x))
#define htonl(x) __builtin_bswap32((uint32_t)(x))
#define ntohs(x) __builtin_bswap16((uint16_t)(x))
#define htons(x) __builtin_bswap16((uint16_t)(x))
#endif

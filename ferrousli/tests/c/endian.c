/*
 * POSIX.1-2024's endian conversions through both their optional macro forms
 * and their required function symbols.
 */

#define _POSIX_C_SOURCE 202405L
#include <endian.h>
#include <stdint.h>

#include "check.h"

int main(void)
{
	uint16_t (*to_be16)(uint16_t) = htobe16;
	uint32_t (*to_be32)(uint32_t) = htobe32;
	uint64_t (*to_be64)(uint64_t) = htobe64;
	uint16_t (*to_le16)(uint16_t) = htole16;
	uint32_t (*to_le32)(uint32_t) = htole32;
	uint64_t (*to_le64)(uint64_t) = htole64;
	uint16_t (*from_be16)(uint16_t) = be16toh;
	uint32_t (*from_be32)(uint32_t) = be32toh;
	uint64_t (*from_be64)(uint64_t) = be64toh;
	uint16_t (*from_le16)(uint16_t) = le16toh;
	uint32_t (*from_le32)(uint32_t) = le32toh;
	uint64_t (*from_le64)(uint64_t) = le64toh;
	uint16_t b16 = to_be16(UINT16_C(0x1234));
	uint32_t b32 = to_be32(UINT32_C(0x12345678));
	uint64_t b64 = to_be64(UINT64_C(0x0123456789abcdef));
	uint16_t l16 = to_le16(UINT16_C(0x1234));
	uint32_t l32 = to_le32(UINT32_C(0x12345678));
	uint64_t l64 = to_le64(UINT64_C(0x0123456789abcdef));
	unsigned char *bytes;
	unsigned int evaluated = 0;

	bytes = (unsigned char *)&b16;
	CHECK(bytes[0] == 0x12 && bytes[1] == 0x34);
	bytes = (unsigned char *)&b32;
	CHECK(bytes[0] == 0x12 && bytes[1] == 0x34 &&
	    bytes[2] == 0x56 && bytes[3] == 0x78);
	bytes = (unsigned char *)&b64;
	CHECK(bytes[0] == 0x01 && bytes[1] == 0x23 &&
	    bytes[2] == 0x45 && bytes[3] == 0x67 &&
	    bytes[4] == 0x89 && bytes[5] == 0xab &&
	    bytes[6] == 0xcd && bytes[7] == 0xef);

	bytes = (unsigned char *)&l16;
	CHECK(bytes[0] == 0x34 && bytes[1] == 0x12);
	bytes = (unsigned char *)&l32;
	CHECK(bytes[0] == 0x78 && bytes[1] == 0x56 &&
	    bytes[2] == 0x34 && bytes[3] == 0x12);
	bytes = (unsigned char *)&l64;
	CHECK(bytes[0] == 0xef && bytes[1] == 0xcd &&
	    bytes[2] == 0xab && bytes[3] == 0x89 &&
	    bytes[4] == 0x67 && bytes[5] == 0x45 &&
	    bytes[6] == 0x23 && bytes[7] == 0x01);

	CHECK(from_be16(b16) == UINT16_C(0x1234));
	CHECK(from_be32(b32) == UINT32_C(0x12345678));
	CHECK(from_be64(b64) == UINT64_C(0x0123456789abcdef));
	CHECK(from_le16(l16) == UINT16_C(0x1234));
	CHECK(from_le32(l32) == UINT32_C(0x12345678));
	CHECK(from_le64(l64) == UINT64_C(0x0123456789abcdef));

	CHECK(be16toh(htobe16(UINT16_C(0xabcd))) == UINT16_C(0xabcd));
	CHECK(be32toh(htobe32(UINT32_C(0x89abcdef))) == UINT32_C(0x89abcdef));
	CHECK(be64toh(htobe64(UINT64_C(0xfedcba9876543210))) == UINT64_C(0xfedcba9876543210));
	CHECK(le16toh(htole16(UINT16_C(0xabcd))) == UINT16_C(0xabcd));
	CHECK(le32toh(htole32(UINT32_C(0x89abcdef))) == UINT32_C(0x89abcdef));
	CHECK(le64toh(htole64(UINT64_C(0xfedcba9876543210))) == UINT64_C(0xfedcba9876543210));

	(void)htobe16(evaluated++);
	(void)be16toh(evaluated++);
	(void)htole16(evaluated++);
	(void)le16toh(evaluated++);
	CHECK(evaluated == 4);
	return t_status;
}

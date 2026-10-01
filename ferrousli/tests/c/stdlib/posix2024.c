/* POSIX.1-2024 additions to stdlib.h, plus the DES primitive setkey drives. */

#define _XOPEN_SOURCE 800
#include <crypt.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "check.h"

static void unpack(uint64_t value, char bits[64])
{
	for (int i = 0; i < 64; i++)
		bits[i] = (value >> (63 - i)) & 1;
}

static uint64_t pack(const char bits[64])
{
	uint64_t value = 0;
	for (int i = 0; i < 64; i++)
		value = value << 1 | (bits[i] & 1);
	return value;
}

int main(void)
{
	/* The historical radix-64 form is least-significant group first. */
	CHECK(strcmp(l64a(0), "") == 0);
	CHECK(strcmp(l64a(1), "/") == 0);
	CHECK(strcmp(l64a(64), "./") == 0);
	CHECK(a64l("./") == 64);
	CHECK(a64l(l64a(0x12345678L)) == 0x12345678L);
	CHECK(a64l(l64a(-1)) == -1);

	char options[] = "ro,size=512,unknown=x,empty=";
	char *at = options;
	char *value = (char *)1;
	char *const tokens[] = { "ro", "size", "empty", 0 };
	CHECK(getsubopt(&at, tokens, &value) == 0 && value == 0);
	CHECK(getsubopt(&at, tokens, &value) == 1 && strcmp(value, "512") == 0);
	CHECK(getsubopt(&at, tokens, &value) == -1 && strcmp(value, "unknown=x") == 0);
	CHECK(getsubopt(&at, tokens, &value) == 2 && strcmp(value, "") == 0);
	CHECK(*at == 0);

	CHECK(setenv("SAFE_VALUE", "visible", 1) == 0);
	CHECK(strcmp(secure_getenv("SAFE_VALUE"), "visible") == 0);
	CHECK(secure_getenv("NOT_PRESENT") == 0);

	/* FIPS 46-3 example: both directions through the POSIX bit-array API. */
	char key[64], block[64];
	unpack(UINT64_C(0x133457799BBCDFF1), key);
	unpack(UINT64_C(0x0123456789ABCDEF), block);
	setkey(key);
	encrypt(block, 0);
	CHECK(pack(block) == UINT64_C(0x85E813540F0AB405));
	encrypt(block, 1);
	CHECK(pack(block) == UINT64_C(0x0123456789ABCDEF));

	/* Traditional and extended DES password hashes remain usable. */
	CHECK(strcmp(crypt("password", "ab"), "abJnggxhB/yWI") == 0);
	CHECK(strcmp(crypt("test", "_J9..CCCC"), "_J9..CCCCZBIc.TMGpK.") == 0);
	return t_status;
}

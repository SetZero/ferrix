/*
 * setkey and encrypt: DES itself, through the bit-array interface POSIX gives
 * it rather than the password hash crypt() puts on top.
 *
 * The interface is peculiar and worth saying out loud: a key and a block are
 * each 64 *bytes*, one per bit, most significant first, and the low bit of
 * each byte is the bit. encrypt() works in place, and its second argument
 * says the direction -- zero encrypts, nonzero decrypts.
 *
 * The vector is FIPS 46-3's own worked example, so a wrong answer here is
 * wrong against the standard and not against another library. Encrypting and
 * then decrypting must also come back to the plaintext, which catches a key
 * schedule that is reversed in only one direction.
 */

#define _XOPEN_SOURCE 800
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "check.h"

/* A machine value as the 64 bytes these two take, most significant first. */
static void unpack(uint64_t value, char bits[64])
{
	for (int i = 0; i < 64; i++)
		bits[i] = (value >> (63 - i)) & 1;
}

/* And back, so the answer can be compared as a number. */
static uint64_t pack(const char bits[64])
{
	uint64_t value = 0;
	for (int i = 0; i < 64; i++)
		value = value << 1 | (bits[i] & 1);
	return value;
}

int main(void)
{
	char key[64], block[64];

	/* FIPS 46-3's example, both directions. */
	unpack(UINT64_C(0x133457799BBCDFF1), key);
	unpack(UINT64_C(0x0123456789ABCDEF), block);
	setkey(key);
	encrypt(block, 0);
	CHECK(pack(block) == UINT64_C(0x85E813540F0AB405));
	encrypt(block, 1);
	CHECK(pack(block) == UINT64_C(0x0123456789ABCDEF));

	/* Only the low bit of each byte is the bit: the same key and block with
	 * the other seven bits set must give the same answer. */
	for (int i = 0; i < 64; i++)
		key[i] |= 0xfe;
	unpack(UINT64_C(0x0123456789ABCDEF), block);
	for (int i = 0; i < 64; i++)
		block[i] |= 0xfe;
	setkey(key);
	encrypt(block, 0);
	CHECK(pack(block) == UINT64_C(0x85E813540F0AB405));

	/* A key of all zeros is still a key, and the block it makes is not the
	 * block it was given. */
	unpack(0, key);
	unpack(UINT64_C(0x0123456789ABCDEF), block);
	setkey(key);
	encrypt(block, 0);
	CHECK(pack(block) != UINT64_C(0x0123456789ABCDEF));
	encrypt(block, 1);
	CHECK(pack(block) == UINT64_C(0x0123456789ABCDEF));

	return t_status;
}

// hash9.c — bit-exact port of the legacy Hash9 block-hash from
// triangles_v5/src/hashblock.h. Chain of 13 512-bit digests, result trimmed
// to the low 256 bits (first 32 bytes of the sphlib digest, matching
// uint512::trim256() on this codebase's little-endian base_blob layout).
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include "sph_blake.h"
#include "sph_bmw.h"
#include "sph_groestl.h"
#include "sph_skein.h"
#include "sph_jh.h"
#include "sph_keccak.h"
#include "sph_luffa.h"
#include "sph_cubehash.h"
#include "sph_shavite.h"
#include "sph_simd.h"
#include "sph_echo.h"
#include "sph_hamsi.h"
#include "sph_fugue.h"

void hash9(const uint8_t *in, size_t inlen, uint8_t out32[32])
{
    uint8_t hash[17][64];

    { sph_blake512_context c; sph_blake512_init(&c); sph_blake512(&c, in, inlen); sph_blake512_close(&c, hash[0]); }
    { sph_bmw512_context c; sph_bmw512_init(&c); sph_bmw512(&c, hash[0], 64); sph_bmw512_close(&c, hash[1]); }
    { sph_groestl512_context c; sph_groestl512_init(&c); sph_groestl512(&c, hash[1], 64); sph_groestl512_close(&c, hash[2]); }
    { sph_skein512_context c; sph_skein512_init(&c); sph_skein512(&c, hash[2], 64); sph_skein512_close(&c, hash[3]); }
    { sph_jh512_context c; sph_jh512_init(&c); sph_jh512(&c, hash[3], 64); sph_jh512_close(&c, hash[4]); }
    { sph_keccak512_context c; sph_keccak512_init(&c); sph_keccak512(&c, hash[4], 64); sph_keccak512_close(&c, hash[5]); }
    { sph_luffa512_context c; sph_luffa512_init(&c); sph_luffa512(&c, hash[5], 64); sph_luffa512_close(&c, hash[6]); }
    { sph_cubehash512_context c; sph_cubehash512_init(&c); sph_cubehash512(&c, hash[6], 64); sph_cubehash512_close(&c, hash[7]); }
    { sph_shavite512_context c; sph_shavite512_init(&c); sph_shavite512(&c, hash[7], 64); sph_shavite512_close(&c, hash[8]); }
    { sph_simd512_context c; sph_simd512_init(&c); sph_simd512(&c, hash[8], 64); sph_simd512_close(&c, hash[9]); }
    { sph_echo512_context c; sph_echo512_init(&c); sph_echo512(&c, hash[9], 64); sph_echo512_close(&c, hash[10]); }
    { sph_hamsi512_context c; sph_hamsi512_init(&c); sph_hamsi512(&c, hash[10], 64); sph_hamsi512_close(&c, hash[11]); }
    { sph_fugue512_context c; sph_fugue512_init(&c); sph_fugue512(&c, hash[11], 64); sph_fugue512_close(&c, hash[12]); }

    memcpy(out32, hash[12], 32);
}

// Oracle harness: links the fork's REAL hdwallet.cpp + secp256k1 and prints
// derivation outputs so tri7's Rust port can be pinned to them byte-for-byte.
#include "hdwallet.h"
#include <cstdio>
#include <string>
#include <vector>

static void hex(const unsigned char* p, size_t n) {
    for (size_t i = 0; i < n; i++) printf("%02x", p[i]);
}

int main() {
    // 1) BIP39 seed, 12-word Trezor vector, passphrase TREZOR (their pinned KAT)
    {
        unsigned char seed[64];
        hd::MnemonicToSeed("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about", "TREZOR", seed);
        printf("S12_TREZOR "); hex(seed, 64); printf("\n");
    }
    // 2) BIP39 seed, 24-word official vector (all-zero 256-bit entropy), no passphrase
    {
        unsigned char seed[64];
        hd::MnemonicToSeed("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art", "", seed);
        printf("S24_NONE "); hex(seed, 64); printf("\n");
    }
    // 3) Master + chain for BIP32 vector 1 seed (their pinned master + m/0H)
    {
        unsigned char seed[16];
        for (int i = 0; i < 16; i++) seed[i] = (unsigned char)i;
        hd::ExtKey m, c;
        hd::MasterFromSeed(seed, 16, m);
        printf("V1_MASTER "); hex(m.key, 32); printf(" "); hex(m.chaincode, 32); printf("\n");
        hd::CKDpriv(m, 0u | hd::HARDENED, c);
        printf("V1_M0H "); hex(c.key, 32); printf(" "); hex(c.chaincode, 32); printf("\n");
        // full vector-1 chain: m/0'/1/2'/2/1000000000
        hd::ExtKey a, b, d, e, f;
        hd::CKDpriv(m, 0u | hd::HARDENED, a);
        hd::CKDpriv(a, 1u, b);
        hd::CKDpriv(b, 2u | hd::HARDENED, d);
        hd::CKDpriv(d, 2u, e);
        hd::CKDpriv(e, 1000000000u, f);
        printf("V1_M0H_1_2H_2_1000000000 "); hex(f.key, 32); printf(" "); hex(f.chaincode, 32); printf("\n");
    }
    // 4) DeriveTriangles KATs: m/44'/2222'/0'/0/i for the 12-word phrase, no passphrase
    {
        const char* mn = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        for (uint32_t i = 0; i < 8; i++) {
            unsigned char priv[32];
            hd::DeriveTriangles(mn, "", 0, 0, i, priv);
            printf("TRI12_%u ", i); hex(priv, 32); printf("\n");
        }
        unsigned char priv[32];
        hd::DeriveTriangles(mn, "TREZOR", 0, 0, 0, priv);
        printf("TRI12_PASS_TREZOR_0 "); hex(priv, 32); printf("\n");
    }
    // 5) Same for the 24-word official vector
    {
        const char* mn = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
        for (uint32_t i = 0; i < 8; i++) {
            unsigned char priv[32];
            hd::DeriveTriangles(mn, "", 0, 0, i, priv);
            printf("TRI24_%u ", i); hex(priv, 32); printf("\n");
        }
        unsigned char priv2[32]; hd::DeriveTriangles(mn, "tri7", 0, 0, 3, priv2);
        printf("TRI24_PASS_TRI7_3 "); hex(priv2, 32); printf("\n");
    }
    // 6) Whitespace variant: what the legacy code does with newlines in the phrase
    {
        unsigned char seed[64], priv[32];
        hd::MnemonicToSeed("abandon\tabandon abandon\nabandon abandon abandon abandon abandon abandon abandon abandon abandon about", "", seed);
        printf("S12_NEWLINES "); hex(seed, 64); printf("\n");
        hd::DeriveTriangles("abandon\tabandon abandon\nabandon abandon abandon abandon abandon abandon abandon abandon abandon about", "", 0, 0, 0, priv);
        printf("TRI12_NEWLINES_0 "); hex(priv, 32); printf("\n");
    }
    return 0;
}

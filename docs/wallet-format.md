# Triangles Legacy Wallet File Format — Rust Reimplementation Spec

Target: tri7 Rust wallet reader. Source of truth: `/root/triangles_v5/src` v6.2.6.7
(`walletdb.h`, `walletdb.cpp`, `walletdb-sqlite.h`, `walletmigrate.cpp`, `walletdb-recover.h`,
`db.h`, `key.cpp`, `base58.h`, `wallet.h`), verified against the live wallet
`/root/.triangles/wallet.dat` (read-only inspection, 63,023 rows).

## 1. Two on-disk formats, one record format

### 1.1 Which file you have

| First 16 bytes | Format |
|---|---|
| `"SQLite format 3\0"` | SQLite wallet (current, post-migration) — read per §3 |
| Anything else (BDB magic `0x061561` at offset 12) | Legacy BerkeleyDB wallet.dat — read per §2 |

`IsSQLiteFile()` in `walletmigrate.cpp:18` is exactly the 16-byte magic check.
The live `/root/.triangles/wallet.dat` is already SQLite:
`PRAGMA application_id = 0x54526977 ("TRIw")`, `user_version = 1`,
schema `CREATE TABLE main (key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL);`.

### 1.2 BerkeleyDB layout (legacy `wallet.dat`)

From `db.h` / `walletmigrate.cpp` / `walletdb-recover.h`:

- A BDB environment lives in the **data directory** (`DbEnv::open(dir, DB_CREATE|DB_INIT_MPOOL|DB_PRIVATE)`),
  not inside the file. The wallet is a single BTREE **sub-database named `main`**
  inside the file `wallet.dat` (`db.open(nullptr, "wallet.dat", "main", DB_BTREE, ...)`).
- Every record is a raw key/value pair of **exactly the same serialized bytes**
  as §4. No BDB-side wrapping of the payloads.
- `MaybeMigrateBerkeleyWalletToSQLite` (`walletmigrate.cpp:57`) copies every
  record byte-for-byte (`WriteKey(key, val, true)`) into the SQLite `main`
  table, verifies row count, and renames the original to `wallet.dat.bdb.bak`.
  **Therefore a Rust reader that reads the SQLite table has already read all
  legacy content**; you only need a BDB reader if you must parse
  `wallet.dat.bdb.bak` files directly.
- If you do: use a pure-Rust BDB BTREE reader or shell out to `db_dump`;
  open the `main` subdatabase, iterate all key/value pairs, decode with §4.
- `walletdb-recover.h` (`BerkeleyRecoverWallet`, `BerkeleyZapWalletTx`) are
  BDB salvage tools only — no equivalent logic is needed in a reader.

## 2. SQLite wallet container

File = ordinary SQLite 3 database. Header (first 100 bytes) is standard; observed:

```
offset 16: page size      0x1000 (4096)
offset 21..23: reserved etc
offset 24: file change counter
offset 68: application id   54 52 69 77  ("TRIw", walletdb-sqlite.h:95)
offset 60: user_version     1           (SQLITE_WALLET_SCHEMA_VERSION)
```

Reader validation: check the 16-byte SQLite magic; optionally verify
`PRAGMA application_id == 0x54526977` and `PRAGMA user_version == 1`
(refuse foreign SQLite files, matching `SQLiteDatabase::Open`).

Schema (one table, everything else is rows):

```sql
CREATE TABLE main (key BLOB PRIMARY KEY NOT NULL, value BLOB NOT NULL);
```

- No WAL used at rest for reading; open with `mode=ro` (`immutable=1` only if
  the daemon is stopped — prefer plain `mode=ro`).
- Live wallet observed row prefixes (hex of first bytes of `key`):
  `02tx…` (~62,400 rows), `03key…` (248), `07keymeta…` (248), `04pool…` (101),
  `04name…` (36), plus one each of `07version`, `09bestblock`, `0adefaultkey`,
  `0aminversion`, `0corderposnext`, `07hdchain`, `0ahdmnemonic`, `07setting…`.

## 3. Record serialization primitives (shared by both backends)

Keys and values are the `CDataStream` (SER_DISK, CLIENT_VERSION = 0x005BDDFF =
6014975, i.e. 6.2.6.7) serializations. Byte order: **little-endian** throughout.

| C++ type | Wire format |
|---|---|
| `std::string`, `std::vector<unsigned char>` | CompactSize length + raw bytes |
| `int32/uint32`, `int64/uint64` | LE |
| `int64_t` as a *map key suffix* (e.g. `pool`, `acentry`) | 8-byte LE |
| `uint256`/`uint160` | fixed 32/20 bytes, internal order = the same order used for tx hashes (raw memcpy of the hex-reversed hash — store raw, compare raw) |
| `CPubKey` | CompactSize len + bytes (33 compressed `02/03…` or 65 uncompressed `04…`) |
| `enum nVersion` prefix | Only in *values* that serialize `nVersion`; records written with `Write()` embed no extra version field unless the C++ class does |

CompactSize: `< 0xFD` → 1 byte; `0xFD` → uint16 LE; `0xFE` → uint32 LE;
`0xFF` → uint64 LE. (No SPECIAL/[de]serialization flags in play.)

**Key blob layout** = CompactSize(type-string-length) + type string + optional suffix:

| Record | Key blob | Value blob |
|---|---|---|
| `version` | `07 "version"` | int32 LE = wallet file version (observed `FF DD 5B 00` = 6014975). Refuse/flag if > CLIENT_VERSION. |
| `bestblock` | `09 "bestblock"` | `CBlockLocator`: int32 nVersion LE (`FF DD 5B 00` observed) + CompactSize count + count × 32-byte block hashes (tip first). **The first hash (index 0) is the wallet sync tip; later entries walk back toward genesis.** |
| `key` | `03 "key"` + CompactSize(pubkey len) + pubkey bytes (33 or 65) | `CPrivKey` = CompactSize(len) + DER ECPrivateKey (see §5). CompactSize is 1 byte (`0xD6`=214 compressed) or 3 bytes (`FD 17 01`=279 uncompressed). |
| `wkey` (legacy, none in live wallet) | `04 "wkey"` + CompactSize + pubkey | `CWalletKey`: int32 nVersion + CPrivKey(same as `key` value) + int64 nTimeCreated + int64 nTimeExpires + string strComment (`wallet.h:769`). |
| `ckey` (encrypted; none live) | `04 "ckey"` + CompactSize + pubkey | CompactSize + AES-256-CBC ciphertext (encrypted 32-byte secret; needs `mkey`). |
| `mkey` | `04 "mkey"` + uint32 LE id | `CMasterKey` (version + vector params + vector salt + vector crypted key + nDerivationMethod + nDerivationIterations + vector other). Only needed to flag *encrypted* wallets. |
| `keymeta` | `07 "keymeta"` + CompactSize + pubkey | int32 nVersion (observed `01 00 00 00`) + int64 nCreateTime LE. |
| `name` | `04 "name"` + CompactSize + address string (34 bytes base58, ASCII `T…`) | string (account label; empty ⇒ single `00` byte, observed). |
| `pool` | `04 "pool"` + int64 LE index | `CKeyPool`: int32 nVersion + int64 nTime + CompactSize + pubkey (observed `… 1F 1A 32 6A 00 00 00 00 21 02 55 CC …`). tri7 WRITES these rows since the 2026-11-07 `keypoolrefill` verb: version constant `0x004DB174` byte-matched to the live rows, pubkey always compressed (46-byte values), pool index = the HD derivation index. |
| `defaultkey` | `0A "defaultkey"` | CompactSize + pubkey (33 or 65 bytes). |
| `minversion` | `0A "minversion"` | int32 LE (observed 60000). |
| `orderposnext` | `0C "orderposnext"` | int64 LE (observed `95 71 01 00 …`). |
| `tx` | `02 "tx"` + 32-byte txid | `CWalletTx` (large, ~313–600 B). **A key-only reader skips these entirely.** |
| `hdchain` | `07 "hdchain"` | int64 LE chain index (observed `79 00 …` = 121). |
| `hdmnemonic` | `0A "hdmnemonic"` | string (BIP39 words). Present in live wallet. |
| `hdcmnemonic` / `hdpassphrase` / `hdcpassphrase` | as written | string, or pair(uint256 IV, CompactSize+ciphertext). |
| `setting\0…` / `acentry` / `account` | legacy compat | can be skipped by a key reader. |

Hex key prefix cheat-sheet: `0274 78`=`tx`, `036B 6579`=`key`,
`046E 616D 65`=`name`, `0470 6F6F 6C`=`pool`, `0477 6B65 79`=`wkey`,
`0768 6463 6861 696E`=`hdchain`, `076B 6579 6D65 7461`=`keymeta`,
`0776 6572 7369 6F6E`=`version`, `0962 6573 7462 6C6F 636B`=`bestblock`,
`0A64 6566 6175 6C74 6B65 79`=`defaultkey`, `0A68 646D 6E65 6D6F 6E69 63`=`hdmnemonic`.

## 4. Private key decoding (`key` records)

`CPrivKey` is **NOT WIF and NOT raw 32 bytes** on disk. It is
`CompactSize(n) + DER ECPrivateKey` (`walletdb.cpp:367` `ssValue >> pkey`,
`key.cpp:173 CKey::SetPrivKey`).

DER structure (secp256k1, produced by `ECDSA_privkey_export_der_secp256k1`):

```
SEQUENCE (n bytes)                 -- n = 214 (compressed pubkey) or 279 (uncompressed)
  INTEGER 1
  OCTET STRING (32)  <— the secret scalar
  [context tag] OCTET STRING / BIT STRING  <— the public key (explicit params)
```

Two robust extraction strategies for Rust:

1. **Minimal DER walk**: parse SEQUENCE header (handle `81 xx` / `82 xx xx`
   lengths), then `02 01 01`, then expect `04 20` and take the next 32 bytes.
2. **Byte-scan fallback** (works for both sizes):
   find the pattern `02 01 01 04 20`; the 32 bytes after it are the secret.

Derive the compressed flag the way C++ does: `fCompressed = (der.len() == 214)`
(`key.cpp:186`). Address = `base58check(0x41 || RIPEMD160(SHA256(pubkey)))`,
where pubkey is the 33-byte compressed form from the record key (or derived
from the secret when compressed).

Verify after import, exactly like `ReadKeyValue` (`walletdb.cpp:368-380`):
`pubkey_from_secret(secret) == record-key pubkey`, else treat record as corrupt.

### WIF / address version bytes (`base58.h:275-440`)

| Purpose | Version byte | Base58 lead char |
|---|---|---|
| P2PK/P2SH pubkey address (mainnet) | `PUBKEY_ADDRESS = 65 (0x41)` | `T` (34-char addresses) |
| Script address (mainnet) | `SCRIPT_ADDRESS = 28 (0x1C)` | P2SH addresses start `C` (e.g. `CP…`; verified: base58check(0x1C‖RIPEMD160) yields `C`-prefix). P2PK addresses (version 65) start `T` — all live wallet `name` records are `T`. |
| Testnet pubkey / script | 111 / 196 | |
| **WIF private key** | `128 + PUBKEY_ADDRESS = 193 (0xC1)` + optional `0x01` suffix when compressed | |

WIF encode = `base58check(0xC1 || secret32 || (0x01 if compressed))`. This is
only for export/display; the on-disk `key` value is DER as above.

## 5. Minimal record set for a Rust reader

To reproduce a spendable wallet (or reseed from HD) a reader needs:

1. `version` — sanity/bail-if-too-new.
2. `bestblock` — tip locator; take the FIRST hash (index 0) as the wallet scan start.
3. Every `key` record (and `wkey` for very old wallets) — the actual funds.
4. `defaultkey` — the wallet's default receive key.
5. `pool` + `keymeta` — needed to reserve/report keypool and birth times.
6. `name` — address labels.
7. `minversion`, `orderposnext`, `hdchain`, `hdmnemonic`/`hdpassphrase`,
   `setting*` — optional; read if present, ignore if absent.
8. `tx` — skip in a key-only reader (62k+ rows, biggest cost).
9. `ckey`/`mkey`/`hdcmnemonic` — if present the wallet is **encrypted**; a
   reader without crypto support must report that and stop.

Recommended read: `SELECT key, value FROM main` and dispatch on the decoded
type string; `WHERE hex(substr(key,2,3))`-style filtering also works for
key-only pulls (`substr` on BLOBs returns BLOBs — compare via `hex()`).

## 6. Rust dependency notes

```toml
[dependencies]
rusqlite = { version = "0.32", features = ["bundled"] } # self-contained SQLite; open with OpenFlags::SQLITE_OPEN_READ_ONLY
secp256k1 = { version = "0.29", features = ["recovery"] } # scalar verify + pubkey derivation (tframes: ECDSA_privkey_export/import equivalents)
sha2 = "0.10"        # SHA-256 (address + base58check)
ripemd = "0.1"       # RIPEMD-160 (KeyID)
base58 = "0.2"       # or bitcoin-private base58check impl: encode(0xC1||sec||0x01), decode/validate addresses
hex = "0.4"
```

- **Do not** add a BerkeleyDB dependency unless parsing `.bdb.bak` files
  directly is required; SQLite migration preserved records verbatim.
- `rusqlite::Connection::open_with_flags(path, READ_ONLY | NO_MUTEX)` is the
  correct read path; never open the live wallet read-write while the daemon
  (`trianglesd`) runs.
- The wallet is unencrypted iff no `mkey` rows exist (the live wallet: 0 mkey,
  0 ckey — plain DER keys).

## 7. Verified-against-live quick facts

- 63,023 rows total; 248 `key` records (all 33-byte compressed pubkeys,
  215-byte values = `0xD6` + 214-byte DER), 248 `keymeta`, 101 `pool`,
  36 `name`, ~62,400 `tx`, 0 `wkey`/`ckey`/`mkey`.
- `version` = 6014975 (6.2.6.7); `minversion` = 60000; `hdchain` = 121;
  `hdmnemonic` present (plaintext BIP39).
- All mainnet addresses begin `T` (PUBKEY_ADDRESS 65).

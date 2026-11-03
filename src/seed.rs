//! HD seed-phrase support (BIP39 mnemonic + BIP32 derivation) for the tri7
//! wallet — a byte-exact port of the legacy client's `hd::` namespace
//! (triangles_v5 src/hdwallet.cpp + src/bip39_english.h), so mnemonics are
//! fully INTERCHANGEABLE: a phrase from a triangles_v5 qt/daemon wallet (or
//! the TRIdock web wallet it matches) derives IDENTICAL keys/addresses here,
//! and vice versa.
//!
//! # Provenance and verification
//! Every constant and algorithm below was ported from the legacy source and
//! pinned against an oracle built from THEIR ACTUAL CODE (hdwallet.cpp
//! compiled against libsecp256k1 + OpenSSL on 2026-11-03):
//!   - BIP39 seeds match their hd_wallet_tests.cpp pinned hex
//!     ("abandon…about" + passphrase "TREZOR" → c55257c3…).
//!   - BIP32 master/m0H match their pinned test vector 1 values
//!     (e8f32e72…, edb2e14f…).
//!   - The TRI path m/44'/2222'/0'/0/i was pinned to hex printed by their
//!     compiled DeriveTriangles for i = 0..8 (tests below), and cross-checked
//!     against an independent from-spec Python reference (hashlib PBKDF2 +
//!     pure-python secp256k1) — all three agree byte-for-byte.
//!   - The embedded wordlist is byte-identical to their bip39_english.h AND
//!     the canonical bitcoin/bips bip-0039/english.txt (verified equal).
//!
//! # Scheme (matches legacy exactly)
//!   - BIP39: canonical. 24-word default (256-bit entropy, 8-bit checksum);
//!     validation accepts 12/15/18/21/24 words like their CheckMnemonic.
//!     Seed = PBKDF2-HMAC-SHA512(mnemonic, "mnemonic"+passphrase, 2048, 64).
//!   - BIP32: canonical master (HMAC-SHA512 key "Bitcoin seed"), canonical
//!     CKDpriv (hardened 0x00‖ser256(k), non-hardened serP(P), tweak-add).
//!   - Path: m/44'/2222'/0'/0/index — coin type 2222 (TRI's derivation
//!     namespace, matching triWallet.js), account 0 and change 0 FIXED (the
//!     legacy wallet only ever derives the 0/0/i branch; index is the
//!     wallet.dat `hdchain` counter).
//!   - Keys are compressed, addresses are standard TRI P2PKH (version 65).
//!
//! # Deliberate divergence (documented, safer side)
//! The legacy MnemonicToSeed feeds the RAW supplied string into PBKDF2 while
//! CheckMnemonic splits on whitespace runs — so a phrase typed with newlines
//! or double spaces derives DIFFERENT keys in the legacy wallet than the
//! canonical spelling of the same words (and from every other BIP39 wallet).
//! tri7 canonicalizes whitespace (single-space join) on the RESTORE path
//! (GUI paste / mnemonic argument of the wallet-level restore), which for
//! every canonical phrase — everything hdnew generates and everything stored
//! in an `hdmnemonic` row — produces byte-identical keys to the legacy
//! wallet. Derivation from a wallet's STORED mnemonic row stays byte-exact
//! (no normalization) so continuing an existing legacy seed wallet matches
//! the qt's DeriveHDKey whatever string it holds. (Their raw-string behavior
//! is pinned by the `raw_string_quirk_matches_legacy` test.)

use sha2::{Digest, Sha512};
use zeroize::Zeroize;

/// BIP32 hardened-offset flag.
pub const HARDENED: u32 = 0x8000_0000;
/// TRI coin type (SLIP44 is unregistered for Triangles; 2222 is the
/// de-facto namespace shared with the TRIdock web wallet — documented and
/// deterministic so any implementation can reproduce it).
pub const TRI_COIN_TYPE: u32 = 2222;
/// Human-readable derivation path (account 0 / change 0 fixed, like legacy).
pub const DERIVATION_PATH: &str = "m/44'/2222'/0'/0/i";

/// A BIP32 extended private key (private scalar + chain code) — port of
/// hd::ExtKey.
#[derive(Clone, Debug)]
pub struct ExtKey {
    pub key: [u8; 32],
    pub chaincode: [u8; 32],
}

// ===========================================================================
// HMAC-SHA512 (RFC 2104 over sha2::Sha512; block size 128).
// Hand-rolled like the rest of tri7's ported crypto (smsg.rs precedent); the
// BIP39/BIP32 known-answer tests below exercise every path end-to-end, and
// the RFC 4231 vectors pin the primitive directly.
// ===========================================================================
const SHA512_BLOCK: usize = 128;

pub struct HmacSha512 {
    inner: Sha512,
    outer: Sha512,
}

impl HmacSha512 {
    pub fn new(key: &[u8]) -> Self {
        let mut k = [0u8; SHA512_BLOCK];
        if key.len() > SHA512_BLOCK {
            let h = Sha512::digest(key);
            k[..64].copy_from_slice(&h);
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut ipad = [0x36u8; SHA512_BLOCK];
        let mut opad = [0x5cu8; SHA512_BLOCK];
        for i in 0..SHA512_BLOCK {
            ipad[i] ^= k[i];
            opad[i] ^= k[i];
        }
        let mut inner = Sha512::new();
        inner.update(&ipad);
        let mut outer = Sha512::new();
        outer.update(&opad);
        Self { inner, outer }
    }

    pub fn compute(&self, data: &[u8]) -> [u8; 64] {
        let mut i = self.inner.clone();
        i.update(data);
        let ih = i.finalize();
        let mut o = self.outer.clone();
        o.update(&ih);
        o.finalize().into()
    }
}

/// PBKDF2-HMAC-SHA512 (RFC 2898) — canonical, dkLen inferred from `out`.
pub fn pbkdf2_hmac_sha512(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    let mac = HmacSha512::new(password);
    for (block_index, chunk) in out.chunks_mut(64).enumerate() {
        let idx = (block_index as u32 + 1).to_be_bytes();
        let mut first = Vec::with_capacity(salt.len() + 4);
        first.extend_from_slice(salt);
        first.extend_from_slice(&idx);
        let mut u = mac.compute(&first);
        let mut f = u;
        for _ in 1..iterations {
            u = mac.compute(&u);
            for (fb, ub) in f.iter_mut().zip(u.iter()) {
                *fb ^= *ub;
            }
        }
        let n = chunk.len().min(64);
        chunk[..n].copy_from_slice(&f[..n]);
    }
}

// ===========================================================================
// BIP39 — port of hd::GenerateMnemonic / CheckMnemonic / MnemonicToSeed
// ===========================================================================

/// Lexicographic binary search of the wordlist — port of hd::WordIndex.
pub fn word_index(w: &str) -> Option<usize> {
    let mut lo = 0usize;
    let mut hi = 2047usize;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        match w.cmp(WORDLIST[mid]) {
            std::cmp::Ordering::Equal => return Some(mid),
            std::cmp::Ordering::Less => {
                if mid == 0 {
                    break;
                }
                hi = mid - 1;
            }
            std::cmp::Ordering::Greater => lo = mid + 1,
        }
    }
    None
}

/// Whitespace-run split — port of hd::SplitWords (space/tab/CR/LF; runs
/// collapse, leading/trailing ignored).
pub fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in s.split(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r') {
        if !tok.is_empty() {
            out.push(tok.to_string());
        }
    }
    out
}

/// Canonical mnemonic form: words joined by single spaces. Used on the
/// restore path (see the module-level divergence note).
pub fn normalize_words(s: &str) -> Option<String> {
    let w = split_words(s);
    if w.is_empty() {
        return None;
    }
    Some(w.join(" "))
}

/// Build a mnemonic from fixed entropy (16 or 32 bytes = 12 or 24 words) —
/// the deterministic core of hd::GenerateMnemonic: checksum is the first
/// ENT/32 bits of SHA256(entropy), and the 11-bit groups index the wordlist
/// MSB-first.
pub fn mnemonic_from_entropy(entropy: &[u8]) -> Result<String, String> {
    let strength_bits = entropy.len() * 8;
    if strength_bits != 128 && strength_bits != 256 {
        return Err(format!(
            "entropy must be 16 or 32 bytes (128/256 bits), got {}",
            entropy.len()
        ));
    }
    let cs_bits = strength_bits / 32;
    let hash = sha256_32(entropy);
    // Bit buffer = entropy || first checksum byte (legacy pushes exactly one
    // byte: 8 checksum bits cover both supported strengths).
    let mut bits = entropy.to_vec();
    bits.push(hash[0]);
    let total_bits = strength_bits + cs_bits;
    let mut out = String::new();
    for i in 0..(total_bits / 11) {
        let mut idx = 0usize;
        for b in 0..11 {
            let bitpos = i * 11 + b;
            let byte = bitpos / 8;
            let off = 7 - (bitpos % 8);
            idx = (idx << 1) | ((bits[byte] >> off) & 1) as usize;
        }
        if i > 0 {
            out.push(' ');
        }
        out.push_str(WORDLIST[idx]);
    }
    Ok(out)
}

fn sha256_32(data: &[u8]) -> [u8; 32] {
    let mut h = sha2::Sha256::new();
    h.update(data);
    h.finalize().into()
}

/// Generate a fresh 24-word mnemonic from the OS CSPRNG (getrandom →
/// BCryptGenRandom on Windows) — tri7's CSPRNG-ONLY policy; entropy failures
/// abort loudly.
pub fn generate_mnemonic_24() -> Result<String, String> {
    let mut ent = [0u8; 32];
    getrandom::getrandom(&mut ent).map_err(|e| format!("CSPRNG failure: {}", e))?;
    let m = mnemonic_from_entropy(&ent);
    ent.zeroize();
    m
}

/// Validate word membership + checksum — port of hd::CheckMnemonic (accepts
/// 12/15/18/21/24 words; whitespace-run input OK; case-sensitive lowercase).
pub fn check_mnemonic(mnemonic: &str) -> bool {
    let w = split_words(mnemonic);
    let nw = w.len();
    if nw != 12 && nw != 15 && nw != 18 && nw != 21 && nw != 24 {
        return false;
    }
    let total_bits = nw * 11;
    let cs_bits = total_bits / 33;
    let ent_bits = total_bits - cs_bits;
    let ent_bytes = ent_bits / 8;

    // Unpack the 11-bit indices into the bit buffer (MSB-first).
    let mut buf = vec![0u8; (total_bits + 7) / 8];
    for (i, word) in w.iter().enumerate() {
        let Some(idx) = word_index(word) else {
            return false;
        };
        for b in 0..11 {
            let bit = (idx >> (10 - b)) & 1;
            let bitpos = i * 11 + b;
            let byte = bitpos / 8;
            let off = 7 - (bitpos % 8);
            if bit == 1 {
                buf[byte] |= 1 << off;
            }
        }
    }
    let hash = sha256_32(&buf[..ent_bytes]);
    for b in 0..cs_bits {
        let bitpos = ent_bits + b;
        let byte = bitpos / 8;
        let off = 7 - (bitpos % 8);
        let got = (buf[byte] >> off) & 1;
        let want = (hash[b / 8] >> (7 - (b % 8))) & 1;
        if got != want {
            return false;
        }
    }
    true
}

/// BIP39 seed — PBKDF2-HMAC-SHA512(mnemonic, "mnemonic"+passphrase, 2048).
/// The mnemonic is used BYTE-EXACT as supplied (legacy MnemonicToSeed
/// behavior); canonicalize first if you are restoring user-typed input (see
/// the module divergence note).
pub fn mnemonic_to_seed(mnemonic: &str, passphrase: &str) -> [u8; 64] {
    let mut salt = String::with_capacity(8 + passphrase.len());
    salt.push_str("mnemonic");
    salt.push_str(passphrase);
    let mut seed = [0u8; 64];
    pbkdf2_hmac_sha512(mnemonic.as_bytes(), salt.as_bytes(), 2048, &mut seed);
    seed
}

// ===========================================================================
// BIP32 — port of hd::MasterFromSeed / CKDpriv / DerivePath / DeriveTriangles
// ===========================================================================

/// Master extended key from a BIP39 seed — HMAC-SHA512 keyed "Bitcoin seed".
pub fn master_from_seed(seed: &[u8]) -> Result<ExtKey, String> {
    let mac = HmacSha512::new(b"Bitcoin seed");
    let i = mac.compute(seed);
    let mut out = ExtKey {
        key: [0u8; 32],
        chaincode: [0u8; 32],
    };
    out.key.copy_from_slice(&i[..32]);
    out.chaincode.copy_from_slice(&i[32..]);
    // Reject an all-zero/invalid master scalar (I_L ∈ [0, n) required —
    // probability ~2^-127; mirrors secp256k1_ec_seckey_verify).
    let _sk = secp256k1::SecretKey::from_slice(&out.key)
        .map_err(|_| "BIP32: master key invalid (I_L out of range)".to_string())?;
    Ok(out)
}

/// CKDpriv — canonical BIP32 child private-key derivation (hardened data =
/// 0x00‖ser256(k_par), normal data = serP(P_par); I = HMAC-SHA512(c_par, ·);
/// k_i = (I_L + k_par) mod n with invalid/zero results rejected, exactly like
/// secp256k1_ec_seckey_tweak_add).
pub fn ckd_priv(parent: &ExtKey, index: u32) -> Result<ExtKey, String> {
    let mut data = [0u8; 37];
    let secp = secp256k1::Secp256k1::signing_only();
    if index & HARDENED != 0 {
        data[0] = 0x00;
        data[1..33].copy_from_slice(&parent.key);
    } else {
        let sk = secp256k1::SecretKey::from_slice(&parent.key)
            .map_err(|_| "BIP32: parent key invalid".to_string())?;
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        data[..33].copy_from_slice(&pk.serialize());
    }
    data[33..37].copy_from_slice(&index.to_be_bytes());

    let mac = HmacSha512::new(&parent.chaincode);
    let i = mac.compute(&data[..37]);

    let sk = secp256k1::SecretKey::from_slice(&parent.key)
        .map_err(|_| "BIP32: parent key invalid".to_string())?;
    let tweak = secp256k1::Scalar::from_be_bytes(i[..32].try_into().unwrap())
        .map_err(|_| "BIP32: I_L out of range (≥ n)".to_string())?;
    let child_sk = sk
        .add_tweak(&tweak)
        .map_err(|_| "BIP32: child key invalid (I_L ≥ n or zero)".to_string())?;
    Ok(ExtKey {
        key: child_sk.secret_bytes(),
        chaincode: i[32..].try_into().unwrap(),
    })
}

/// Walk a path of child indices from the master key.
pub fn derive_path(master: &ExtKey, path: &[u32]) -> Result<ExtKey, String> {
    let mut cur = master.clone();
    for &i in path {
        cur = ckd_priv(&cur, i)?;
    }
    Ok(cur)
}

/// Derive the 32-byte private scalar for m/44'/2222'/account'/change/index —
/// byte-exact port of hd::DeriveTriangles. The mnemonic is used byte-exact.
pub fn derive_triangles(
    mnemonic: &str,
    passphrase: &str,
    account: u32,
    change: u32,
    index: u32,
) -> Result<[u8; 32], String> {
    let seed = mnemonic_to_seed(mnemonic, passphrase);
    let master = master_from_seed(&seed)?;
    let path = [
        44 | HARDENED,
        TRI_COIN_TYPE | HARDENED,
        account | HARDENED,
        change,
        index,
    ];
    Ok(derive_path(&master, &path)?.key)
}

/// Secret-string wrapper that zeroes its buffer on drop (the legacy
/// CleanseWalletString analogue; tri7 has no OpenSSL cleanse).
pub struct SecretString(pub String);

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::ops::Deref for SecretString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

/// The canonical BIP39 English wordlist — byte-identical to the legacy
/// client's bip39_english.h and to bitcoin/bips bip-0039/english.txt
/// (verified equal, 2048 words, lexicographically sorted).
pub const WORDLIST: [&str; 2048] = [
    "abandon", "ability", "able", "about",
    "above", "absent", "absorb", "abstract",
    "absurd", "abuse", "access", "accident",
    "account", "accuse", "achieve", "acid",
    "acoustic", "acquire", "across", "act",
    "action", "actor", "actress", "actual",
    "adapt", "add", "addict", "address",
    "adjust", "admit", "adult", "advance",
    "advice", "aerobic", "affair", "afford",
    "afraid", "again", "age", "agent",
    "agree", "ahead", "aim", "air",
    "airport", "aisle", "alarm", "album",
    "alcohol", "alert", "alien", "all",
    "alley", "allow", "almost", "alone",
    "alpha", "already", "also", "alter",
    "always", "amateur", "amazing", "among",
    "amount", "amused", "analyst", "anchor",
    "ancient", "anger", "angle", "angry",
    "animal", "ankle", "announce", "annual",
    "another", "answer", "antenna", "antique",
    "anxiety", "any", "apart", "apology",
    "appear", "apple", "approve", "april",
    "arch", "arctic", "area", "arena",
    "argue", "arm", "armed", "armor",
    "army", "around", "arrange", "arrest",
    "arrive", "arrow", "art", "artefact",
    "artist", "artwork", "ask", "aspect",
    "assault", "asset", "assist", "assume",
    "asthma", "athlete", "atom", "attack",
    "attend", "attitude", "attract", "auction",
    "audit", "august", "aunt", "author",
    "auto", "autumn", "average", "avocado",
    "avoid", "awake", "aware", "away",
    "awesome", "awful", "awkward", "axis",
    "baby", "bachelor", "bacon", "badge",
    "bag", "balance", "balcony", "ball",
    "bamboo", "banana", "banner", "bar",
    "barely", "bargain", "barrel", "base",
    "basic", "basket", "battle", "beach",
    "bean", "beauty", "because", "become",
    "beef", "before", "begin", "behave",
    "behind", "believe", "below", "belt",
    "bench", "benefit", "best", "betray",
    "better", "between", "beyond", "bicycle",
    "bid", "bike", "bind", "biology",
    "bird", "birth", "bitter", "black",
    "blade", "blame", "blanket", "blast",
    "bleak", "bless", "blind", "blood",
    "blossom", "blouse", "blue", "blur",
    "blush", "board", "boat", "body",
    "boil", "bomb", "bone", "bonus",
    "book", "boost", "border", "boring",
    "borrow", "boss", "bottom", "bounce",
    "box", "boy", "bracket", "brain",
    "brand", "brass", "brave", "bread",
    "breeze", "brick", "bridge", "brief",
    "bright", "bring", "brisk", "broccoli",
    "broken", "bronze", "broom", "brother",
    "brown", "brush", "bubble", "buddy",
    "budget", "buffalo", "build", "bulb",
    "bulk", "bullet", "bundle", "bunker",
    "burden", "burger", "burst", "bus",
    "business", "busy", "butter", "buyer",
    "buzz", "cabbage", "cabin", "cable",
    "cactus", "cage", "cake", "call",
    "calm", "camera", "camp", "can",
    "canal", "cancel", "candy", "cannon",
    "canoe", "canvas", "canyon", "capable",
    "capital", "captain", "car", "carbon",
    "card", "cargo", "carpet", "carry",
    "cart", "case", "cash", "casino",
    "castle", "casual", "cat", "catalog",
    "catch", "category", "cattle", "caught",
    "cause", "caution", "cave", "ceiling",
    "celery", "cement", "census", "century",
    "cereal", "certain", "chair", "chalk",
    "champion", "change", "chaos", "chapter",
    "charge", "chase", "chat", "cheap",
    "check", "cheese", "chef", "cherry",
    "chest", "chicken", "chief", "child",
    "chimney", "choice", "choose", "chronic",
    "chuckle", "chunk", "churn", "cigar",
    "cinnamon", "circle", "citizen", "city",
    "civil", "claim", "clap", "clarify",
    "claw", "clay", "clean", "clerk",
    "clever", "click", "client", "cliff",
    "climb", "clinic", "clip", "clock",
    "clog", "close", "cloth", "cloud",
    "clown", "club", "clump", "cluster",
    "clutch", "coach", "coast", "coconut",
    "code", "coffee", "coil", "coin",
    "collect", "color", "column", "combine",
    "come", "comfort", "comic", "common",
    "company", "concert", "conduct", "confirm",
    "congress", "connect", "consider", "control",
    "convince", "cook", "cool", "copper",
    "copy", "coral", "core", "corn",
    "correct", "cost", "cotton", "couch",
    "country", "couple", "course", "cousin",
    "cover", "coyote", "crack", "cradle",
    "craft", "cram", "crane", "crash",
    "crater", "crawl", "crazy", "cream",
    "credit", "creek", "crew", "cricket",
    "crime", "crisp", "critic", "crop",
    "cross", "crouch", "crowd", "crucial",
    "cruel", "cruise", "crumble", "crunch",
    "crush", "cry", "crystal", "cube",
    "culture", "cup", "cupboard", "curious",
    "current", "curtain", "curve", "cushion",
    "custom", "cute", "cycle", "dad",
    "damage", "damp", "dance", "danger",
    "daring", "dash", "daughter", "dawn",
    "day", "deal", "debate", "debris",
    "decade", "december", "decide", "decline",
    "decorate", "decrease", "deer", "defense",
    "define", "defy", "degree", "delay",
    "deliver", "demand", "demise", "denial",
    "dentist", "deny", "depart", "depend",
    "deposit", "depth", "deputy", "derive",
    "describe", "desert", "design", "desk",
    "despair", "destroy", "detail", "detect",
    "develop", "device", "devote", "diagram",
    "dial", "diamond", "diary", "dice",
    "diesel", "diet", "differ", "digital",
    "dignity", "dilemma", "dinner", "dinosaur",
    "direct", "dirt", "disagree", "discover",
    "disease", "dish", "dismiss", "disorder",
    "display", "distance", "divert", "divide",
    "divorce", "dizzy", "doctor", "document",
    "dog", "doll", "dolphin", "domain",
    "donate", "donkey", "donor", "door",
    "dose", "double", "dove", "draft",
    "dragon", "drama", "drastic", "draw",
    "dream", "dress", "drift", "drill",
    "drink", "drip", "drive", "drop",
    "drum", "dry", "duck", "dumb",
    "dune", "during", "dust", "dutch",
    "duty", "dwarf", "dynamic", "eager",
    "eagle", "early", "earn", "earth",
    "easily", "east", "easy", "echo",
    "ecology", "economy", "edge", "edit",
    "educate", "effort", "egg", "eight",
    "either", "elbow", "elder", "electric",
    "elegant", "element", "elephant", "elevator",
    "elite", "else", "embark", "embody",
    "embrace", "emerge", "emotion", "employ",
    "empower", "empty", "enable", "enact",
    "end", "endless", "endorse", "enemy",
    "energy", "enforce", "engage", "engine",
    "enhance", "enjoy", "enlist", "enough",
    "enrich", "enroll", "ensure", "enter",
    "entire", "entry", "envelope", "episode",
    "equal", "equip", "era", "erase",
    "erode", "erosion", "error", "erupt",
    "escape", "essay", "essence", "estate",
    "eternal", "ethics", "evidence", "evil",
    "evoke", "evolve", "exact", "example",
    "excess", "exchange", "excite", "exclude",
    "excuse", "execute", "exercise", "exhaust",
    "exhibit", "exile", "exist", "exit",
    "exotic", "expand", "expect", "expire",
    "explain", "expose", "express", "extend",
    "extra", "eye", "eyebrow", "fabric",
    "face", "faculty", "fade", "faint",
    "faith", "fall", "false", "fame",
    "family", "famous", "fan", "fancy",
    "fantasy", "farm", "fashion", "fat",
    "fatal", "father", "fatigue", "fault",
    "favorite", "feature", "february", "federal",
    "fee", "feed", "feel", "female",
    "fence", "festival", "fetch", "fever",
    "few", "fiber", "fiction", "field",
    "figure", "file", "film", "filter",
    "final", "find", "fine", "finger",
    "finish", "fire", "firm", "first",
    "fiscal", "fish", "fit", "fitness",
    "fix", "flag", "flame", "flash",
    "flat", "flavor", "flee", "flight",
    "flip", "float", "flock", "floor",
    "flower", "fluid", "flush", "fly",
    "foam", "focus", "fog", "foil",
    "fold", "follow", "food", "foot",
    "force", "forest", "forget", "fork",
    "fortune", "forum", "forward", "fossil",
    "foster", "found", "fox", "fragile",
    "frame", "frequent", "fresh", "friend",
    "fringe", "frog", "front", "frost",
    "frown", "frozen", "fruit", "fuel",
    "fun", "funny", "furnace", "fury",
    "future", "gadget", "gain", "galaxy",
    "gallery", "game", "gap", "garage",
    "garbage", "garden", "garlic", "garment",
    "gas", "gasp", "gate", "gather",
    "gauge", "gaze", "general", "genius",
    "genre", "gentle", "genuine", "gesture",
    "ghost", "giant", "gift", "giggle",
    "ginger", "giraffe", "girl", "give",
    "glad", "glance", "glare", "glass",
    "glide", "glimpse", "globe", "gloom",
    "glory", "glove", "glow", "glue",
    "goat", "goddess", "gold", "good",
    "goose", "gorilla", "gospel", "gossip",
    "govern", "gown", "grab", "grace",
    "grain", "grant", "grape", "grass",
    "gravity", "great", "green", "grid",
    "grief", "grit", "grocery", "group",
    "grow", "grunt", "guard", "guess",
    "guide", "guilt", "guitar", "gun",
    "gym", "habit", "hair", "half",
    "hammer", "hamster", "hand", "happy",
    "harbor", "hard", "harsh", "harvest",
    "hat", "have", "hawk", "hazard",
    "head", "health", "heart", "heavy",
    "hedgehog", "height", "hello", "helmet",
    "help", "hen", "hero", "hidden",
    "high", "hill", "hint", "hip",
    "hire", "history", "hobby", "hockey",
    "hold", "hole", "holiday", "hollow",
    "home", "honey", "hood", "hope",
    "horn", "horror", "horse", "hospital",
    "host", "hotel", "hour", "hover",
    "hub", "huge", "human", "humble",
    "humor", "hundred", "hungry", "hunt",
    "hurdle", "hurry", "hurt", "husband",
    "hybrid", "ice", "icon", "idea",
    "identify", "idle", "ignore", "ill",
    "illegal", "illness", "image", "imitate",
    "immense", "immune", "impact", "impose",
    "improve", "impulse", "inch", "include",
    "income", "increase", "index", "indicate",
    "indoor", "industry", "infant", "inflict",
    "inform", "inhale", "inherit", "initial",
    "inject", "injury", "inmate", "inner",
    "innocent", "input", "inquiry", "insane",
    "insect", "inside", "inspire", "install",
    "intact", "interest", "into", "invest",
    "invite", "involve", "iron", "island",
    "isolate", "issue", "item", "ivory",
    "jacket", "jaguar", "jar", "jazz",
    "jealous", "jeans", "jelly", "jewel",
    "job", "join", "joke", "journey",
    "joy", "judge", "juice", "jump",
    "jungle", "junior", "junk", "just",
    "kangaroo", "keen", "keep", "ketchup",
    "key", "kick", "kid", "kidney",
    "kind", "kingdom", "kiss", "kit",
    "kitchen", "kite", "kitten", "kiwi",
    "knee", "knife", "knock", "know",
    "lab", "label", "labor", "ladder",
    "lady", "lake", "lamp", "language",
    "laptop", "large", "later", "latin",
    "laugh", "laundry", "lava", "law",
    "lawn", "lawsuit", "layer", "lazy",
    "leader", "leaf", "learn", "leave",
    "lecture", "left", "leg", "legal",
    "legend", "leisure", "lemon", "lend",
    "length", "lens", "leopard", "lesson",
    "letter", "level", "liar", "liberty",
    "library", "license", "life", "lift",
    "light", "like", "limb", "limit",
    "link", "lion", "liquid", "list",
    "little", "live", "lizard", "load",
    "loan", "lobster", "local", "lock",
    "logic", "lonely", "long", "loop",
    "lottery", "loud", "lounge", "love",
    "loyal", "lucky", "luggage", "lumber",
    "lunar", "lunch", "luxury", "lyrics",
    "machine", "mad", "magic", "magnet",
    "maid", "mail", "main", "major",
    "make", "mammal", "man", "manage",
    "mandate", "mango", "mansion", "manual",
    "maple", "marble", "march", "margin",
    "marine", "market", "marriage", "mask",
    "mass", "master", "match", "material",
    "math", "matrix", "matter", "maximum",
    "maze", "meadow", "mean", "measure",
    "meat", "mechanic", "medal", "media",
    "melody", "melt", "member", "memory",
    "mention", "menu", "mercy", "merge",
    "merit", "merry", "mesh", "message",
    "metal", "method", "middle", "midnight",
    "milk", "million", "mimic", "mind",
    "minimum", "minor", "minute", "miracle",
    "mirror", "misery", "miss", "mistake",
    "mix", "mixed", "mixture", "mobile",
    "model", "modify", "mom", "moment",
    "monitor", "monkey", "monster", "month",
    "moon", "moral", "more", "morning",
    "mosquito", "mother", "motion", "motor",
    "mountain", "mouse", "move", "movie",
    "much", "muffin", "mule", "multiply",
    "muscle", "museum", "mushroom", "music",
    "must", "mutual", "myself", "mystery",
    "myth", "naive", "name", "napkin",
    "narrow", "nasty", "nation", "nature",
    "near", "neck", "need", "negative",
    "neglect", "neither", "nephew", "nerve",
    "nest", "net", "network", "neutral",
    "never", "news", "next", "nice",
    "night", "noble", "noise", "nominee",
    "noodle", "normal", "north", "nose",
    "notable", "note", "nothing", "notice",
    "novel", "now", "nuclear", "number",
    "nurse", "nut", "oak", "obey",
    "object", "oblige", "obscure", "observe",
    "obtain", "obvious", "occur", "ocean",
    "october", "odor", "off", "offer",
    "office", "often", "oil", "okay",
    "old", "olive", "olympic", "omit",
    "once", "one", "onion", "online",
    "only", "open", "opera", "opinion",
    "oppose", "option", "orange", "orbit",
    "orchard", "order", "ordinary", "organ",
    "orient", "original", "orphan", "ostrich",
    "other", "outdoor", "outer", "output",
    "outside", "oval", "oven", "over",
    "own", "owner", "oxygen", "oyster",
    "ozone", "pact", "paddle", "page",
    "pair", "palace", "palm", "panda",
    "panel", "panic", "panther", "paper",
    "parade", "parent", "park", "parrot",
    "party", "pass", "patch", "path",
    "patient", "patrol", "pattern", "pause",
    "pave", "payment", "peace", "peanut",
    "pear", "peasant", "pelican", "pen",
    "penalty", "pencil", "people", "pepper",
    "perfect", "permit", "person", "pet",
    "phone", "photo", "phrase", "physical",
    "piano", "picnic", "picture", "piece",
    "pig", "pigeon", "pill", "pilot",
    "pink", "pioneer", "pipe", "pistol",
    "pitch", "pizza", "place", "planet",
    "plastic", "plate", "play", "please",
    "pledge", "pluck", "plug", "plunge",
    "poem", "poet", "point", "polar",
    "pole", "police", "pond", "pony",
    "pool", "popular", "portion", "position",
    "possible", "post", "potato", "pottery",
    "poverty", "powder", "power", "practice",
    "praise", "predict", "prefer", "prepare",
    "present", "pretty", "prevent", "price",
    "pride", "primary", "print", "priority",
    "prison", "private", "prize", "problem",
    "process", "produce", "profit", "program",
    "project", "promote", "proof", "property",
    "prosper", "protect", "proud", "provide",
    "public", "pudding", "pull", "pulp",
    "pulse", "pumpkin", "punch", "pupil",
    "puppy", "purchase", "purity", "purpose",
    "purse", "push", "put", "puzzle",
    "pyramid", "quality", "quantum", "quarter",
    "question", "quick", "quit", "quiz",
    "quote", "rabbit", "raccoon", "race",
    "rack", "radar", "radio", "rail",
    "rain", "raise", "rally", "ramp",
    "ranch", "random", "range", "rapid",
    "rare", "rate", "rather", "raven",
    "raw", "razor", "ready", "real",
    "reason", "rebel", "rebuild", "recall",
    "receive", "recipe", "record", "recycle",
    "reduce", "reflect", "reform", "refuse",
    "region", "regret", "regular", "reject",
    "relax", "release", "relief", "rely",
    "remain", "remember", "remind", "remove",
    "render", "renew", "rent", "reopen",
    "repair", "repeat", "replace", "report",
    "require", "rescue", "resemble", "resist",
    "resource", "response", "result", "retire",
    "retreat", "return", "reunion", "reveal",
    "review", "reward", "rhythm", "rib",
    "ribbon", "rice", "rich", "ride",
    "ridge", "rifle", "right", "rigid",
    "ring", "riot", "ripple", "risk",
    "ritual", "rival", "river", "road",
    "roast", "robot", "robust", "rocket",
    "romance", "roof", "rookie", "room",
    "rose", "rotate", "rough", "round",
    "route", "royal", "rubber", "rude",
    "rug", "rule", "run", "runway",
    "rural", "sad", "saddle", "sadness",
    "safe", "sail", "salad", "salmon",
    "salon", "salt", "salute", "same",
    "sample", "sand", "satisfy", "satoshi",
    "sauce", "sausage", "save", "say",
    "scale", "scan", "scare", "scatter",
    "scene", "scheme", "school", "science",
    "scissors", "scorpion", "scout", "scrap",
    "screen", "script", "scrub", "sea",
    "search", "season", "seat", "second",
    "secret", "section", "security", "seed",
    "seek", "segment", "select", "sell",
    "seminar", "senior", "sense", "sentence",
    "series", "service", "session", "settle",
    "setup", "seven", "shadow", "shaft",
    "shallow", "share", "shed", "shell",
    "sheriff", "shield", "shift", "shine",
    "ship", "shiver", "shock", "shoe",
    "shoot", "shop", "short", "shoulder",
    "shove", "shrimp", "shrug", "shuffle",
    "shy", "sibling", "sick", "side",
    "siege", "sight", "sign", "silent",
    "silk", "silly", "silver", "similar",
    "simple", "since", "sing", "siren",
    "sister", "situate", "six", "size",
    "skate", "sketch", "ski", "skill",
    "skin", "skirt", "skull", "slab",
    "slam", "sleep", "slender", "slice",
    "slide", "slight", "slim", "slogan",
    "slot", "slow", "slush", "small",
    "smart", "smile", "smoke", "smooth",
    "snack", "snake", "snap", "sniff",
    "snow", "soap", "soccer", "social",
    "sock", "soda", "soft", "solar",
    "soldier", "solid", "solution", "solve",
    "someone", "song", "soon", "sorry",
    "sort", "soul", "sound", "soup",
    "source", "south", "space", "spare",
    "spatial", "spawn", "speak", "special",
    "speed", "spell", "spend", "sphere",
    "spice", "spider", "spike", "spin",
    "spirit", "split", "spoil", "sponsor",
    "spoon", "sport", "spot", "spray",
    "spread", "spring", "spy", "square",
    "squeeze", "squirrel", "stable", "stadium",
    "staff", "stage", "stairs", "stamp",
    "stand", "start", "state", "stay",
    "steak", "steel", "stem", "step",
    "stereo", "stick", "still", "sting",
    "stock", "stomach", "stone", "stool",
    "story", "stove", "strategy", "street",
    "strike", "strong", "struggle", "student",
    "stuff", "stumble", "style", "subject",
    "submit", "subway", "success", "such",
    "sudden", "suffer", "sugar", "suggest",
    "suit", "summer", "sun", "sunny",
    "sunset", "super", "supply", "supreme",
    "sure", "surface", "surge", "surprise",
    "surround", "survey", "suspect", "sustain",
    "swallow", "swamp", "swap", "swarm",
    "swear", "sweet", "swift", "swim",
    "swing", "switch", "sword", "symbol",
    "symptom", "syrup", "system", "table",
    "tackle", "tag", "tail", "talent",
    "talk", "tank", "tape", "target",
    "task", "taste", "tattoo", "taxi",
    "teach", "team", "tell", "ten",
    "tenant", "tennis", "tent", "term",
    "test", "text", "thank", "that",
    "theme", "then", "theory", "there",
    "they", "thing", "this", "thought",
    "three", "thrive", "throw", "thumb",
    "thunder", "ticket", "tide", "tiger",
    "tilt", "timber", "time", "tiny",
    "tip", "tired", "tissue", "title",
    "toast", "tobacco", "today", "toddler",
    "toe", "together", "toilet", "token",
    "tomato", "tomorrow", "tone", "tongue",
    "tonight", "tool", "tooth", "top",
    "topic", "topple", "torch", "tornado",
    "tortoise", "toss", "total", "tourist",
    "toward", "tower", "town", "toy",
    "track", "trade", "traffic", "tragic",
    "train", "transfer", "trap", "trash",
    "travel", "tray", "treat", "tree",
    "trend", "trial", "tribe", "trick",
    "trigger", "trim", "trip", "trophy",
    "trouble", "truck", "true", "truly",
    "trumpet", "trust", "truth", "try",
    "tube", "tuition", "tumble", "tuna",
    "tunnel", "turkey", "turn", "turtle",
    "twelve", "twenty", "twice", "twin",
    "twist", "two", "type", "typical",
    "ugly", "umbrella", "unable", "unaware",
    "uncle", "uncover", "under", "undo",
    "unfair", "unfold", "unhappy", "uniform",
    "unique", "unit", "universe", "unknown",
    "unlock", "until", "unusual", "unveil",
    "update", "upgrade", "uphold", "upon",
    "upper", "upset", "urban", "urge",
    "usage", "use", "used", "useful",
    "useless", "usual", "utility", "vacant",
    "vacuum", "vague", "valid", "valley",
    "valve", "van", "vanish", "vapor",
    "various", "vast", "vault", "vehicle",
    "velvet", "vendor", "venture", "venue",
    "verb", "verify", "version", "very",
    "vessel", "veteran", "viable", "vibrant",
    "vicious", "victory", "video", "view",
    "village", "vintage", "violin", "virtual",
    "virus", "visa", "visit", "visual",
    "vital", "vivid", "vocal", "voice",
    "void", "volcano", "volume", "vote",
    "voyage", "wage", "wagon", "wait",
    "walk", "wall", "walnut", "want",
    "warfare", "warm", "warrior", "wash",
    "wasp", "waste", "water", "wave",
    "way", "wealth", "weapon", "wear",
    "weasel", "weather", "web", "wedding",
    "weekend", "weird", "welcome", "west",
    "wet", "whale", "what", "wheat",
    "wheel", "when", "where", "whip",
    "whisper", "wide", "width", "wife",
    "wild", "will", "win", "window",
    "wine", "wing", "wink", "winner",
    "winter", "wire", "wisdom", "wise",
    "wish", "witness", "wolf", "woman",
    "wonder", "wood", "wool", "word",
    "work", "world", "worry", "worth",
    "wrap", "wreck", "wrestle", "wrist",
    "write", "wrong", "yard", "year",
    "yellow", "you", "young", "youth",
    "zebra", "zero", "zone", "zoo",
];


// ===========================================================================
// Tests. Hex expectations are pinned from the legacy client's compiled code
// (see the module provenance note) and cross-verified against an independent
// from-spec Python reference; BIP39 vectors are the official trezor
// vectors.json english set (seed column = passphrase "TREZOR").
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    const MN12: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const MN24: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }

    // ---- HMAC-SHA512 (RFC 4231) ----
    #[test]
    fn hmac_sha512_rfc4231_vectors() {
        let tc1 = HmacSha512::new(&[0x0b; 20]).compute(b"Hi There");
        assert_eq!(
            hex(&tc1),
            "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854"
        );
        let tc2 = HmacSha512::new(b"Jefe").compute(b"what do ya want for nothing?");
        assert_eq!(
            hex(&tc2),
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
        );
        // TC6: key longer than the 128-byte block must be hashed first.
        let tc6 = HmacSha512::new(&[0xaa; 131])
            .compute(b"Test Using Larger Than Block-Size Key - Hash Key First");
        assert_eq!(
            hex(&tc6),
            "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f3526b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598"
        );
    }

    // ---- BIP39: mnemonic generation + checksum ----
    #[test]
    fn wordlist_is_canonical() {
        assert_eq!(WORDLIST.len(), 2048);
        // lexicographically sorted (WordIndex's binary-search precondition)
        let mut sorted: Vec<&str> = WORDLIST.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted.as_slice(), WORDLIST);
        assert_eq!(word_index("abandon"), Some(0));
        assert_eq!(word_index("zoo"), Some(2047));
        assert_eq!(word_index("abitcoiny"), None);
        assert_eq!(word_index("ABANDON"), None); // case-sensitive, like legacy
        // every word distinct
        let set: std::collections::HashSet<&&str> = WORDLIST.iter().collect();
        assert_eq!(set.len(), 2048);
    }

    #[test]
    fn mnemonic_from_entropy_official_vectors() {
        // official 24-word vectors: entropy → mnemonic (checksum pinned)
        assert_eq!(
            mnemonic_from_entropy(&[0u8; 32]).unwrap(),
            MN24.trim_end()
        );
        let ffs = [0x7fu8; 32];
        assert_eq!(
            mnemonic_from_entropy(&ffs).unwrap(),
            "legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth title"
        );
        let eights = [0x80u8; 32];
        assert_eq!(
            mnemonic_from_entropy(&eights).unwrap(),
            "letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic bless"
        );
        let ffs16 = [0xffu8; 16];
        assert_eq!(
            mnemonic_from_entropy(&ffs16).unwrap(),
            "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong"
        );
        assert!(mnemonic_from_entropy(&[0u8; 8]).is_err());
    }

    #[test]
    fn generated_mnemonics_are_valid() {
        for _ in 0..100 {
            let m = generate_mnemonic_24().unwrap();
            let words = split_words(&m);
            assert_eq!(words.len(), 24);
            assert!(check_mnemonic(&m), "generated mnemonic failed checksum: {}", m);
            assert!(words.iter().all(|w| word_index(w).is_some()));
        }
        // distinct draws (256-bit entropy collision ~ impossible)
        let a = generate_mnemonic_24().unwrap();
        let b = generate_mnemonic_24().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn check_mnemonic_official_vectors_and_rejections() {
        // (entropy, mnemonic) pairs from the official english vectors —
        // every mnemonic must validate.
        let vectors = [
            ("00000000000000000000000000000000", "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"),
            ("7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f", "legal winner thank year wave sausage worth useful legal winner thank yellow"),
            ("80808080808080808080808080808080", "letter advice cage absurd amount doctor acoustic avoid letter advice cage above"),
            ("ffffffffffffffffffffffffffffffff", "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo wrong"),
            ("9e885d952ad362caeb4efe34a8e91bd2", "ozone drill grab fiber curtain grace pudding thank cruise elder eight picnic"),
            ("c0ba5a8e914111210f2bd131f3d5e08d", "scheme spot photo card baby mountain device kick cradle pact join borrow"),
            ("f30f8c1da665478f49b001d94c5fc452", "vessel ladder alter error federal sibling chat ability sun glass valve picture"),
            ("23db8160a31d3e0dca3688ed941adbf3", "cat swing flag economy stadium alone churn speed unique patch report train"),
            ("0000000000000000000000000000000000000000000000000000000000000000", MN24),
            ("7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f", "legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth title"),
            ("8080808080808080808080808080808080808080808080808080808080808080", "letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic bless"),
            ("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo vote"),
            ("68a79eaca2324873eacc50cb9c6eca8cc68ea5d936f98787c60c7ebc74e6ce7c", "hamster diagram private dutch cause delay private meat slide toddler razor book happy fancy gospel tennis maple dilemma loan word shrug inflict delay length"),
            ("9f6a2878b2520799a44ef18bc7df394e7061a224d2c33cd015b157d746869863", "panda eyebrow bullet gorilla call smoke muffin taste mesh discover soft ostrich alcohol speed nation flash devote level hobby quick inner drive ghost inside"),
            ("066dca1a2bb7e8a1db2832148ce9933eea0f3ac9548d793112d9a95c9407efad", "all hour make first leader extend hole alien behind guard gospel lava path output census museum junior mass reopen famous sing advance salt reform"),
            ("f585c11aec520db57dd353c69554b21a89b20fb0650966fa0a9d6f74fd989d8f", "void come effort suffer camp survey warrior heavy shoot primary clutch crush open amazing screen patrol group space point ten exist slush involve unfold"),
        ];
        for (ent, mn) in vectors {
            assert!(check_mnemonic(mn), "vector mnemonic rejected: {}", mn);
            // and regeneration from the entropy must reproduce it exactly
            let ent_bytes: Vec<u8> = (0..ent.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&ent[i..i + 2], 16).unwrap())
                .collect();
            assert_eq!(mnemonic_from_entropy(&ent_bytes).unwrap(), mn);
        }
    }

    #[test]
    fn check_mnemonic_rejects_garbage() {
        // corrupted checksum word (their bad_checksum test)
        assert!(!check_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon"
        ));
        // non-wordlist token
        assert!(!check_mnemonic(
            "zzzz not real bip39 words here at all foo bar baz qux"
        ));
        // wrong count
        assert!(!check_mnemonic("abandon abandon abandon"));
        // uppercase membership fails (case-sensitive, like legacy compare)
        assert!(!check_mnemonic(
            "ABANDON abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
        ));
        // checksum bits flipped: last word shifted by one index
        assert!(!check_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon actor"
        ));
    }

    // ---- BIP39: mnemonic → seed KATs (legacy compiled-code oracle) ----
    #[test]
    fn mnemonic_to_seed_legacy_oracle_vectors() {
        // their hd_wallet_tests.cpp pinned hex (12-word, "TREZOR")
        let s = mnemonic_to_seed(MN12, "TREZOR");
        assert_eq!(hex(&s), "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04");
        // their code on the official 24-word vector, no passphrase
        let s = mnemonic_to_seed(MN24, "");
        assert_eq!(hex(&s), "408b285c123836004f4b8842c89324c1f01382450c0d439af345ba7fc49acf705489c6fc77dbd4e3dc1dd8cc6bc9f043db8ada1e243c4a0eafb290d399480840");
        // official vector seed for the same phrase WITH "TREZOR" (vectors.json)
        let s = mnemonic_to_seed(MN24, "TREZOR");
        assert_eq!(hex(&s), "bda85446c68413707090a52022edd26a1c9462295029f2e60cd7c4f2bbd3097170af7a4d73245cafa9c3cca8d561a7c3de6f5d4a10be8ed2a5e608d68f92fcc8");
    }

    // ---- BIP32 KATs (test vector 1 + legacy oracle) ----
    #[test]
    fn bip32_vector1_master_and_children() {
        let seed: Vec<u8> = (0u8..16).collect();
        let m = master_from_seed(&seed).unwrap();
        assert_eq!(hex(&m.key), "e8f32e723decf4051aefac8e2c93c9c5b214313817cdb01a1494b917c8436b35");
        assert_eq!(hex(&m.chaincode), "873dff81c02f525623fd1fe5167eac3a55a049de3d314bb42ee227ffed37d508");

        let c0 = ckd_priv(&m, 0 | HARDENED).unwrap();
        assert_eq!(hex(&c0.key), "edb2e14f9ee77d26dd93b4ecede8d16ed408ce149b6cd80b0715a2d911a0afea");
        assert_eq!(hex(&c0.chaincode), "47fdacbd0f1097043b78c63c20c34ef4ed9a111d980047ad16282c7ae6236141");

        // full vector-1 chain — exercises HARDENED and NON-hardened steps
        // (m/0'/1/2'/2/1000000000); oracle-pinned from their compiled CKDpriv.
        let f = derive_path(&m, &[0 | HARDENED, 1, 2 | HARDENED, 2, 1_000_000_000]).unwrap();
        assert_eq!(hex(&f.key), "471b76e389e528d6de6d816857e012c5455051cad6660850e58372a6c3e6e7c8");
        assert_eq!(hex(&f.chaincode), "c783e67b921d2beb8f6b389cc646d7263b4145701dadd2161548a8b078e65e9e");
    }

    #[test]
    fn bip32_vector2_master() {
        // test vector 2: seed of 64 0xff bytes (official BIP32 spec; key and
        // chaincode cross-verified against the from-spec python reference)
        let seed = [0xffu8; 64];
        let m = master_from_seed(&seed).unwrap();
        assert_eq!(hex(&m.key), "8f2eb3f4358e84106dffe74e1b20700c24ae8f315b08c9666c3d6c8c90985a63");
        assert_eq!(hex(&m.chaincode), "dd719999824adea78f31482582bb85d132f6b33cd528576f631e549b872123fd");
    }

    // ---- DeriveTriangles: the interchangeability KATs ----
    #[test]
    fn derive_triangles_matches_legacy_oracle() {
        // m/44'/2222'/0'/0/i, 12-word phrase, no passphrase — hex printed by
        // the legacy hdwallet.cpp compiled into the oracle binary.
        let tri12 = [
            "0a3178756e79fefde873798a7f256b3a6c932f9123eea605dbbf5745c03a3637",
            "dda5a9f8e959559d07261c148c75cf2fd76be625e8404045bd9118d9b9bf4873",
            "65756e6e0e6e78123011e45780a01f046637c62bdfe7466702d0074c49357524",
            "5494215f80621f22009e7af83c177b51c9fa8f37af314c5235f778f65a6d2fd5",
            "a9c86fda465bc6b7559a08ff546d7afc7f79f271ea075e7b51b3b1f3b9eb8923",
            "096ca0305716d3988d139ee79707fd54918d365e2194854b4522adfe08a0ee93",
            "12e168250cebdd56ab37ea799b0f6a4822c0fe988274f6f2ef0395215d411541",
            "41ff3b300d834dfbb6362bf28c5e2ba661e3c0afc98024fef63e382b39b20639",
        ];
        for (i, want) in tri12.iter().enumerate() {
            let got = derive_triangles(MN12, "", 0, 0, i as u32).unwrap();
            assert_eq!(hex(&got), *want, "TRI12 index {}", i);
        }
        // passphrase changes the keys (their "25th word" semantics)
        let got = derive_triangles(MN12, "TREZOR", 0, 0, 0).unwrap();
        assert_eq!(hex(&got), "d1072445dc49e63805371dd7a736e16b07403c7812cfc607482c04f9b23c316a");

        let tri24 = [
            "c43da56b6d1485121a2e319632ba4f860c2a645d1d8cb4533ccd99e20442f0ea",
            "8597d6e372c3ec965f319c90ab0b01298f6964ec085da7818da218d75c230750",
            "410e52700f2284cc40d61ff15e6ab2b4364dbe4cb48f184c8cd65f752fdc4167",
            "e99f3a4a464ee0b304705a7603546c9b6d5ccf1737b50e1187dc7a3e1479c679",
            "276dae1efec36f00eb8d80fe1639911b29445ef435f9d025c62c2cc162b949bc",
            "924114566fe3a896bcb733923d0de02197d12eb2b7aa18495501df0e86ad6675",
            "409e8a94f6a92cab0a6c28c6cf239c569563f8029107101e4ef775e34fbe5726",
            "11b9c95efd1c7654b0dfb95e82f6e14153abaadb83ee674f6ab8e3c9ad0751bd",
        ];
        for (i, want) in tri24.iter().enumerate() {
            let got = derive_triangles(MN24, "", 0, 0, i as u32).unwrap();
            assert_eq!(hex(&got), *want, "TRI24 index {}", i);
        }
        let got = derive_triangles(MN24, "tri7", 0, 0, 3).unwrap();
        assert_eq!(hex(&got), "9e835876af45c5e8450e948cc82351f02587fc11cf98352c1ed6f13bedaaa835");
    }

    #[test]
    fn derivation_is_deterministic_and_index_sensitive() {
        let a = derive_triangles(MN24, "", 0, 0, 0).unwrap();
        let b = derive_triangles(MN24, "", 0, 0, 0).unwrap();
        let c = derive_triangles(MN24, "", 0, 0, 1).unwrap();
        let d = derive_triangles(MN24, "x", 0, 0, 0).unwrap();
        let e = derive_triangles(MN24, "", 1, 0, 0).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_ne!(a, e);
    }

    #[test]
    fn normalized_whitespace_derives_canonical_keys() {
        // The restore path canonicalizes whitespace — the canonical phrase
        // must produce the SAME keys however it was spaced when typed.
        let messy = "abandon\tabandon  abandon\nabandon abandon abandon abandon abandon abandon abandon abandon about";
        let words = split_words(messy);
        assert_eq!(words.len(), 12);
        let canonical = normalize_words(messy).unwrap();
        assert!(check_mnemonic(&canonical));
        assert_eq!(
            derive_triangles(&canonical, "", 0, 0, 0).unwrap(),
            derive_triangles(MN12, "", 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn raw_string_quirk_matches_legacy() {
        // DOCUMENTED LEGACY QUIRK, pinned: deriving from a RAW string with
        // embedded newlines reproduces the legacy wallet's (non-canonical)
        // keys byte-for-byte — tri7 does this only for mnemonics LOADED from
        // a wallet row (hdmnemonic), never for user-typed restores.
        let raw = "abandon\tabandon abandon\nabandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let got = derive_triangles(raw, "", 0, 0, 0).unwrap();
        assert_eq!(hex(&got), "0a25e1a1de7d396262cc5535c89b5256d0abcb38d965b7dc72e7d9d7bd256cb3");
    }

    #[test]
    fn secret_string_zeroizes_contents() {
        // hygiene: zeroize() must scrub the buffer in place (the legacy
        // CleanseWalletString analogue) while the string is still alive —
        // no freed-memory reads.
        let mut s = SecretString("seed words here".to_string());
        s.0.zeroize();
        assert!(s.0.bytes().all(|b| b == 0));
        // and a dropped SecretString must not panic (zeroize runs on drop)
        let s2 = SecretString("more words".to_string());
        drop(s2);
    }
}

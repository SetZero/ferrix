//! Checked against published and independently computed vectors.
//!
//! `BLAKE2b`'s come from Python's `hashlib.blake2b` on the host; the Argon2id
//! vectors are RFC 9106 §5.3's and, for the rest, the output of `RustCrypto`'s
//! `argon2` crate 0.5.3, run on the host by a scratch program that is not in
//! this tree.

extern crate std;

use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

use crate::blake2b::{Blake2b, digest};
use crate::phc::{Encoded, ParseError};
use crate::{Block, Error, Inputs, Params, equal, hash};

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| std::format!("{b:02x}")).collect()
}

fn pattern(len: usize, modulus: usize) -> Vec<u8> {
    (0..len).map(|i| (i % modulus) as u8).collect()
}

#[test]
fn blake2b_matches_hashlib() {
    let cases: [(Vec<u8>, usize, &str); 8] = [
        (
            Vec::new(),
            64,
            "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce",
        ),
        // RFC 7693 Appendix A.
        (
            b"abc".to_vec(),
            64,
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d17d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923",
        ),
        (
            b"abc".to_vec(),
            32,
            "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319",
        ),
        // Exactly one block, and one byte over: the last-block rule.
        (
            pattern(128, 256),
            64,
            "2319e3789c47e2daa5fe807f61bec2a1a6537fa03f19ff32e87eecbfd64b7e0e8ccff439ac333b040f19b0c4ddd11a61e24ac1fe0f10a039806c5dcc0da3d115",
        ),
        (
            pattern(129, 256),
            64,
            "f59711d44a031d5f97a9413c065d1e614c417ede998590325f49bad2fd444d3e4418be19aec4e11449ac1a57207898bc57d76a1bcf3566292c20c683a5c4648f",
        ),
        (
            pattern(1000, 251),
            48,
            "f0a7a4bb3c3290f432e513caa227ab3bf933c4c8c167193dff1cb10a0b992f042f5679e477f00c551e2cf2bec8101f1e",
        ),
        (vec![b'a'; 256], 1, "7e"),
        (
            b"The quick brown fox jumps over the lazy dog".to_vec(),
            20,
            "3c523ed102ab45a37d54f5610d5a983162fde84f",
        ),
    ];
    for (data, len, expected) in cases {
        let mut out = vec![0_u8; len];
        digest(&data, &mut out);
        assert_eq!(hex(&out), expected, "{} bytes into {len}", data.len());
        // The same bytes fed in uneven pieces.
        for piece in [1, 7, 64, 127, 128, 129] {
            let mut hasher = Blake2b::new(len);
            for chunk in data.chunks(piece) {
                hasher.update(chunk);
            }
            let mut split = vec![0_u8; len];
            hasher.finalize(&mut split);
            assert_eq!(
                hex(&split),
                expected,
                "{} bytes in pieces of {piece}",
                data.len()
            );
        }
    }
}

/// One Argon2id vector: costs, inputs, and the tag in hex.
struct Vector {
    params: Params,
    password: Vec<u8>,
    salt: Vec<u8>,
    secret: Vec<u8>,
    associated: Vec<u8>,
    tag: &'static str,
}

fn run(vector: &Vector) -> Vec<u8> {
    let mut memory = vec![Block::ZERO; vector.params.blocks().unwrap()];
    let mut out = vec![0_u8; vector.tag.len() / 2];
    hash(
        &vector.params,
        &Inputs {
            password: &vector.password,
            salt: &vector.salt,
            secret: &vector.secret,
            associated: &vector.associated,
        },
        &mut memory,
        &mut out,
    )
    .unwrap();
    assert!(
        memory.iter().all(|block| block.0.iter().all(|&w| w == 0)),
        "the memory was left holding what the password made"
    );
    out
}

fn vector(
    memory_kib: u32,
    passes: u32,
    lanes: u32,
    password: &[u8],
    salt: &[u8],
    tag: &'static str,
) -> Vector {
    Vector {
        params: Params {
            memory_kib,
            passes,
            lanes,
        },
        password: password.to_vec(),
        salt: salt.to_vec(),
        secret: Vec::new(),
        associated: Vec::new(),
        tag,
    }
}

#[test]
fn argon2id_matches_rfc_9106() {
    // RFC 9106 §5.3.
    let rfc = Vector {
        params: Params {
            memory_kib: 32,
            passes: 3,
            lanes: 4,
        },
        password: vec![1; 32],
        salt: vec![2; 16],
        secret: vec![3; 8],
        associated: vec![4; 12],
        tag: "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659",
    };
    assert_eq!(hex(&run(&rfc)), rfc.tag);
}

#[test]
fn argon2id_matches_rustcrypto() {
    let vectors = [
        // The least memory one lane allows, one pass.
        vector(
            8,
            1,
            1,
            b"password",
            b"somesalt",
            "f137f8e186a403a679ccd0606e5ab5dcdafe43c1640855ac8c6e33e9bd63eeb3",
        ),
        // Two passes: the XOR into the old block.
        vector(
            64,
            2,
            1,
            b"password",
            b"somesalt",
            "16a1a498734609dd01456da406de9f3d9da93e6c86c300a12fc1465214ce4922",
        ),
        // An empty password, two lanes, a 64-byte tag.
        vector(
            256,
            3,
            2,
            b"",
            b"saltsaltsaltsalt",
            "8d6e44f253ccbc849df5b58f91746764a052c042f30cc0a903376a2ae818b05177afe9a6b85e8216120d058f8b3c8ad4f9e1d0b8e31b9ff4658f494c8b57059e",
        ),
        // A tag past 64 bytes: H' chains digests.
        vector(
            1024,
            1,
            1,
            b"correct horse battery staple",
            b"0123456789abcdef",
            "af0fd6507bb56727b307c1ec474d0360cdb07f0918ab309bc3e7004f1eb7b1610dccd84862608425b1da12f490033fa4778b97ea4e9050e15d21d0c28515b22c213ed04ad5ecad56264251586e819a719579d3e400ba7c4fe3157e39ffefbae7f2658c19",
        ),
        // Memory that is not a multiple of four blocks a lane, three lanes,
        // and a tag of odd length.
        vector(
            33,
            2,
            3,
            b"odd memory",
            b"saltysaltysalty!",
            "35688c33a2956a53aecd8779030af2614d",
        ),
    ];
    for v in &vectors {
        // Miri interprets every word of every block: the vectors past 64 KiB
        // would take it most of an hour, and the ones left take every path
        // the others do.
        if cfg!(miri) && v.params.memory_kib > 64 {
            continue;
        }
        assert_eq!(hex(&run(v)), v.tag, "{:?}", v.params);
    }
}

/// The floor `authd` never goes below (`docs/AUTH.md` §5.1): 19 MiB, two
/// passes, one lane. Slow under Miri, so left to the native run.
#[test]
#[cfg_attr(miri, ignore)]
fn argon2id_at_the_floor() {
    let floor = vector(
        19_456,
        2,
        1,
        b"ferrix",
        b"0123456789abcdef",
        "99261f0377abb9fc3f51662b61b141016639a1ac1acb124b21e2a57706cd506f",
    );
    assert_eq!(hex(&run(&floor)), floor.tag);
}

#[test]
fn refusals() {
    let params = Params {
        memory_kib: 8,
        passes: 1,
        lanes: 1,
    };
    let inputs = Inputs {
        password: b"pw",
        salt: b"somesalt",
        secret: &[],
        associated: &[],
    };
    let mut memory = vec![Block::ZERO; 8];
    let mut out = [0_u8; 32];
    let bad = |memory_kib, passes, lanes| Params {
        memory_kib,
        passes,
        lanes,
    };
    for p in [
        bad(8, 0, 1),
        bad(8, 1, 0),
        bad(7, 1, 1),
        bad(15, 1, 2),
        bad(64, 1, 0x0100_0000),
    ] {
        assert_eq!(
            hash(&p, &inputs, &mut memory, &mut out),
            Err(Error::Params),
            "{p:?}"
        );
    }
    let short = Inputs {
        salt: b"7 bytes",
        ..inputs
    };
    assert_eq!(
        hash(&params, &short, &mut memory, &mut out),
        Err(Error::Salt)
    );
    assert_eq!(
        hash(&params, &inputs, &mut memory, &mut [0; 3]),
        Err(Error::Output)
    );
    assert_eq!(
        hash(&params, &inputs, &mut memory[..7], &mut out),
        Err(Error::Memory)
    );
    assert_eq!(out, [0; 32], "a refused hash wrote nothing");
}

#[test]
fn blocks_round_down_to_four_a_lane() {
    let blocks = |memory_kib, lanes| {
        Params {
            memory_kib,
            passes: 1,
            lanes,
        }
        .blocks()
        .unwrap()
    };
    assert_eq!(blocks(8, 1), 8);
    assert_eq!(blocks(11, 1), 8);
    assert_eq!(blocks(33, 3), 24);
    assert_eq!(blocks(65_536, 4), 65_536);
}

#[test]
fn the_same_inputs_hash_the_same_and_one_byte_changes_it() {
    let params = Params {
        memory_kib: 16,
        passes: 2,
        lanes: 2,
    };
    let mut memory = vec![Block::ZERO; 16];
    let mut one = [0_u8; 32];
    let mut two = [0_u8; 32];
    let mut three = [0_u8; 32];
    let inputs = Inputs {
        password: b"hunter2",
        salt: b"saltsalt",
        secret: &[],
        associated: &[],
    };
    hash(&params, &inputs, &mut memory, &mut one).unwrap();
    hash(&params, &inputs, &mut memory, &mut two).unwrap();
    let changed = Inputs {
        password: b"hunter3",
        ..inputs
    };
    hash(&params, &changed, &mut memory, &mut three).unwrap();
    assert_eq!(one, two);
    assert_ne!(one, three);
}

#[test]
fn phc_round_trips_and_verifies() {
    let params = Params {
        memory_kib: 64,
        passes: 2,
        lanes: 1,
    };
    let salt = b"somesalt";
    let tag = unhex("16a1a498734609dd01456da406de9f3d9da93e6c86c300a12fc1465214ce4922");
    let encoded = Encoded::new(params, salt, &tag).unwrap();
    let text = encoded.to_string();
    // What `RustCrypto`'s `password-hash` writes for the same hash.
    assert_eq!(
        text,
        "$argon2id$v=19$m=64,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI"
    );
    let back = Encoded::parse(&text).unwrap();
    assert_eq!(back, encoded);
    let mut memory = vec![Block::ZERO; 64];
    assert_eq!(back.verify(b"password", &mut memory), Ok(true));
    assert_eq!(back.verify(b"passwore", &mut memory), Ok(false));
    assert_eq!(back.verify(b"", &mut memory), Ok(false));
    assert_eq!(
        back.verify(b"password", &mut memory[..10]),
        Err(Error::Memory)
    );
}

#[test]
fn phc_refuses_what_it_did_not_write() {
    let good =
        "$argon2id$v=19$m=64,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI";
    assert!(Encoded::parse(good).is_ok());
    let cases: [(&str, ParseError); 14] = [
        (
            "argon2id$v=19$m=64,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Shape,
        ),
        (
            "$argon2i$v=19$m=64,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Algorithm,
        ),
        ("$6$saltsalt$abc", ParseError::Algorithm),
        (
            "$argon2id$v=16$m=64,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Version,
        ),
        (
            "$argon2id$m=64,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Version,
        ),
        (
            "$argon2id$v=19$t=2,m=64,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Params,
        ),
        (
            "$argon2id$v=19$m=64,t=2,p=1,x=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Params,
        ),
        (
            "$argon2id$v=19$m=064,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Params,
        ),
        (
            "$argon2id$v=19$m=4,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Params,
        ),
        (
            "$argon2id$v=19$m=64,t=0,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Params,
        ),
        // Padding, a character outside the alphabet, and bits left over.
        (
            "$argon2id$v=19$m=64,t=2,p=1$c29tZXNhbHQ=$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Field,
        ),
        (
            "$argon2id$v=19$m=64,t=2,p=1$c29tZXNhbH-$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Field,
        ),
        (
            "$argon2id$v=19$m=64,t=2,p=1$c29tZXNhbHR$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI",
            ParseError::Field,
        ),
        (
            "$argon2id$v=19$m=64,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI$",
            ParseError::Shape,
        ),
    ];
    for (text, why) in cases {
        assert_eq!(Encoded::parse(text), Err(why), "{text}");
    }
    // A salt too short and a tag too short are refused as fields.
    assert_eq!(
        Encoded::parse(
            "$argon2id$v=19$m=64,t=2,p=1$c2FsdA$FqGkmHNGCd0BRW2kBt6fPZ2pPmyGwwChL8FGUhTOSSI"
        ),
        Err(ParseError::Field)
    );
    assert_eq!(
        Encoded::parse("$argon2id$v=19$m=64,t=2,p=1$c29tZXNhbHQ$FqGkmHNGCd0"),
        Err(ParseError::Field)
    );
}

#[test]
fn debug_shows_no_secret() {
    let encoded = Encoded::new(
        Params {
            memory_kib: 64,
            passes: 2,
            lanes: 1,
        },
        b"somesalt",
        &[0xAB; 32],
    )
    .unwrap();
    let shown = std::format!("{encoded:?} {:?}", Block::ZERO);
    assert!(
        !shown.contains("171") && !shown.contains("ab") && !shown.contains("c29t"),
        "{shown}"
    );
}

#[test]
fn equal_compares_whole_slices() {
    assert!(equal(b"abc", b"abc"));
    assert!(!equal(b"abc", b"abd"));
    assert!(!equal(b"abc", b"ab"));
    assert!(equal(b"", b""));
}

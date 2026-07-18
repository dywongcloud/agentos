//! Short authentication-string (SAS) pairing phrase, derived deterministically from the iroh
//! ticket so the Mac and the iOS app display the SAME human-readable phrase and the user can
//! confirm they match -- defeating a QR-substitution MITM (a swapped ticket yields a different
//! phrase). This is the daemon half of the cross-platform contract specified in
//! `holoiroh/ios/PAIRING_PHRASE.md`; the iOS half (`PairingPhrase.swift` / `PairingWordlist.swift`)
//! must derive byte-identical output. See that spec for the full rationale and known-answer
//! vectors.
//!
//! Algorithm (version 1): `SHA-256(utf8 bytes of the ticket string)` -> take the first `N`
//! digest bytes (default 4) -> index each into a fixed **256-word** list (one word per possible
//! byte value, so no modulo bias) -> join with spaces. 4 words out of 256 = 256^4 ~= 2^32
//! possible phrases, far beyond an interactive attacker's reach against one live pairing attempt.
//!
//! The wordlist below is generated from (and must stay identical to) `PairingWordlist.swift`'s
//! `WORDLIST` -- both are "version 1" of the shared contract; changing either without the other
//! silently breaks pairing verification.

use sha2::{Digest, Sha256};

/// Default number of words in a pairing phrase. Matches `PairingPhrase.swift`'s default.
pub const PHRASE_WORD_COUNT: usize = 4;

/// The 256-word SAS wordlist (index == digest byte value). MUST match
/// `holoiroh/ios/Sources/HoloIrohApp/PairingWordlist.swift`'s `WORDLIST` exactly, in order.
pub const WORDLIST: [&str; 256] = [
    "acid", "alarm", "album", "anchor", "apple", "april", "arena", "atlas",
    "aztec", "bacon", "badge", "baker", "banjo", "basil", "beach", "bench",
    "berry", "bison", "black", "blend", "blimp", "block", "bloom", "board",
    "bonus", "boost", "brave", "bread", "brick", "brisk", "broom", "brush",
    "bugle", "cabin", "cable", "cactus", "camel", "candy", "canoe", "canyon",
    "cargo", "carol", "cedar", "chalk", "charm", "chess", "chief", "chime",
    "cider", "cigar", "civic", "clamp", "clash", "clay", "clerk", "cliff",
    "cloak", "clock", "clove", "clown", "cobra", "cocoa", "comet", "coral",
    "couch", "cover", "coyote", "crane", "crate", "crisp", "crown", "crumb",
    "crust", "curry", "dance", "dandy", "daisy", "delta", "denim", "depot",
    "diner", "ditch", "diver", "dodge", "donor", "dough", "draft", "drama",
    "dress", "drift", "drone", "eagle", "ember", "envoy", "epoch", "extra",
    "fable", "fancy", "feast", "fence", "ferry", "fiber", "field", "finch",
    "flame", "flare", "flask", "fleet", "flint", "float", "flock", "flute",
    "focus", "forge", "fossil", "frost", "fudge", "gecko", "genie", "ghost",
    "giant", "glass", "glide", "globe", "glove", "grape", "grill", "grove",
    "guide", "gumbo", "gusto", "harbor", "hazel", "heron", "hobby", "honey",
    "hotel", "hound", "ivory", "jazz", "jelly", "jetty", "jewel", "jolly",
    "juice", "jumbo", "kayak", "kettle", "koala", "label", "lager", "lasso",
    "latch", "ledge", "lemon", "lever", "lilac", "linen", "llama", "locket",
    "lodge", "lotus", "lunar", "lyric", "macro", "mango", "maple", "marble",
    "medal", "melon", "mercy", "metro", "mimic", "miner", "mocha", "motto",
    "mound", "mural", "nacho", "nectar", "niece", "ninja", "noble", "nomad",
    "notch", "novel", "oasis", "ocean", "olive", "onion", "opera", "orbit",
    "otter", "owlet", "oxide", "paddle", "panda", "panel", "pansy", "parka",
    "pasta", "patio", "pearl", "pecan", "penny", "perch", "piano", "pilot",
    "pixel", "pizza", "plaza", "plume", "polar", "pouch", "prism", "prize",
    "proud", "puma", "punch", "quail", "quartz", "quest", "quill", "quilt",
    "radar", "raft", "raven", "razor", "relic", "rhino", "ridge", "rival",
    "robin", "rodeo", "rugby", "ruler", "salsa", "sauna", "scarf", "scout",
    "sedan", "sheep", "shell", "shrub", "siren", "sloth", "solar", "spice",
    "spine", "spoon", "sprout", "squid", "stork", "sugar", "syrup", "tulip",
];

/// Derive the pairing phrase for `ticket` (default `PHRASE_WORD_COUNT` words). `ticket` is
/// hashed as-is: the daemon passes `LiveTicket::to_string()`, which has no surrounding
/// whitespace, so it equals the iOS side's whitespace-trimmed canonical form (see the spec).
pub fn pairing_phrase(ticket: &str) -> String {
    pairing_phrase_n(ticket, PHRASE_WORD_COUNT)
}

/// Same as [`pairing_phrase`] but with an explicit word count (`n` clamped to 1..=32 -- a
/// SHA-256 digest is 32 bytes, so at most 32 words can be derived without rehashing).
pub fn pairing_phrase_n(ticket: &str, n: usize) -> String {
    let n = n.clamp(1, 32);
    let digest = Sha256::digest(ticket.as_bytes());
    digest
        .iter()
        .take(n)
        .map(|&b| WORDLIST[b as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

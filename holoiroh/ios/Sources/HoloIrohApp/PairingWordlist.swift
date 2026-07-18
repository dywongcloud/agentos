import Foundation

/// The fixed 256-word list used to render a pairing verification phrase
/// (a short-authentication-string / SAS) from a hash of the iroh ticket.
///
/// ## This list is a shared-secret *contract* with the Mac daemon
///
/// The whole point of the verification phrase (Project Aro PRD P0-2 / 7.1)
/// is that the *same* phrase appears on both the Mac and the iPhone, so a
/// man-in-the-middle who substituted the QR/ticket is caught when the two
/// phrases fail to match. That only works if **both ends derive the phrase
/// from the identical wordlist, in the identical order, with the identical
/// indexing rule** (see `PairingPhrase.swift` for the derivation).
///
/// Therefore:
/// - The list has **exactly 256 entries** (`2^8`), so a single digest byte
///   (`0...255`) maps to exactly one word with no modulo bias and no
///   bit-slicing ambiguity: `word = wordlist[digestByte]`.
/// - The **order is load-bearing**. Index `i` must map to the same word on
///   the daemon side. Reordering, inserting, or removing any word is a
///   **breaking change** to the pairing contract and must be accompanied by
///   a bump of `PairingPhrase.algorithmVersion` (and a matching bump on the
///   daemon).
/// - Words are chosen to be short, lowercase, ASCII, distinct, and
///   low-homophone so they are unambiguous when one person reads the phrase
///   aloud and the other confirms it. (Classic SAS-wordlist properties; a
///   compact hand-curated list rather than the full 2048-word BIP-39 list,
///   because 4 words out of 256 already gives 2^32 ≈ 4.3 billion possible
///   phrases, far beyond what an interactive attacker can grind against a
///   single live pairing attempt.)
///
/// The daemon must embed a byte-for-byte identical list. `PAIRING_PHRASE.md`
/// (in `holoiroh/ios/`) is the authoritative spec the daemon side follows,
/// and includes a known-answer test vector so agreement can be proven.
enum PairingWordlist {
    /// Version of *this specific list's contents+ordering*. Bumped together
    /// with `PairingPhrase.algorithmVersion` whenever the list changes, so a
    /// mismatched daemon/app pair can detect that they disagree rather than
    /// silently show phrases that never match.
    static let version = 1

    /// Exactly 256 words. Index == digest byte value.
    static let words: [String] = [
        // 0x00 - 0x0F
        "acid", "alarm", "album", "anchor", "apple", "april", "arena", "atlas",
        "aztec", "bacon", "badge", "baker", "banjo", "basil", "beach", "bench",
        // 0x10 - 0x1F
        "berry", "bison", "black", "blend", "blimp", "block", "bloom", "board",
        "bonus", "boost", "brave", "bread", "brick", "brisk", "broom", "brush",
        // 0x20 - 0x2F
        "bugle", "cabin", "cable", "cactus", "camel", "candy", "canoe", "canyon",
        "cargo", "carol", "cedar", "chalk", "charm", "chess", "chief", "chime",
        // 0x30 - 0x3F
        "cider", "cigar", "civic", "clamp", "clash", "clay", "clerk", "cliff",
        "cloak", "clock", "clove", "clown", "cobra", "cocoa", "comet", "coral",
        // 0x40 - 0x4F
        "couch", "cover", "coyote", "crane", "crate", "crisp", "crown", "crumb",
        "crust", "curry", "dance", "dandy", "daisy", "delta", "denim", "depot",
        // 0x50 - 0x5F
        "diner", "ditch", "diver", "dodge", "donor", "dough", "draft", "drama",
        "dress", "drift", "drone", "eagle", "ember", "envoy", "epoch", "extra",
        // 0x60 - 0x6F
        "fable", "fancy", "feast", "fence", "ferry", "fiber", "field", "finch",
        "flame", "flare", "flask", "fleet", "flint", "float", "flock", "flute",
        // 0x70 - 0x7F
        "focus", "forge", "fossil", "frost", "fudge", "gecko", "genie", "ghost",
        "giant", "glass", "glide", "globe", "glove", "grape", "grill", "grove",
        // 0x80 - 0x8F
        "guide", "gumbo", "gusto", "harbor", "hazel", "heron", "hobby", "honey",
        "hotel", "hound", "ivory", "jazz", "jelly", "jetty", "jewel", "jolly",
        // 0x90 - 0x9F
        "juice", "jumbo", "kayak", "kettle", "koala", "label", "lager", "lasso",
        "latch", "ledge", "lemon", "lever", "lilac", "linen", "llama", "locket",
        // 0xA0 - 0xAF
        "lodge", "lotus", "lunar", "lyric", "macro", "mango", "maple", "marble",
        "medal", "melon", "mercy", "metro", "mimic", "miner", "mocha", "motto",
        // 0xB0 - 0xBF
        "mound", "mural", "nacho", "nectar", "niece", "ninja", "noble", "nomad",
        "notch", "novel", "oasis", "ocean", "olive", "onion", "opera", "orbit",
        // 0xC0 - 0xCF
        "otter", "owlet", "oxide", "paddle", "panda", "panel", "pansy", "parka",
        "pasta", "patio", "pearl", "pecan", "penny", "perch", "piano", "pilot",
        // 0xD0 - 0xDF
        "pixel", "pizza", "plaza", "plume", "polar", "pouch", "prism", "prize",
        "proud", "puma", "punch", "quail", "quartz", "quest", "quill", "quilt",
        // 0xE0 - 0xEF
        "radar", "raft", "raven", "razor", "relic", "rhino", "ridge", "rival",
        "robin", "rodeo", "rugby", "ruler", "salsa", "sauna", "scarf", "scout",
        // 0xF0 - 0xFF
        "sedan", "sheep", "shell", "shrub", "siren", "sloth", "solar", "spice",
        "spine", "spoon", "sprout", "squid", "stork", "sugar", "syrup", "tulip",
    ]
}

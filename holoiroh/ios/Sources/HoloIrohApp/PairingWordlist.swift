import Foundation

/// Defines the fixed word list for the ticket verification phrase.
///
/// The app and daemon use the same list and index rule.
///
/// This list has these constraints:
///
/// - It contains exactly 256 entries.
/// - Each digest byte maps directly to one list index.
/// - Each word uses lowercase American Standard Code for Information Interchange (ASCII) characters.
/// - Each word is distinct and has few homophones.
///
/// The word order is part of the pairing contract.
/// Reordering, adding, or removing a word is a breaking change.
/// Update both implementations and their algorithm versions together.
/// See `PAIRING_PHRASE.md` for the contract and known-answer vectors.
enum PairingWordlist {
    /// Identifies this exact list and word order.
    /// Update this value with `PairingPhrase.algorithmVersion` when the list changes.
    static let version = 1

    /// Contains 256 words indexed by digest byte value.
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

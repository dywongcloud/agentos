import Foundation

// A PERMANENT conformance check for `ConnectionProfileRepository` /
// `ConnectionProfileStore` -- deliberately a standalone verification executable
// (compiled + run in CI), NOT an XCTest/jest-style suite. It runs the REAL store
// against temp databases and asserts the always-present-default invariants that
// took a hard, device-confirmed fix to get right; a regression exits non-zero
// and fails CI. Run via ./run.sh (which compiles the real sources alongside it).

var failures = 0
func check(_ cond: Bool, _ msg: String) {
    if cond { print("  PASS: \(msg)") } else { print("  FAIL: \(msg)"); failures += 1 }
}

@MainActor func devMacCount(_ s: ConnectionProfileStore) -> Int {
    s.profiles.filter { $0.name == "Dev Mac" }.count
}
@MainActor func devMac(_ s: ConnectionProfileStore) -> ConnectionProfile? {
    s.profiles.first(where: { $0.name == "Dev Mac" })
}

@MainActor func run() {
    let dir = NSTemporaryDirectory()
    func tmp(_ n: String) -> URL {
        let u = URL(fileURLWithPath: dir).appendingPathComponent(n)
        try? FileManager.default.removeItem(at: u)
        return u
    }

    print("[1] fresh empty DB -> exactly one Dev Mac default, pin 394299, first in list")
    let a = tmp("conf-a.sqlite")
    let s1 = ConnectionProfileStore(databaseURL: a)
    check(devMacCount(s1) == 1, "fresh: exactly one Dev Mac")
    check(s1.profiles.first?.name == "Dev Mac", "fresh: Dev Mac is first")
    check(devMac(s1)?.pin == "394299", "fresh: pin is 394299")

    print("[2] reopen the same DB -> still exactly one Dev Mac (no duplicate)")
    let s2 = ConnectionProfileStore(databaseURL: a)
    check(devMacCount(s2) == 1, "reopen: exactly one Dev Mac")

    print("[3] user deletes the default, reopen -> it re-appears (never permanently lost)")
    if let dev = devMac(s2) { s2.delete(dev) }
    let s3 = ConnectionProfileStore(databaseURL: a)
    check(devMacCount(s3) == 1, "delete+reopen: Dev Mac re-synthesized")

    print("[4] a STALE stored 'Dev Mac' row -> filtered; the default is the current constant")
    let b = tmp("conf-b.sqlite")
    let seed = ConnectionProfileStore(databaseURL: b)
    if let dev = devMac(seed) { seed.delete(dev) }
    _ = seed.save(name: "Dev Mac",
                  ticket: "iroh-live:STALEoUavJaTyFA2AXzWPTiUUg38hFs6cOjKHKJu9stale/holoiroh",
                  pin: "000000")
    let s4 = ConnectionProfileStore(databaseURL: b)
    check(devMacCount(s4) == 1, "stale+reopen: exactly one Dev Mac")
    check(devMac(s4)?.pin == "394299", "stale+reopen: default is the current constant (pin 394299)")

    print("[5] BROKEN sqlite (db == nil) -> the default is STILL present (the device failure mode)")
    let bad = URL(fileURLWithPath: "/dev/null/cannot-open.sqlite")
    let s5 = ConnectionProfileStore(databaseURL: bad)
    check(devMacCount(s5) == 1, "broken-db: Dev Mac still present in-memory")
    check(devMac(s5)?.pin == "394299", "broken-db: pin is 394299")

    print("")
    if failures == 0 {
        print("CONFORMANCE OK: all ConnectionProfileRepository invariants passed")
        exit(0)
    } else {
        print("CONFORMANCE FAILED: \(failures) invariant(s) broken")
        exit(1)
    }
}

DispatchQueue.main.async { run() }
dispatchMain()

struct Report: Hashable {
    let id: Int
    // The sole in-repo `hash(into:)` — a Hashable protocol requirement whose
    // canonical definition is the compiler-synthesized one, not this override.
    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
    }
}

struct Widget {
    let value: Int
}

// `w` is a `Widget`, which has no `hash(into:)` of its own. The repo-wide
// single-definition fallback must NOT capture this call for `Report.hash`.
func logHash(_ w: Widget) {
    var hasher = Hasher()
    w.hash(into: &hasher)
}

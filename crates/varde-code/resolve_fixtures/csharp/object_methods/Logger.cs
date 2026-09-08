public class Logger
{
    public Logger() {}

    // `item` is a plain object: neither the type-directed pass (unknown type)
    // nor the repo-wide fallback should resolve `item.ToString()` to the sole
    // in-repo override (`Report.ToString`) — that is a false edge.
    public void Log(object item)
    {
        var text = item.ToString();
    }
}

namespace App;

public class Service
{
    public void Handle()
    {
        // `Compute` is defined exactly once in *production* (Helper.cs), so the
        // repo-wide single-definition fallback must resolve this call.
        Compute();
        // `RunOnce` is defined only in a *test double* (App.Tests/FakeService.cs).
        // Excluding test-file defs from the fallback index means this stays
        // unresolved instead of binding a production caller to test code.
        RunOnce();
    }
}

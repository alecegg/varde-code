namespace App.Tests;

// A test double living under the `.Tests/` project directory (matched by
// `db::path_is_test`). It is the *only* definition of `RunOnce` in the repo,
// so before the test-def exclusion the repo-wide fallback bound every
// production `RunOnce()` caller to this fake.
public class FakeService
{
    public void RunOnce()
    {
    }
}

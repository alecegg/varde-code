namespace App;

public class Service
{
    // `ExternalBase` is not defined in the repo — it stands in for a framework
    // base class (`TimeProvider`, `DbContext`, ...). Its only in-repo subtype
    // is a unit-test fake, so the type-directed pass would otherwise bind this
    // production call to test code.
    private readonly ExternalBase _dep;

    public Service(ExternalBase dep)
    {
        _dep = dep;
    }

    public void Run()
    {
        _dep.Fetch();
    }
}

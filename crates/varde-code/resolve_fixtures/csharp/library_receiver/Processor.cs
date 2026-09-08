namespace App;

public class Processor
{
    // `Catalog` is an in-repo type, so this receiver is NOT external. The
    // type-directed pass resolves the call to the matching in-repo method —
    // the veto must not touch it.
    private readonly Catalog _catalog;

    public Processor(Catalog catalog)
    {
        _catalog = catalog;
    }

    public void Go()
    {
        _catalog.FindOne();
    }
}

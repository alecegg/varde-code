namespace App;

public class Catalog
{
    // The sole in-repo definition of `FindOne`. It is the *real* target only
    // for a receiver typed as `Catalog` — not for a call on a library
    // repository that happens to share the method name.
    public void FindOne()
    {
    }
}

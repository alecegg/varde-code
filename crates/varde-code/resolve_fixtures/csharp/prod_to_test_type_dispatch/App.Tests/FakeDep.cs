namespace App.Tests;

// A unit-test fake extending the external base. It is the sole in-repo
// definition of `Fetch`, and the type-directed pass reaches it as a subtype of
// `ExternalBase`. The production caller in Service.cs must NOT resolve here —
// the production→test guard drops the edge.
public class FakeDep : ExternalBase
{
    public void Fetch()
    {
    }
}
